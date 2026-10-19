//! One message, end to end.

use crate::budget::{BreakerEvent, Budget, Denied, Priority};
use crate::cache::TtlCache;
use crate::config::{Config, ContextFeed, GuildConfig};
use crate::fusion::{fuse, Inputs};
use crate::guarded::{guarded, GuardErr, Guarded};
use crate::imaging;
use crate::model::*;
use crate::normalize::{contains_url, hash_bytes, hash_text, normalize};
use crate::policy::{decide, flag_threshold};
use crate::ports::*;
use crate::providers::jev::JevState;
use crate::risk::{compute_tier, detection_plan, graduated, ocr_eligible, Plan, TierHints};
use crate::rules::{evaluate, image_text_signals, MessageFacts, RecentMsg, RecentTracker};
use crate::store::{NewIncident, Store};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub guild_id: u64,
    /// Channel or thread ID; also Paxmod's `context_id`.
    pub channel_id: u64,
    pub message_id: u64,
    pub author_id: u64,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub image_urls: Vec<String>,
    pub distinct_mentions: usize,
    pub mentions_everyone_text: bool,
    pub can_mention_everyone: bool,
    pub is_edit: bool,
    pub profile: MemberProfile,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub tier: Tier,
    pub plan: Plan,
    pub verdict: Option<Verdict>,
    pub planned: Action,
    /// none | flag | would-have | delete | delete+timeout | kick | ban | refused | failed | already
    pub taken: String,
    pub incident_id: Option<i64>,
    pub swept: usize,
    pub ocr_text: Option<String>,
}

impl Outcome {
    fn skipped(tier: Tier) -> Self {
        Outcome { tier, plan: Plan::NONE, verdict: None, planned: Action::None, taken: "none".into(), incident_id: None, swept: 0, ocr_text: None }
    }
}

pub struct Budgets {
    pub paxmod: Arc<Budget>,
    pub jev: Arc<Budget>,
    pub ocr: Arc<Budget>,
}

pub struct Deps {
    pub cfg: Arc<Config>,
    pub store: Store,
    pub paxmod: Arc<dyn TextModerator>,
    pub jev: Arc<dyn Classifier>,
    pub ocr: Arc<dyn TextReader>,
    pub fetcher: Arc<dyn ImageFetcher>,
    pub sink: Arc<dyn ActionSink>,
    pub modlog: Arc<dyn ModLog>,
    pub budgets: Budgets,
    pub ocr_max_upload: usize,
    /// Uniform sample in [0,1) for sampling decisions.
    pub roll: Arc<dyn Fn() -> f64 + Send + Sync>,
}

/// Which channels a Quarantine or Fresh member spoke in recently.
#[derive(Default)]
struct Activity(Mutex<HashMap<u64, DateTime<Utc>>>);

impl Activity {
    fn note(&self, ch: u64, now: DateTime<Utc>) {
        let mut m = self.0.lock().unwrap();
        m.insert(ch, now);
        if m.len() > 4096 {
            m.retain(|_, t| now - *t < Duration::hours(1));
        }
    }
    fn active(&self, ch: u64, now: DateTime<Utc>, window: Duration) -> bool {
        self.0.lock().unwrap().get(&ch).is_some_and(|t| now - *t <= window)
    }
}

pub struct Pipeline {
    d: Deps,
    tracker: RecentTracker,
    activity: Activity,
    jev_cache: TtlCache<String, JevResult>,
    ocr_cache: TtlCache<String, String>,
    /// Per-guild effective config (static + DB override); short TTL, invalidated on write.
    cfg_cache: TtlCache<u64, Arc<GuildConfig>>,
    announced: Mutex<HashSet<(&'static str, &'static str, NaiveDate)>>,
    feed_tasks: Mutex<Vec<JoinHandle<()>>>,
}

fn trunc(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

impl Pipeline {
    pub fn new(d: Deps) -> Self {
        let jev_ttl = std::time::Duration::from_secs(d.cfg.runtime.jev_cache_ttl_secs);
        let ocr_ttl = std::time::Duration::from_secs(d.cfg.ocr.cache_ttl_secs);
        Pipeline {
            d,
            tracker: RecentTracker::default(),
            activity: Activity::default(),
            jev_cache: TtlCache::new(jev_ttl, 4096),
            ocr_cache: TtlCache::new(ocr_ttl, 4096),
            cfg_cache: TtlCache::new(std::time::Duration::from_secs(30), 256),
            announced: Mutex::new(HashSet::new()),
            feed_tasks: Mutex::new(vec![]),
        }
    }

    pub fn store(&self) -> &Store {
        &self.d.store
    }

    /// Expose the sink's hierarchy/owner check for the manual mod-log buttons.
    pub async fn check_can_act(&self, guild: u64, user: u64) -> Result<(), String> {
        self.d.sink.can_act(guild, user).await
    }

    /// Post a notice to the guild's mod log (audit trace for manual actions).
    pub async fn log_notice(&self, guild: u64, text: &str) {
        self.d.modlog.notice(guild, text).await;
    }

    /// Mod-log destination for a guild: the DB override (set via `/guard logchannel`)
    /// wins over the config.toml default.
    pub async fn effective_log_channel(&self, guild: u64) -> Option<u64> {
        self.d.store.log_channel(guild).await.or(self.d.cfg.guild(guild).log_channel_id)
    }

    /// The effective config for a guild: static config.toml ([defaults] + [guild.<id>])
    /// deep-merged with the guild's DB override doc. Cached for 30s; invalidated by
    /// `invalidate_guild_config` on every `/guard config` write. A corrupt stored doc
    /// falls back to the static config (fail-open to the known-good state).
    pub async fn effective_guild_config(&self, guild: u64) -> Arc<GuildConfig> {
        if let Some(c) = self.cfg_cache.get(&guild) {
            return c;
        }
        let g = match self.d.store.guild_override(guild).await {
            Some(doc) if !doc.trim().is_empty() => self.d.cfg.apply_override(guild, &doc).unwrap_or_else(|e| {
                tracing::warn!("stored config override for guild {guild} is invalid ({e}); using static config");
                self.d.cfg.guild(guild).clone()
            }),
            _ => self.d.cfg.guild(guild).clone(),
        };
        let g = Arc::new(g);
        self.cfg_cache.put(guild, g.clone());
        g
    }

    pub fn invalidate_guild_config(&self, guild: u64) {
        self.cfg_cache.invalidate(&guild);
    }

    /// Validate a candidate override doc for a guild without saving it.
    pub fn check_guild_override(&self, guild: u64, doc: &str) -> anyhow::Result<GuildConfig> {
        self.d.cfg.apply_override(guild, doc)
    }

    /// (provider name, daily cap) for status output.
    pub fn caps(&self) -> Vec<(&'static str, u64)> {
        vec![
            ("paxmod", self.d.budgets.paxmod.usage().2),
            ("jev", self.d.budgets.jev.usage().2),
            ("ocr", self.d.budgets.ocr.usage().2),
        ]
    }

    pub fn gc(&self, now: DateTime<Utc>) {
        self.tracker.gc(now);
    }

    /// Wait for outstanding context-feed sends (used by tests and shutdown).
    pub async fn drain_feed(&self) {
        let handles: Vec<_> = std::mem::take(&mut *self.feed_tasks.lock().unwrap());
        for h in handles {
            let _ = h.await;
        }
    }

    pub async fn handle(&self, m: IncomingMessage, now: DateTime<Utc>) -> Outcome {
        let g = m.guild_id;
        let gc = self.effective_guild_config(g).await;
        let mut profile = m.profile.clone();
        if gc.allowlisted_user_ids.contains(&m.author_id) || self.d.store.is_allowlisted(g, m.author_id).await {
            profile.is_trusted = true;
        }
        let history = self.d.store.history(g, m.author_id, now).await.unwrap_or_else(|e| {
            tracing::warn!("history lookup failed: {e}");
            MemberHistory::default()
        });
        let is_first = history.messages_seen == 0 && !m.is_edit;
        let has_link = contains_url(&m.content);
        let hints = TierHints { risky_first_message: is_first && (has_link || m.distinct_mentions > 0) };
        let tier = compute_tier(&profile, &history, hints, &gc.tiers, now);

        if tier == Tier::Trusted {
            self.maybe_feed(&m, tier, Plan::NONE, now);
            return Outcome::skipped(tier);
        }

        // ---- local rules ----
        if !m.is_edit {
            if let Err(e) = self.d.store.note_message(g, m.author_id).await {
                tracing::warn!("note_message failed: {e}");
            }
        }
        if let Err(e) = self.d.store.add_recent(g, m.author_id, m.channel_id, m.message_id, now).await {
            tracing::warn!("add_recent failed: {e}");
        }
        let recent = self.tracker.snapshot(g, m.author_id, now);
        let facts = MessageFacts {
            text: &m.content,
            channel_id: m.channel_id,
            now,
            distinct_mentions: m.distinct_mentions,
            mentions_everyone_text: m.mentions_everyone_text,
            can_mention_everyone: m.can_mention_everyone,
            is_first_message: is_first,
        };
        let mut local = evaluate(&facts, &recent, &gc.rules, &gc);
        if !m.is_edit {
            self.tracker.push(g, m.author_id, RecentMsg { at: now, channel_id: m.channel_id, hash: hash_text(&normalize(&m.content)), excerpt: trunc(&m.content, 500) });
        }

        // ---- plan ----
        let grad = graduated(tier, &history, &gc.tiers);
        let weak_image = !m.image_urls.is_empty() && (m.content.trim().chars().count() < 10 || has_link || m.distinct_mentions > 0);
        let mut plan = detection_plan(tier, grad, local.score, weak_image, (self.d.roll)(), &gc.plan);
        plan.ocr = plan.ocr && ocr_eligible(&profile, &gc.tiers, now);
        let prio = if tier == Tier::Quarantine || (tier == Tier::Fresh && !grad) { Priority::High } else { Priority::Low };
        self.maybe_feed(&m, tier, plan, now);

        let mut coverage = Coverage::default();
        let mut failures: Vec<String> = vec![];

        // ---- OCR ----
        let mut ocr_text = String::new();
        if plan.ocr && !m.image_urls.is_empty() {
            let (t, cov) = self.run_ocr(&m, prio, now, &mut failures).await;
            ocr_text = t;
            coverage.ocr = cov;
        }
        if !ocr_text.is_empty() {
            let young = matches!(tier, Tier::Quarantine | Tier::Fresh);
            local.merge(image_text_signals(&ocr_text, m.content.trim().is_empty(), young, &gc.rules, &gc));
        }

        // ---- providers ----
        let combined = if ocr_text.is_empty() { m.content.clone() } else { format!("{}\n[image text] {}", m.content, ocr_text) };
        let combined = trunc(&combined, 5000);
        let mut pax: Option<PaxmodResult> = None;
        let mut jev: Option<JevResult> = None;
        if plan.providers && !combined.trim().is_empty() {
            let state = self.jev_state(&m, &profile, &ocr_text, &recent, now);
            // The Jev cache key carries author context (tier + account-age bucket in
            // hours) so a verdict for a brand-new account is never reused for an
            // established one posting the same text.
            let jev_ctx = format!("{}:{}", tier.name(), (now - m.profile.account_created).num_hours());
            let (p, j) = tokio::join!(
                self.run_paxmod(&m, &combined, prio, now, &mut failures),
                self.run_jev(g, &combined, state, prio, now, &jev_ctx)
            );
            let (pr, pc) = p;
            let (jr, jc, jf) = j;
            pax = pr;
            jev = jr;
            coverage.paxmod = pc;
            coverage.jev = jc;
            failures.extend(jf);
        }

        // ---- fuse + decide ----
        let verdict = fuse(
            &Inputs { local: Some(&local), paxmod: pax.as_ref(), jev: jev.as_ref(), coverage },
            &gc.fusion,
            flag_threshold(tier, &gc.ladder),
        );
        let planned = decide(tier, &verdict, &history, &gc.ladder);
        tracing::info!(
            "decision: guild={g} msg={} user={} tier={} local={:.3} providers={} ocr={} fused={:.3} label={} planned={} signals={:?} everyone={} first={} text={:?}",
            m.message_id, m.author_id, tier.name(), local.score, plan.providers, plan.ocr,
            verdict.score, verdict.label.name(), planned.name(),
            local.signals.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            facts.mentions_everyone_text, facts.is_first_message,
            trunc(&m.content, 120)
        );

        // ---- execute ----
        let mut taken = "none".to_string();
        let mut swept = 0usize;
        let mut strike: Option<&'static str> = None;
        if planned == Action::Flag {
            taken = "flag".into();
        } else if planned > Action::Flag {
            // Re-read the mode right before acting so a switch to Observe cancels this.
            if self.d.store.mode(g).await == Mode::Observe {
                taken = "would-have".into();
            } else if self.d.store.incident_exists(g, m.message_id).await {
                taken = "already".into();
            } else {
                let r = self.execute(&m, tier, planned, now, &mut failures).await;
                taken = r.0;
                swept = r.1;
                strike = r.2;
            }
        }

        // ---- record ----
        let mut incident_id = None;
        // "already": the incident for this message exists and was actioned; do not
        // overwrite its stored action_taken or post a second mod-log embed.
        if planned >= Action::Flag && taken != "already" {
            let ni = NewIncident {
                guild_id: g,
                user_id: m.author_id,
                channel_id: m.channel_id,
                message_id: m.message_id,
                tier: tier.name().into(),
                label: verdict.label.name().into(),
                score: verdict.score,
                local_signals: serde_json::to_string(&local.signals).unwrap_or_default(),
                paxmod_json: pax.as_ref().and_then(|p| serde_json::to_string(p).ok()),
                jev_json: jev.as_ref().and_then(|j| serde_json::to_string(j).ok()),
                ocr_json: (!ocr_text.is_empty()).then(|| serde_json::json!({"text": trunc(&ocr_text, 500)}).to_string()),
                verdict_json: serde_json::to_string(&verdict).unwrap_or_default(),
                action_planned: planned.name().into(),
                action_taken: taken.clone(),
                outcome: if taken == "would-have" { "observe".into() } else { "open".into() },
                text_excerpt: trunc(&m.content, 500),
            };
            match self.d.store.insert_incident(&ni, now).await {
                Ok(id) => incident_id = Some(id),
                Err(e) => tracing::error!("insert_incident failed: {e}"),
            }
            if let Some(kind) = strike {
                if let Err(e) = self.d.store.add_strike(g, m.author_id, kind, gc.ladder.strike_days, incident_id, now).await {
                    tracing::warn!("add_strike failed: {e}");
                }
            }
            if let Err(e) = self.d.store.note_flag(g, m.author_id, now).await {
                tracing::warn!("note_flag failed: {e}");
            }
            if let Some(id) = incident_id {
                let acct = (now - m.profile.account_created).num_days();
                let report = IncidentReport {
                    incident_id: id,
                    guild_id: g,
                    user_id: m.author_id,
                    channel_id: m.channel_id,
                    message_id: m.message_id,
                    tier: tier.name().into(),
                    label: verdict.label.name().into(),
                    score: verdict.score,
                    spam_score: verdict.spam_score,
                    abuse_score: verdict.abuse_score,
                    agreeing_sources: verdict.agreeing_sources,
                    planned: planned.name().into(),
                    taken: taken.clone(),
                    observe: taken == "would-have",
                    account_age_days: acct,
                    minutes_in_guild: m.profile.joined_at.map(|j| (now - j).num_minutes()),
                    coverage: format!("{:?}", verdict.coverage),
                    evidence: verdict.evidence.clone(),
                    excerpt: trunc(&m.content, 500),
                    image_text: (!ocr_text.is_empty()).then(|| trunc(&ocr_text, 500)),
                    failures: failures.clone(),
                    swept,
                };
                self.d.modlog.incident(&report).await;
            }
        } else if verdict.label == Label::Clean
            && (matches!(verdict.coverage.paxmod, Cov::Ran | Cov::Degraded) || verdict.coverage.jev == Cov::Ran)
        {
            if let Err(e) = self.d.store.note_clean_scan(g, m.author_id).await {
                tracing::warn!("note_clean_scan failed: {e}");
            }
        }

        Outcome {
            tier,
            plan,
            verdict: Some(verdict),
            planned,
            taken,
            incident_id,
            swept,
            ocr_text: (!ocr_text.is_empty()).then_some(ocr_text),
        }
    }

    // ------------------------------------------------------------------

    fn jev_state(&self, m: &IncomingMessage, p: &MemberProfile, ocr: &str, recent: &[RecentMsg], now: DateTime<Utc>) -> JevState {
        let n = self.d.cfg.runtime.jev_prior_messages;
        let prior: Vec<String> = recent.iter().rev().take(n).rev().map(|r| r.excerpt.clone()).collect();
        JevState {
            message: trunc(&m.content, self.d.cfg.runtime.jev_text_chars),
            image_text: (!ocr.is_empty()).then(|| trunc(ocr, 1000)),
            prior_messages: prior,
            account_age_days: (now - p.account_created).num_days(),
            minutes_in_guild: p.joined_at.map(|j| (now - j).num_minutes()),
            has_default_avatar: p.has_default_avatar,
            role_count: p.role_count,
        }
    }

    async fn announce_once(&self, guild: u64, provider: &'static str, kind: &'static str, now: DateTime<Utc>, text: String) {
        let first = self.announced.lock().unwrap().insert((provider, kind, now.date_naive()));
        if first {
            self.d.modlog.notice(guild, &text).await;
        }
    }

    async fn on_event(&self, guild: u64, name: &str, ev: Option<BreakerEvent>) {
        match ev {
            Some(BreakerEvent::Opened) => {
                self.d.modlog.notice(guild, &format!("{name} is failing repeatedly; pausing it for about a minute. Local rules keep running.")).await
            }
            Some(BreakerEvent::Closed) => self.d.modlog.notice(guild, &format!("{name} recovered.")).await,
            None => {}
        }
    }

    async fn run_paxmod(&self, m: &IncomingMessage, text: &str, prio: Priority, now: DateTime<Utc>, failures: &mut Vec<String>) -> (Option<PaxmodResult>, Cov) {
        let g: Guarded<PaxmodResult> = guarded(&self.d.budgets.paxmod, prio, now, || {
            self.d.paxmod.moderate(text, Some(m.author_id), Some(m.channel_id))
        })
        .await;
        self.on_event(m.guild_id, "Paxmod", g.event).await;
        if g.called {
            let _ = self.d.store.record_usage("paxmod", 1, g.result.is_err() as i64, now).await;
        }
        match g.result {
            Ok(r) => {
                let cov = if r.degraded { Cov::Degraded } else { Cov::Ran };
                (Some(r), cov)
            }
            Err(GuardErr::Denied(Denied::OverCap)) => {
                self.announce_once(m.guild_id, "paxmod", "cap", now, "Paxmod daily call cap reached; its checks are paused until tomorrow (UTC).".into()).await;
                failures.push("paxmod: over cap".into());
                (None, Cov::Skipped)
            }
            Err(GuardErr::Denied(Denied::CircuitOpen)) => {
                failures.push("paxmod: circuit open".into());
                (None, Cov::Skipped)
            }
            Err(GuardErr::Provider(e)) => {
                tracing::warn!("paxmod failed: {e}");
                failures.push(format!("paxmod: {e}"));
                (None, Cov::Failed)
            }
        }
    }

    /// `cache_ctx` binds the cache entry to the author's trust context (tier and
    /// account-age bucket), so identical text from different author contexts costs
    /// separate calls and cannot share a verdict.
    async fn run_jev(&self, guild: u64, text: &str, state: JevState, prio: Priority, now: DateTime<Utc>, cache_ctx: &str) -> (Option<JevResult>, Cov, Vec<String>) {
        let key = hash_text(&format!("{cache_ctx}\n{}", normalize(text)));
        if let Some(hit) = self.jev_cache.get(&key) {
            return (Some(hit), Cov::Ran, vec![]);
        }
        let g: Guarded<JevResult> = guarded(&self.d.budgets.jev, prio, now, || self.d.jev.assess(&state)).await;
        self.on_event(guild, "Jev", g.event).await;
        if g.called {
            let _ = self.d.store.record_usage("jev", 1, g.result.is_err() as i64, now).await;
        }
        match g.result {
            Ok(r) => {
                self.jev_cache.put(key, r.clone());
                (Some(r), Cov::Ran, vec![])
            }
            Err(GuardErr::Denied(Denied::OverCap)) => {
                self.announce_once(guild, "jev", "cap", now, "Jev daily call cap reached; its checks are paused until tomorrow (UTC).".into()).await;
                (None, Cov::Skipped, vec!["jev: over cap".into()])
            }
            Err(GuardErr::Denied(Denied::CircuitOpen)) => (None, Cov::Skipped, vec!["jev: circuit open".into()]),
            Err(GuardErr::Provider(e)) => {
                tracing::warn!("jev failed: {e}");
                (None, Cov::Failed, vec![format!("jev: {e}")])
            }
        }
    }

    async fn run_ocr(&self, m: &IncomingMessage, prio: Priority, now: DateTime<Utc>, failures: &mut Vec<String>) -> (String, Cov) {
        let oc = &self.d.cfg.ocr;
        let (mut ok, mut bad) = (0usize, 0usize);
        let mut texts: Vec<String> = vec![];
        for url in m.image_urls.iter().take(oc.max_images_per_message) {
            let bytes = match self.d.fetcher.fetch(url, oc.max_download_bytes).await {
                Ok(b) => b,
                Err(e) => {
                    tracing::debug!("image fetch failed: {e}");
                    failures.push("ocr: image download failed".into());
                    bad += 1;
                    continue;
                }
            };
            let key = hash_bytes(&bytes);
            if let Some(t) = self.ocr_cache.get(&key) {
                ok += 1;
                texts.push(t);
                continue;
            }
            let prepared = match imaging::prepare(&bytes, self.d.ocr_max_upload) {
                Ok(p) => p,
                Err(e) => {
                    failures.push(format!("ocr: image unusable ({e:?})"));
                    bad += 1;
                    continue;
                }
            };
            let g: Guarded<String> = guarded(&self.d.budgets.ocr, prio, now, || self.d.ocr.read(&prepared.bytes, prepared.mime)).await;
            self.on_event(m.guild_id, "OCR", g.event).await;
            if g.called {
                let _ = self.d.store.record_usage("ocr", 1, g.result.is_err() as i64, now).await;
            }
            match g.result {
                Ok(t) => {
                    let t = t.trim().to_string();
                    let t = if t.chars().filter(|c| !c.is_whitespace()).count() < oc.min_text_chars { String::new() } else { t };
                    self.ocr_cache.put(key, t.clone());
                    ok += 1;
                    texts.push(t);
                }
                Err(GuardErr::Denied(Denied::OverCap)) => {
                    self.announce_once(m.guild_id, "ocr", "cap", now, "OCR daily call cap reached; image text is not being read until tomorrow (UTC).".into()).await;
                    failures.push("ocr: over cap".into());
                    bad += 1;
                }
                Err(GuardErr::Denied(Denied::CircuitOpen)) => {
                    failures.push("ocr: circuit open".into());
                    bad += 1;
                }
                Err(GuardErr::Provider(e)) => {
                    failures.push(format!("ocr: {e}"));
                    bad += 1;
                }
            }
        }
        let cov = match (ok, bad) {
            (0, 0) => Cov::Skipped,
            (0, _) => Cov::Failed,
            (_, 0) => Cov::Ran,
            _ => Cov::Degraded,
        };
        let joined = texts.into_iter().filter(|t| !t.is_empty()).collect::<Vec<_>>().join("\n");
        (joined, cov)
    }

    /// Paxmod Player Context feed: send messages the plan would skip so Paxmod sees the
    /// whole conversation. Results are not acted on.
    fn maybe_feed(&self, m: &IncomingMessage, tier: Tier, plan: Plan, now: DateTime<Utc>) {
        let window = Duration::minutes(self.d.cfg.paxmod.feed_window_minutes);
        if matches!(tier, Tier::Quarantine | Tier::Fresh) {
            self.activity.note(m.channel_id, now);
        }
        if plan.providers || m.content.trim().is_empty() {
            return;
        }
        let apply = match self.d.cfg.paxmod.context_feed {
            ContextFeed::Scanned => false,
            ContextFeed::All => true,
            ContextFeed::ActiveChannels => self.activity.active(m.channel_id, now, window),
        };
        if !apply {
            return;
        }
        let (pax, budget) = (self.d.paxmod.clone(), self.d.budgets.paxmod.clone());
        let store = self.d.store.clone();
        let (text, user, ch) = (trunc(&m.content, 5000), m.author_id, m.channel_id);
        let h = tokio::spawn(async move {
            let g = guarded(&budget, Priority::Low, now, || pax.moderate(&text, Some(user), Some(ch))).await;
            if g.called {
                let _ = store.record_usage("paxmod", 1, g.result.is_err() as i64, now).await;
            }
            if let Err(GuardErr::Provider(e)) = g.result {
                tracing::debug!("context feed send failed: {e}");
            }
        });
        let mut tasks = self.feed_tasks.lock().unwrap();
        tasks.retain(|t| !t.is_finished());
        tasks.push(h);
    }

    async fn execute(&self, m: &IncomingMessage, tier: Tier, action: Action, now: DateTime<Utc>, failures: &mut Vec<String>) -> (String, usize, Option<&'static str>) {
        let g = m.guild_id;
        let gc = self.effective_guild_config(g).await;
        let l = &gc.ladder;
        let s = &self.d.sink;
        if let Err(why) = s.can_act(g, m.author_id).await {
            failures.push(format!("refused: {why}"));
            return ("refused".into(), 0, None);
        }
        let reason = "sudobot-chat-guard automatic action";
        let mut main_ok = true;
        let mut strike = None;

        if action != Action::Ban {
            if let Err(e) = s.delete(m.channel_id, m.message_id).await {
                failures.push(format!("delete failed: {e}"));
                main_ok = false;
            }
        }
        match action {
            Action::DeleteTimeout => match s.timeout(g, m.author_id, l.timeout_minutes).await {
                Ok(()) => strike = Some("timeout"),
                Err(e) => {
                    failures.push(format!("timeout failed: {e}"));
                    main_ok = false;
                }
            },
            Action::Kick => match s.kick(g, m.author_id, reason).await {
                Ok(()) => strike = Some("kick"),
                Err(e) => {
                    failures.push(format!("kick failed: {e}"));
                    main_ok = false;
                }
            },
            Action::Ban => match s.ban(g, m.author_id, l.ban_delete_seconds, reason).await {
                Ok(()) => strike = Some("ban"),
                Err(e) => {
                    failures.push(format!("ban failed: {e}"));
                    main_ok = false;
                }
            },
            _ => {}
        }

        let mut swept = 0;
        if main_ok && action >= Action::Delete && matches!(tier, Tier::Quarantine | Tier::Fresh) && action != Action::Ban {
            swept = self.sweep(m, now, failures).await;
        }
        let taken = if main_ok { action.name().to_string() } else { "failed".to_string() };
        (taken, swept, strike)
    }

    async fn sweep(&self, m: &IncomingMessage, now: DateTime<Utc>, failures: &mut Vec<String>) -> usize {
        let gc = self.effective_guild_config(m.guild_id).await;
        let l = &gc.ladder;
        let rows = match self.d.store.recent_since(m.guild_id, m.author_id, now - Duration::minutes(l.sweep_minutes)).await {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("sweep lookup failed: {e}"));
                return 0;
            }
        };
        let mut by_channel: HashMap<u64, Vec<u64>> = HashMap::new();
        for (ch, id) in rows.into_iter().filter(|(_, id)| *id != m.message_id) {
            by_channel.entry(ch).or_default().push(id);
        }
        let mut done = 0;
        let mut forgotten = vec![];
        for (ch, ids) in by_channel {
            for chunk in ids.chunks(100) {
                match self.d.sink.bulk_delete(ch, chunk).await {
                    Ok(()) => {
                        done += chunk.len();
                        forgotten.extend_from_slice(chunk);
                    }
                    Err(e) => failures.push(format!("sweep failed in channel: {e}")),
                }
            }
        }
        let _ = self.d.store.forget_recent(m.guild_id, &forgotten).await;
        done
    }
}

#[derive(Debug, Clone)]
pub struct JoinEvent {
    pub guild_id: u64,
    pub user_id: u64,
    pub username: String,
    pub display_name: String,
    pub profile: MemberProfile,
}

impl Pipeline {
    /// Join-time screening. Only flags the member (raising their tier) and posts an
    /// alert. It never kicks or bans: names alone are weak evidence.
    pub async fn handle_join(&self, j: JoinEvent, now: DateTime<Utc>) -> bool {
        let gc = self.effective_guild_config(j.guild_id).await;
        let mut reasons: Vec<String> = vec![];
        let names = format!("{} {}", j.username, j.display_name);

        if j.profile.has_default_avatar && now - j.profile.account_created < Duration::hours(24) {
            reasons.push("default avatar on an account under 24h old".into());
        }
        if contains_url(&names) || !crate::normalize::extract_invite_codes(&names).is_empty() {
            reasons.push("link or invite in name".into());
        }
        let n = normalize(&names);
        if ["free nitro", "giveaway", "airdrop", "crypto", "admin support", "mod support", "support team"].iter().any(|w| n.contains(w)) {
            reasons.push("spam wording in name".into());
        }
        let blocked: Vec<String> = gc.blocked_domains.iter().map(|d| d.to_lowercase()).collect();
        if crate::normalize::extract_domains(&names).iter().any(|d| blocked.iter().any(|b| d == b || d.ends_with(&format!(".{b}")))) {
            reasons.push("blocked domain in name".into());
        }

        // Providers on the name fields. No context_id: names must not pollute conversations.
        let now_p = now;
        let g = guarded(&self.d.budgets.paxmod, Priority::Low, now_p, || self.d.paxmod.moderate(names.trim(), Some(j.user_id), None)).await;
        if g.called {
            let _ = self.d.store.record_usage("paxmod", 1, g.result.is_err() as i64, now).await;
        }
        if let Ok(r) = g.result {
            let (spam, abuse) = crate::fusion::paxmod_risks(&r);
            if spam >= 0.7 || abuse >= 0.8 {
                reasons.push(format!("Paxmod flagged name ({:.2})", spam.max(abuse)));
            }
        }
        let state = JevState {
            message: names.trim().to_string(),
            account_age_days: (now - j.profile.account_created).num_days(),
            minutes_in_guild: Some(0),
            has_default_avatar: j.profile.has_default_avatar,
            role_count: j.profile.role_count,
            ..Default::default()
        };
        let g = guarded(&self.d.budgets.jev, Priority::Low, now_p, || self.d.jev.assess(&state)).await;
        if g.called {
            let _ = self.d.store.record_usage("jev", 1, g.result.is_err() as i64, now).await;
        }
        if let Ok(r) = g.result {
            let (spam, _) = crate::fusion::jev_risks(&r);
            if spam >= 0.8 {
                reasons.push(format!("Jev flagged name ({spam:.2})"));
            }
        }

        if reasons.is_empty() {
            return false;
        }
        if let Err(e) = self.d.store.set_join_flag(j.guild_id, j.user_id, true).await {
            tracing::warn!("set_join_flag failed: {e}");
        }
        self.d
            .modlog
            .notice(j.guild_id, &format!("Join screening flagged <@{}>: {}. Their tier is raised; no action taken.", j.user_id, reasons.join("; ")))
            .await;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::ProviderError;
    use async_trait::async_trait;

    // ---------- fakes ----------

    /// (message, user_id, context_id) recorded for each Paxmod call.
    type PaxCall = (String, Option<u64>, Option<u64>);

    #[derive(Default)]
    struct FakePax {
        result: Mutex<Option<Result<PaxmodResult, ProviderError>>>,
        calls: Mutex<Vec<PaxCall>>,
    }
    impl FakePax {
        fn spam(score: f64) -> Arc<Self> {
            let f = Arc::new(FakePax::default());
            *f.result.lock().unwrap() = Some(Ok(PaxmodResult { categories: [("Spam".to_string(), score)].into(), ..Default::default() }));
            f
        }
        fn failing() -> Arc<Self> {
            let f = Arc::new(FakePax::default());
            *f.result.lock().unwrap() = Some(Err(ProviderError::Server(503)));
            f
        }
        fn n(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }
    #[async_trait]
    impl TextModerator for FakePax {
        async fn moderate(&self, msg: &str, u: Option<u64>, c: Option<u64>) -> Result<PaxmodResult, ProviderError> {
            self.calls.lock().unwrap().push((msg.into(), u, c));
            self.result.lock().unwrap().clone().unwrap_or_else(|| Ok(PaxmodResult::default()))
        }
    }

    struct FakeJev {
        result: Result<JevResult, ProviderError>,
        calls: Mutex<u32>,
        flip_to_observe: Mutex<Option<Store>>,
    }
    impl FakeJev {
        fn new(r: Result<JevResult, ProviderError>) -> Arc<Self> {
            Arc::new(FakeJev { result: r, calls: Mutex::new(0), flip_to_observe: Mutex::new(None) })
        }
        fn clean() -> Arc<Self> {
            Self::new(Ok(JevResult { class: "legitimate".into(), class_probs: [("legitimate".to_string(), 1.0)].into(), ..Default::default() }))
        }
        fn spam() -> Arc<Self> {
            Self::new(Ok(JevResult { class: "spam".into(), class_probs: [("legitimate".to_string(), 0.02)].into(), ..Default::default() }))
        }
        fn n(&self) -> u32 {
            *self.calls.lock().unwrap()
        }
    }
    #[async_trait]
    impl Classifier for FakeJev {
        async fn assess(&self, _s: &JevState) -> Result<JevResult, ProviderError> {
            *self.calls.lock().unwrap() += 1;
            let st = self.flip_to_observe.lock().unwrap().clone();
            if let Some(st) = st {
                st.set_mode(1, Mode::Observe, 0).await.unwrap();
            }
            self.result.clone()
        }
    }

    struct FakeOcr {
        text: String,
        calls: Mutex<u32>,
    }
    #[async_trait]
    impl TextReader for FakeOcr {
        async fn read(&self, _b: &[u8], _m: &str) -> Result<String, ProviderError> {
            *self.calls.lock().unwrap() += 1;
            Ok(self.text.clone())
        }
    }

    struct FakeFetcher(Vec<u8>);
    #[async_trait]
    impl ImageFetcher for FakeFetcher {
        async fn fetch(&self, _u: &str, _m: usize) -> anyhow::Result<Vec<u8>> {
            Ok(self.0.clone())
        }
    }

    #[derive(Default)]
    struct FakeSink {
        log: Mutex<Vec<String>>,
        refuse: bool,
    }
    impl FakeSink {
        fn has(&self, s: &str) -> bool {
            self.log.lock().unwrap().iter().any(|l| l.starts_with(s))
        }
        fn count(&self, s: &str) -> usize {
            self.log.lock().unwrap().iter().filter(|l| l.starts_with(s)).count()
        }
        fn empty(&self) -> bool {
            self.log.lock().unwrap().is_empty()
        }
    }
    #[async_trait]
    impl ActionSink for FakeSink {
        async fn can_act(&self, _g: u64, _u: u64) -> Result<(), String> {
            if self.refuse { Err("role hierarchy".into()) } else { Ok(()) }
        }
        async fn delete(&self, c: u64, m: u64) -> anyhow::Result<()> {
            self.log.lock().unwrap().push(format!("delete {c} {m}"));
            Ok(())
        }
        async fn bulk_delete(&self, c: u64, m: &[u64]) -> anyhow::Result<()> {
            self.log.lock().unwrap().push(format!("bulk {c} {m:?}"));
            Ok(())
        }
        async fn timeout(&self, _g: u64, u: u64, mins: u32) -> anyhow::Result<()> {
            self.log.lock().unwrap().push(format!("timeout {u} {mins}"));
            Ok(())
        }
        async fn kick(&self, _g: u64, u: u64, _r: &str) -> anyhow::Result<()> {
            self.log.lock().unwrap().push(format!("kick {u}"));
            Ok(())
        }
        async fn ban(&self, _g: u64, u: u64, _d: u32, _r: &str) -> anyhow::Result<()> {
            self.log.lock().unwrap().push(format!("ban {u}"));
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeLog {
        incidents: Mutex<Vec<IncidentReport>>,
        notices: Mutex<Vec<String>>,
    }
    #[async_trait]
    impl ModLog for FakeLog {
        async fn incident(&self, r: &IncidentReport) {
            self.incidents.lock().unwrap().push(r.clone());
        }
        async fn notice(&self, _g: u64, t: &str) {
            self.notices.lock().unwrap().push(t.into());
        }
    }

    // ---------- harness ----------

    struct H {
        p: Pipeline,
        pax: Arc<FakePax>,
        jev: Arc<FakeJev>,
        ocr: Arc<FakeOcr>,
        sink: Arc<FakeSink>,
        log: Arc<FakeLog>,
    }

    fn png() -> Vec<u8> {
        imaging::png_bytes(&image::DynamicImage::new_rgb8(8, 8))
    }

    async fn harness(pax: Arc<FakePax>, jev: Arc<FakeJev>, enforce: bool, roll: f64, sink: FakeSink) -> H {
        harness_with_cfg(Config::default(), pax, jev, enforce, roll, sink).await
    }

    async fn harness_with_cfg(cfg: Config, pax: Arc<FakePax>, jev: Arc<FakeJev>, enforce: bool, roll: f64, sink: FakeSink) -> H {
        let store = Store::memory().await.unwrap();
        if enforce {
            store.set_mode(1, Mode::Enforce, 0).await.unwrap();
        }
        let ocr = Arc::new(FakeOcr { text: "CLAIM FREE NITRO at scam-site.xyz".into(), calls: Mutex::new(0) });
        let sink = Arc::new(sink);
        let log = Arc::new(FakeLog::default());
        let p = Pipeline::new(Deps {
            cfg: Arc::new(cfg),
            store,
            paxmod: pax.clone(),
            jev: jev.clone(),
            ocr: ocr.clone(),
            fetcher: Arc::new(FakeFetcher(png())),
            sink: sink.clone(),
            modlog: log.clone(),
            budgets: Budgets { paxmod: Arc::new(Budget::new(1000, 0.5)), jev: Arc::new(Budget::new(1000, 0.5)), ocr: Arc::new(Budget::new(1000, 0.5)) },
            ocr_max_upload: 1_000_000,
            roll: Arc::new(move || roll),
        });
        H { p, pax, jev, ocr, sink, log }
    }

    fn profile(user: u64, account_hours: i64, in_guild_minutes: i64) -> MemberProfile {
        let now = Utc::now();
        MemberProfile {
            guild_id: 1,
            user_id: user,
            account_created: now - Duration::hours(account_hours),
            joined_at: Some(now - Duration::minutes(in_guild_minutes)),
            is_trusted: false,
            has_default_avatar: false,
            role_count: 0,
        }
    }

    fn msg(id: u64, user: u64, text: &str, p: MemberProfile) -> IncomingMessage {
        IncomingMessage {
            guild_id: 1,
            channel_id: 50,
            message_id: id,
            author_id: user,
            content: text.into(),
            created_at: Utc::now(),
            image_urls: vec![],
            distinct_mentions: 0,
            mentions_everyone_text: false,
            can_mention_everyone: false,
            is_edit: false,
            profile: p,
        }
    }

    fn new_user(u: u64) -> MemberProfile {
        profile(u, 2, 3) // 2h old account, 3 minutes in guild: Quarantine
    }
    fn old_user(u: u64) -> MemberProfile {
        profile(u, 24 * 1000, 60 * 24 * 100) // established
    }

    /// Seed enough clean scans for `u` to earn the Established tier (sleeper-alt
    /// rule: Established requires history, not just time).
    async fn seed_clean(h: &H, u: u64) {
        for _ in 0..10 {
            h.p.store().note_clean_scan(1, u).await.unwrap();
        }
    }

    // ---------- tests ----------

    #[tokio::test]
    async fn quarantine_spam_is_deleted_timed_out_and_swept_in_enforce() {
        let h = harness(FakePax::spam(0.8), FakeJev::clean(), true, 0.99, FakeSink::default()).await;
        let now = Utc::now();
        // two earlier messages from the same spammer
        h.p.store().add_recent(1, 7, 50, 901, now - Duration::minutes(2)).await.unwrap();
        h.p.store().add_recent(1, 7, 51, 902, now - Duration::minutes(1)).await.unwrap();
        let o = h.p.handle(msg(1000, 7, "buy cheap stuff now", new_user(7)), now).await;
        assert_eq!(o.tier, Tier::Quarantine);
        assert_eq!(o.planned, Action::DeleteTimeout);
        assert_eq!(o.taken, "delete+timeout");
        assert!(h.sink.has("delete 50 1000"));
        assert!(h.sink.has("timeout 7 60"));
        assert_eq!(o.swept, 2);
        assert!(h.sink.has("bulk 50 [901]") && h.sink.has("bulk 51 [902]"));
        assert_eq!(h.log.incidents.lock().unwrap().len(), 1);
        // strike recorded, so the next message is escalated
        assert_eq!(h.p.store().history(1, 7, now).await.unwrap().active_strikes, 1);
    }

    #[tokio::test]
    async fn observe_mode_performs_no_discord_action() {
        let h = harness(FakePax::spam(0.8), FakeJev::clean(), false, 0.99, FakeSink::default()).await;
        let o = h.p.handle(msg(1, 7, "buy cheap stuff now", new_user(7)), Utc::now()).await;
        assert_eq!(o.planned, Action::DeleteTimeout);
        assert_eq!(o.taken, "would-have");
        assert!(h.sink.empty());
        assert!(h.log.incidents.lock().unwrap()[0].observe);
        assert_eq!(h.p.store().history(1, 7, Utc::now()).await.unwrap().active_strikes, 0);
    }

    #[tokio::test]
    async fn mode_switched_to_observe_mid_flight_cancels_action() {
        let jev = FakeJev::spam();
        let h = harness(FakePax::spam(0.9), jev.clone(), true, 0.99, FakeSink::default()).await;
        *jev.flip_to_observe.lock().unwrap() = Some(h.p.store().clone());
        let o = h.p.handle(msg(1, 7, "free nitro dm me", new_user(7)), Utc::now()).await;
        assert!(o.planned > Action::Flag);
        assert_eq!(o.taken, "would-have");
        assert!(h.sink.empty());
    }

    #[tokio::test]
    async fn hierarchy_refusal_is_recorded_not_retried() {
        let h = harness(FakePax::spam(0.8), FakeJev::clean(), true, 0.99, FakeSink { refuse: true, ..Default::default() }).await;
        let o = h.p.handle(msg(1, 7, "buy cheap stuff now", new_user(7)), Utc::now()).await;
        assert_eq!(o.taken, "refused");
        assert!(h.sink.empty());
        assert!(h.log.incidents.lock().unwrap()[0].failures.iter().any(|f| f.contains("hierarchy")));
    }

    #[tokio::test]
    async fn trusted_members_are_never_scanned_or_actioned() {
        let h = harness(FakePax::spam(1.0), FakeJev::spam(), true, 0.0, FakeSink::default()).await;
        let mut p = new_user(7);
        p.is_trusted = true;
        let o = h.p.handle(msg(1, 7, "free nitro", p), Utc::now()).await;
        assert_eq!(o.tier, Tier::Trusted);
        assert!(h.sink.empty());
        assert_eq!(h.pax.n(), 0);
    }

    #[tokio::test]
    async fn allowlisted_member_becomes_trusted() {
        let h = harness(FakePax::spam(1.0), FakeJev::spam(), true, 0.0, FakeSink::default()).await;
        h.p.store().set_allowlisted(1, 7, true).await.unwrap();
        let o = h.p.handle(msg(1, 7, "free nitro", new_user(7)), Utc::now()).await;
        assert_eq!(o.tier, Tier::Trusted);
    }

    #[tokio::test]
    async fn established_clean_message_skips_paid_providers() {
        let h = harness(FakePax::spam(1.0), FakeJev::spam(), true, 0.99, FakeSink::default()).await;
        seed_clean(&h, 8).await; // Established must be earned by history
        let o = h.p.handle(msg(1, 8, "hello everyone, nice weather", old_user(8)), Utc::now()).await;
        assert_eq!(o.tier, Tier::Established);
        assert_eq!(o.plan, Plan::NONE);
        assert_eq!((h.pax.n(), h.jev.n()), (0, 0));
        assert_eq!(o.taken, "none");
    }

    #[tokio::test]
    async fn established_local_hit_triggers_providers() {
        let h = harness(FakePax::spam(0.0), FakeJev::clean(), false, 0.99, FakeSink::default()).await;
        seed_clean(&h, 8).await;
        let mut m = msg(1, 8, "hi", old_user(8));
        m.distinct_mentions = 12; // mass mention, 0.7
        let o = h.p.handle(m, Utc::now()).await;
        assert!(o.plan.providers);
        assert_eq!(h.pax.n(), 1);
    }

    #[tokio::test]
    async fn paxmod_receives_user_and_context_ids() {
        let h = harness(FakePax::spam(0.0), FakeJev::clean(), false, 0.99, FakeSink::default()).await;
        h.p.handle(msg(1, 7, "hello there", new_user(7)), Utc::now()).await;
        let c = h.pax.calls.lock().unwrap();
        assert_eq!(c[0].1, Some(7));
        assert_eq!(c[0].2, Some(50));
    }

    #[tokio::test]
    async fn both_providers_down_fails_open_to_local_rules() {
        let h = harness(FakePax::failing(), FakeJev::new(Err(ProviderError::Timeout)), true, 0.99, FakeSink::default()).await;
        let o = h.p.handle(msg(1, 7, "just chatting here", new_user(7)), Utc::now()).await;
        let v = o.verdict.unwrap();
        assert_eq!(v.coverage.paxmod, Cov::Failed);
        assert_eq!(v.coverage.jev, Cov::Failed);
        assert_eq!(o.taken, "none");
        // local rules still work when providers are down: blocked shortener on first message
        let o = h.p.handle(msg(2, 9, "see https://bit.ly/x https://bit.ly/y discord.gg/abc", new_user(9)), Utc::now()).await;
        assert!(o.verdict.unwrap().score > 0.0);
    }

    #[tokio::test]
    async fn over_cap_providers_are_skipped_and_announced_once() {
        let h = harness(FakePax::spam(0.9), FakeJev::spam(), false, 0.99, FakeSink::default()).await;
        h.p.d.budgets.paxmod.preload(1000);
        for i in 0..3 {
            let o = h.p.handle(msg(i, 7, "hello", new_user(7)), Utc::now()).await;
            assert_eq!(o.verdict.unwrap().coverage.paxmod, Cov::Skipped);
        }
        assert_eq!(h.pax.n(), 0);
        let n = h.log.notices.lock().unwrap().iter().filter(|t| t.contains("Paxmod daily call cap")).count();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn same_message_twice_is_actioned_once() {
        let h = harness(FakePax::spam(0.8), FakeJev::clean(), true, 0.99, FakeSink::default()).await;
        let m = msg(5, 7, "buy cheap stuff now", new_user(7));
        h.p.handle(m.clone(), Utc::now()).await;
        let mut edit = m;
        edit.is_edit = true;
        let o = h.p.handle(edit, Utc::now()).await;
        assert_eq!(o.taken, "already");
        assert_eq!(h.sink.count("delete 50 5"), 1);
    }

    #[tokio::test]
    async fn jev_results_are_cached_for_crossposted_text() {
        let h = harness(FakePax::spam(0.0), FakeJev::clean(), false, 0.99, FakeSink::default()).await;
        h.p.handle(msg(1, 7, "check this out", new_user(7)), Utc::now()).await;
        h.p.handle(msg(2, 8, "Check  this   OUT", new_user(8)), Utc::now()).await;
        assert_eq!(h.jev.n(), 1);
        assert_eq!(h.pax.n(), 2, "Paxmod must see every message and author");
    }

    #[tokio::test]
    async fn image_only_scam_is_caught_via_ocr_and_text_reaches_providers() {
        let h = harness(FakePax::spam(0.0), FakeJev::clean(), false, 0.99, FakeSink::default()).await;
        let mut m = msg(1, 7, "", new_user(7));
        m.image_urls = vec!["https://cdn.discordapp.com/attachments/1/2/a.png".into()];
        let o = h.p.handle(m, Utc::now()).await;
        assert!(o.ocr_text.as_deref().unwrap().contains("FREE NITRO"));
        let v = o.verdict.unwrap();
        assert_eq!(v.coverage.ocr, Cov::Ran);
        assert!(v.evidence.iter().any(|e| e.contains("image_text_bait")));
        let sent = h.pax.calls.lock().unwrap()[0].0.clone();
        assert!(sent.contains("[image text]") && sent.contains("scam-site.xyz"));
    }

    #[tokio::test]
    async fn repeated_image_costs_one_ocr_call() {
        let h = harness(FakePax::spam(0.0), FakeJev::clean(), false, 0.99, FakeSink::default()).await;
        for i in 0..3 {
            let mut m = msg(i, 7, "look", new_user(7));
            m.image_urls = vec!["https://cdn.discordapp.com/a.png".into()];
            h.p.handle(m, Utc::now()).await;
        }
        assert_eq!(*h.ocr.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn established_members_are_never_ocrd() {
        let h = harness(FakePax::spam(0.0), FakeJev::clean(), false, 0.99, FakeSink::default()).await;
        seed_clean(&h, 8).await;
        let mut m = msg(1, 8, "here is the picture from our trip yesterday", old_user(8));
        m.image_urls = vec!["https://cdn.discordapp.com/a.png".into()];
        h.p.handle(m, Utc::now()).await;
        assert_eq!(*h.ocr.calls.lock().unwrap(), 0);
        // not even for an image-only message
        let mut m = msg(2, 8, "", old_user(8));
        m.image_urls = vec!["https://cdn.discordapp.com/b.png".into()];
        h.p.handle(m, Utc::now()).await;
        assert_eq!(*h.ocr.calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn sleeper_without_history_is_not_ocrd() {
        let h = harness(FakePax::spam(0.0), FakeJev::clean(), false, 0.99, FakeSink::default()).await;
        let mut m = msg(1, 8, "", old_user(8));
        m.image_urls = vec!["https://cdn.discordapp.com/a.png".into()];
        let o = h.p.handle(m, Utc::now()).await;
        assert_eq!(o.tier, Tier::Fresh);
        assert_eq!(*h.ocr.calls.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn time_young_fresh_member_is_ocrd() {
        let h = harness(FakePax::spam(0.0), FakeJev::clean(), false, 0.99, FakeSink::default()).await;
        let mut m = msg(1, 8, "", profile(8, 24 * 10, 60 * 24 * 2));
        m.image_urls = vec!["https://cdn.discordapp.com/a.png".into()];
        let o = h.p.handle(m, Utc::now()).await;
        assert_eq!(o.tier, Tier::Fresh);
        assert_eq!(*h.ocr.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn context_feed_sends_active_channel_messages_to_paxmod() {
        let h = harness(FakePax::spam(0.0), FakeJev::clean(), false, 0.99, FakeSink::default()).await;
        seed_clean(&h, 8).await; // earn Established so the member is fed, not scanned
        let now = Utc::now();
        // a new member speaks in channel 50, opening the feed window
        h.p.handle(msg(1, 7, "hello", new_user(7)), now).await;
        let before = h.pax.n();
        // an established member replies in the same channel: not scanned, but fed
        let o = h.p.handle(msg(2, 8, "hi there friend", old_user(8)), now).await;
        h.p.drain_feed().await;
        assert_eq!(o.plan, Plan::NONE);
        assert_eq!(h.pax.n(), before + 1);
        assert_eq!(h.jev.n(), 1, "feed goes to Paxmod only");
        // a different, quiet channel is not fed
        let mut other = msg(3, 8, "completely different words here", old_user(8));
        other.channel_id = 99;
        h.p.handle(other, now).await;
        h.p.drain_feed().await;
        assert_eq!(h.pax.n(), before + 1);
        // window expires
        h.p.handle(msg(4, 8, "much later", old_user(8)), now + Duration::minutes(11)).await;
        h.p.drain_feed().await;
        assert_eq!(h.pax.n(), before + 1);
    }

    #[tokio::test]
    async fn clean_scans_accumulate_and_flags_reset_them() {
        let h = harness(FakePax::spam(0.0), FakeJev::clean(), false, 0.99, FakeSink::default()).await;
        let now = Utc::now();
        for i in 0..3 {
            h.p.handle(msg(i, 7, &format!("totally normal message number {i}"), new_user(7)), now).await;
        }
        assert_eq!(h.p.store().history(1, 7, now).await.unwrap().clean_scans, 3);
    }

    #[tokio::test]
    async fn join_screening_flags_but_never_acts() {
        let h = harness(FakePax::spam(0.0), FakeJev::clean(), true, 0.99, FakeSink::default()).await;
        let mut p = new_user(7);
        p.has_default_avatar = true;
        let flagged = h
            .p
            .handle_join(JoinEvent { guild_id: 1, user_id: 7, username: "FREE NITRO giveaway".into(), display_name: "x".into(), profile: p }, Utc::now())
            .await;
        assert!(flagged);
        assert!(h.sink.empty());
        assert!(h.p.store().history(1, 7, Utc::now()).await.unwrap().join_flag);
        assert_eq!(h.log.notices.lock().unwrap().len(), 1);
        // an ordinary join is not flagged and Paxmod saw no context_id
        let ok = h
            .p
            .handle_join(JoinEvent { guild_id: 1, user_id: 8, username: "alice".into(), display_name: "Alice".into(), profile: old_user(8) }, Utc::now())
            .await;
        assert!(!ok);
        assert!(h.pax.calls.lock().unwrap().iter().all(|c| c.2.is_none()));
    }

    #[tokio::test]
    async fn context_feed_and_join_screening_record_usage() {
        let h = harness(FakePax::spam(0.0), FakeJev::clean(), false, 0.99, FakeSink::default()).await;
        seed_clean(&h, 8).await; // earn Established so the member is fed, not scanned
        let now = Utc::now();
        // quarantine member speaks: scanned (paxmod 1, jev 1), opens the feed window
        h.p.handle(msg(1, 7, "hello", new_user(7)), now).await;
        // established member in the same channel: context-fed to Paxmod only
        h.p.handle(msg(2, 8, "hi there friend", old_user(8)), now).await;
        h.p.drain_feed().await;
        // join screening calls both providers at Low priority
        h.p.handle_join(JoinEvent { guild_id: 1, user_id: 9, username: "alice".into(), display_name: "a".into(), profile: old_user(9) }, now).await;
        assert_eq!(h.p.store().usage_today("paxmod", now).await.0, 3, "scan + feed + join screening");
        assert_eq!(h.p.store().usage_today("jev", now).await.0, 2, "scan + join screening");
    }

    #[tokio::test]
    async fn effective_log_channel_db_override_beats_config_default() {        let cfg = Config::from_toml_str("[defaults]\nlog_channel_id = 9\n").unwrap();
        let h = harness_with_cfg(cfg, FakePax::spam(0.0), FakeJev::clean(), false, 0.99, FakeSink::default()).await;
        assert_eq!(h.p.effective_log_channel(1).await, Some(9), "config default applies when no DB override");
        h.p.store().set_log_channel(1, 42, 7).await.unwrap();
        assert_eq!(h.p.effective_log_channel(1).await, Some(42), "DB override wins over config");
        assert_eq!(h.p.effective_log_channel(2).await, Some(9), "other guilds keep the config default");
    }

    #[tokio::test]
    async fn guild_override_doc_changes_effective_config() {
        let h = harness(FakePax::spam(0.0), FakeJev::clean(), false, 0.99, FakeSink::default()).await;
        assert!(h.p.effective_guild_config(1).await.blocked_domains.is_empty());
        let doc = crate::config::override_set(None, "blocked_domains", "evil.com, scam.xyz").unwrap();
        h.p.store().set_guild_override(1, &doc, 7).await.unwrap();
        h.p.invalidate_guild_config(1);
        let gc = h.p.effective_guild_config(1).await;
        assert_eq!(gc.blocked_domains, vec!["evil.com".to_string(), "scam.xyz".to_string()]);
        assert!(h.p.effective_guild_config(2).await.blocked_domains.is_empty(), "other guilds unaffected");
        // a corrupt stored doc fails open to the static config
        h.p.store().set_guild_override(1, "ladder = 42", 7).await.unwrap();
        h.p.invalidate_guild_config(1);
        assert!(h.p.effective_guild_config(1).await.blocked_domains.is_empty());
    }

    // ---------- regression tests for audit findings (all #[ignore]d) ----------
    // Regression M5 (fixed): reprocessing an already-actioned message returns
    // "already" and leaves the stored incident's action_taken untouched (no upsert,
    // no second mod-log embed).
    #[tokio::test]
    async fn m5_reprocessing_does_not_overwrite_action_taken() {
        let h = harness(FakePax::spam(0.8), FakeJev::clean(), true, 0.99, FakeSink::default()).await;
        let m = msg(5, 7, "buy cheap stuff now", new_user(7));
        let first = h.p.handle(m.clone(), Utc::now()).await;
        assert_eq!(first.taken, "delete+timeout");
        let id = first.incident_id.unwrap();
        let second = h.p.handle(m, Utc::now()).await;
        assert_eq!(second.taken, "already", "second pass should recognize the incident");
        assert_eq!(h.sink.count("delete 50 5"), 1);
        let row = h.p.store().incident(id).await.unwrap().unwrap();
        assert_eq!(row.action_taken, "delete+timeout", "stored action_taken must survive reprocessing");
    }

    // Regression S2 (fixed): the Jev cache key includes author context (tier and
    // account-age bucket), so identical text from a 2h-old and a 20h-old quarantine
    // account no longer shares one cached verdict. Asserted at the observable level
    // (Jev call count) because jev_cache is private.
    #[tokio::test]
    async fn s2_jev_cache_key_includes_author_context() {
        let h = harness(FakePax::spam(0.0), FakeJev::clean(), false, 0.99, FakeSink::default()).await;
        h.p.handle(msg(1, 7, "check this out", profile(7, 2, 3)), Utc::now()).await; // 2h-old account
        h.p.handle(msg(2, 8, "check this out", profile(8, 20, 3)), Utc::now()).await; // 20h-old account
        assert_eq!(h.jev.n(), 2, "identical text from different author contexts must not share a Jev cache entry");
    }
}
