//! High-level what-if simulations: provider outage, flaky provider, quota exhaustion,
//! a raid of identical spam, and an evasion corpus against the local rules.
//!
//! These tests assert CURRENT correct behavior and must pass with the tree as-is.
//! Known evasion gaps are documented in the #[ignore]d regression tests instead
//! (see src/rules.rs S3/S8, src/fusion.rs M1/M4, etc.).

use async_trait::async_trait;
use chrono::{Duration, Utc};
use std::sync::{Arc, Mutex};
use sudobot_chat_guard::budget::Budget;
use sudobot_chat_guard::config::{Config, GuildConfig, RuleConfig};
use sudobot_chat_guard::imaging;
use sudobot_chat_guard::model::*;
use sudobot_chat_guard::pipeline::{Budgets, Deps, IncomingMessage, Pipeline};
use sudobot_chat_guard::ports::*;
use sudobot_chat_guard::providers::jev::{JevClient, JevState};
use sudobot_chat_guard::providers::ocr::OcrClient;
use sudobot_chat_guard::providers::paxmod::PaxmodClient;
use sudobot_chat_guard::providers::ProviderError;
use sudobot_chat_guard::rules::text_signals;
use sudobot_chat_guard::secret::Secret;
use sudobot_chat_guard::store::Store;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

// ---------- fakes ----------

#[derive(Default)]
struct Log {
    notices: Mutex<Vec<String>>,
    incidents: Mutex<Vec<IncidentReport>>,
}
#[async_trait]
impl ModLog for Log {
    async fn incident(&self, r: &IncidentReport) {
        self.incidents.lock().unwrap().push(r.clone());
    }
    async fn notice(&self, _g: u64, t: &str) {
        self.notices.lock().unwrap().push(t.into());
    }
}

#[derive(Default)]
struct NopSink;
#[async_trait]
impl ActionSink for NopSink {
    async fn can_act(&self, _: u64, _: u64) -> Result<(), String> {
        Ok(())
    }
    async fn delete(&self, _: u64, _: u64) -> anyhow::Result<()> {
        Ok(())
    }
    async fn bulk_delete(&self, _: u64, _: &[u64]) -> anyhow::Result<()> {
        Ok(())
    }
    async fn timeout(&self, _: u64, _: u64, _: u32) -> anyhow::Result<()> {
        Ok(())
    }
    async fn kick(&self, _: u64, _: u64, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn ban(&self, _: u64, _: u64, _: u32, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Provider stand-in that instantly fails in a non-health-affecting way.
struct Offline;
#[async_trait]
impl TextModerator for Offline {
    async fn moderate(&self, _: &str, _: Option<u64>, _: Option<u64>) -> Result<PaxmodResult, ProviderError> {
        Err(ProviderError::Rejected(0, "offline".into()))
    }
}
#[async_trait]
impl Classifier for Offline {
    async fn assess(&self, _: &JevState) -> Result<JevResult, ProviderError> {
        Err(ProviderError::Rejected(0, "offline".into()))
    }
}
#[async_trait]
impl TextReader for Offline {
    async fn read(&self, _: &[u8], _: &str) -> Result<String, ProviderError> {
        Err(ProviderError::Rejected(0, "offline".into()))
    }
}
#[async_trait]
impl ImageFetcher for Offline {
    async fn fetch(&self, _: &str, _: usize) -> anyhow::Result<Vec<u8>> {
        anyhow::bail!("offline")
    }
}

struct PngFetcher(Vec<u8>);
#[async_trait]
impl ImageFetcher for PngFetcher {
    async fn fetch(&self, _: &str, _: usize) -> anyhow::Result<Vec<u8>> {
        Ok(self.0.clone())
    }
}

// ---------- harness ----------

struct Sim {
    p: Pipeline,
    log: Arc<Log>,
    paxmod_budget: Arc<Budget>,
    jev_budget: Arc<Budget>,
    ocr_budget: Arc<Budget>,
}

fn png() -> Vec<u8> {
    imaging::png_bytes(&image::DynamicImage::new_rgb8(8, 8))
}

async fn sim(
    paxmod: Arc<dyn TextModerator>,
    jev: Arc<dyn Classifier>,
    ocr: Arc<dyn TextReader>,
    fetcher: Arc<dyn ImageFetcher>,
) -> Sim {
    let log = Arc::new(Log::default());
    let (pb, jb, ob) = (Arc::new(Budget::new(1000, 0.5)), Arc::new(Budget::new(1000, 0.5)), Arc::new(Budget::new(1000, 0.5)));
    let p = Pipeline::new(Deps {
        cfg: Arc::new(Config::default()),
        store: Store::memory().await.unwrap(),
        paxmod,
        jev,
        ocr,
        fetcher,
        sink: Arc::new(NopSink),
        modlog: log.clone(),
        budgets: Budgets { paxmod: pb.clone(), jev: jb.clone(), ocr: ob.clone() },
        ocr_max_upload: 1_000_000,
        roll: Arc::new(|| 0.99),
    });
    Sim { p, log, paxmod_budget: pb, jev_budget: jb, ocr_budget: ob }
}

/// Quarantine tier: 2h-old account, 3 minutes in the guild.
fn quarantine_msg(id: u64, user: u64, text: &str) -> IncomingMessage {
    let now = Utc::now();
    IncomingMessage {
        guild_id: 1,
        channel_id: 50,
        message_id: id,
        author_id: user,
        content: text.into(),
        created_at: now,
        image_urls: vec![],
        distinct_mentions: 0,
        mentions_everyone_text: false,
        can_mention_everyone: false,
        is_edit: false,
        profile: MemberProfile {
            guild_id: 1,
            user_id: user,
            account_created: now - Duration::hours(2),
            joined_at: Some(now - Duration::minutes(3)),
            is_trusted: false,
            has_default_avatar: false,
            role_count: 0,
        },
    }
}

const PAX_OK: &str = r#"{"status":"success","id":"abc","service":"pax","result":"flagged",
  "content_moderation":{"flagged":true,"categories":{"Spam":{"flagged":true,"score":0.8,"threshold":0.5}}}}"#;

fn notices_containing(log: &Log, needle: &str) -> usize {
    log.notices.lock().unwrap().iter().filter(|t| t.contains(needle)).count()
}

// ---------- 1. outage simulation ----------

/// All three providers return 500 on every call. The pipeline must still produce
/// local-only verdicts, each breaker must open after the failure threshold, and each
/// provider's degradation must be announced exactly once per day.
#[tokio::test]
async fn outage_all_providers_fails_open_breakers_open_announced_once() {
    let mk = || async {
        let s = MockServer::start().await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(500)).mount(&s).await;
        s
    };
    let (ps, js, os) = (mk().await, mk().await, mk().await);
    let t = std::time::Duration::from_millis(500);
    let sim = sim(
        Arc::new(PaxmodClient::new(&ps.uri(), Secret::new("k"), t)),
        Arc::new(JevClient::new(&js.uri(), Secret::new("k"), "jev-latest", t)),
        Arc::new(OcrClient::new(&os.uri(), Secret::new("k"), 2, t)),
        Arc::new(PngFetcher(png())),
    )
    .await;

    for i in 0..6u64 {
        let mut m = quarantine_msg(i, 100 + i, "join discord.gg/xyz free nitro");
        m.image_urls = vec!["https://cdn.discordapp.com/attachments/1/2/a.png".into()];
        let o = sim.p.handle(m, Utc::now()).await;
        let v = o.verdict.expect("verdict must still be produced during an outage");
        assert!(v.score > 0.0, "local rules still score while providers are down");
        assert_eq!(v.label, Label::Spam);
        if i < 5 {
            assert_eq!(v.coverage.paxmod, Cov::Failed, "message {i}");
            assert_eq!(v.coverage.jev, Cov::Failed, "message {i}");
        } else {
            // Breaker open: no further HTTP calls, coverage reports Skipped.
            assert_eq!(v.coverage.paxmod, Cov::Skipped, "message {i}");
            assert_eq!(v.coverage.jev, Cov::Skipped, "message {i}");
        }
    }
    assert!(sim.paxmod_budget.breaker_open());
    assert!(sim.jev_budget.breaker_open());
    assert!(sim.ocr_budget.breaker_open());
    // Exactly one degradation announcement per provider per day.
    assert_eq!(notices_containing(&sim.log, "Paxmod is failing repeatedly"), 1);
    assert_eq!(notices_containing(&sim.log, "Jev is failing repeatedly"), 1);
    assert_eq!(notices_containing(&sim.log, "OCR is failing repeatedly"), 1);
}

// ---------- 2. flaky provider simulation ----------

/// First call 500s, the retry succeeds: exactly one budgeted call, verdict delivered.
#[tokio::test]
async fn flaky_provider_recovers_on_retry_with_single_budgeted_call() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let s = MockServer::start().await;
    let hits = Arc::new(AtomicU32::new(0));
    let h = hits.clone();
    // wiremock evaluates mocks in mount order: the one-shot 500 comes first, so the
    // first request fails and the retry falls through to the success responder.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(1)
        .expect(1)
        .mount(&s)
        .await;
    Mock::given(method("POST"))
        .respond_with(move |_: &wiremock::Request| {
            h.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_string(PAX_OK)
        })
        .mount(&s)
        .await;
    let sim = sim(
        Arc::new(PaxmodClient::new(&s.uri(), Secret::new("k"), std::time::Duration::from_millis(500))),
        Arc::new(Offline),
        Arc::new(Offline),
        Arc::new(Offline),
    )
    .await;

    let o = sim.p.handle(quarantine_msg(1, 100, "buy cheap stuff now"), Utc::now()).await;
    let v = o.verdict.unwrap();
    assert_eq!(v.coverage.paxmod, Cov::Ran, "verdict delivered after the retry");
    assert_eq!(sim.paxmod_budget.usage().0, 1, "a retry is not a second budgeted call");
    assert_eq!(hits.load(Ordering::SeqCst), 1, "the retry hits the success responder exactly once");
}

// ---------- 3. quota (429) simulation ----------

/// A 429 marks the provider exhausted for the day; later messages skip it without HTTP.
#[tokio::test]
async fn quota_429_marks_exhausted_and_skips_without_http() {
    use std::sync::atomic::{AtomicU32, Ordering};
    let s = MockServer::start().await;
    let hits = Arc::new(AtomicU32::new(0));
    let h = hits.clone();
    Mock::given(method("POST"))
        .respond_with(move |_: &wiremock::Request| {
            h.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(429)
        })
        .expect(1)
        .mount(&s)
        .await;
    let sim = sim(
        Arc::new(PaxmodClient::new(&s.uri(), Secret::new("k"), std::time::Duration::from_millis(500))),
        Arc::new(Offline),
        Arc::new(Offline),
        Arc::new(Offline),
    )
    .await;

    let o = sim.p.handle(quarantine_msg(1, 100, "hello there"), Utc::now()).await;
    assert_eq!(o.verdict.unwrap().coverage.paxmod, Cov::Failed, "the 429 itself surfaces as a failure");
    let o = sim.p.handle(quarantine_msg(2, 101, "hello again"), Utc::now()).await;
    assert_eq!(o.verdict.unwrap().coverage.paxmod, Cov::Skipped, "exhausted provider is skipped");
    let o = sim.p.handle(quarantine_msg(3, 102, "and again"), Utc::now()).await;
    assert_eq!(o.verdict.unwrap().coverage.paxmod, Cov::Skipped);
    assert_eq!(hits.load(Ordering::SeqCst), 1, "no HTTP after exhaustion (429 is not retried)");
    assert_eq!(notices_containing(&sim.log, "Paxmod daily call cap"), 1, "cap announced exactly once");
}

// ---------- 4. raid simulation ----------

/// 200 fresh accounts post identical spam. Local rules + normalization must detect
/// every one consistently; throughput is reported, not asserted.
#[tokio::test]
async fn raid_two_hundred_identical_spam_messages_all_detected() {
    let off = Arc::new(Offline);
    let sim = sim(off.clone(), off.clone(), off.clone(), off).await;
    const N: u64 = 200;
    const TEXT: &str = "FREE NITRO claim your reward at bit.ly/free-nitro now discord.gg/nitro";

    let start = std::time::Instant::now();
    let mut scores = Vec::new();
    for i in 0..N {
        let o = sim.p.handle(quarantine_msg(i, 1000 + i, TEXT), Utc::now()).await;
        let v = o.verdict.unwrap();
        assert!(v.score >= 0.4, "message {i} below the quarantine flag threshold: {:.3}", v.score);
        assert_ne!(v.label, Label::Clean, "message {i} not flagged");
        scores.push(v.score);
    }
    let elapsed = start.elapsed();
    assert!(
        scores.iter().all(|s| *s == scores[0]),
        "identical raid text must score identically: {:?}",
        scores
    );
    println!(
        "raid simulation: {N} messages in {:.2?} = {:.0} msgs/sec (local rules + offline providers)",
        elapsed,
        N as f64 / elapsed.as_secs_f64()
    );
}

// ---------- 5. evasion corpus ----------

/// Which evasion techniques are caught TODAY by normalize + the local text rules.
/// `caught` documents current behavior (this test must stay green); known gaps such
/// as trailing-dot domains and case-insensitive invite allowlists live in the
/// #[ignore]d regression tests (S3, S8) instead.
#[test]
fn evasion_corpus_current_coverage() {
    let rc = RuleConfig::default();
    let blocked = |domains: &[&str]| GuildConfig {
        blocked_domains: domains.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    let invites = |codes: &[&str]| GuildConfig {
        allowed_invites: codes.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };

    // (technique, text, caught today by blocked_domains = ["evil.com"])
    let domain_cases: &[(&str, &str, bool)] = &[
        ("plain link", "see https://evil.com/x", true),
        ("uppercase", "see HTTPS://EVIL.COM/x", true),
        ("cyrillic homoglyph", "see https://еvil.com/x", true), // Cyrillic е
        ("zero-width split", "go to ev\u{200B}il.com", true),
        ("fullwidth", "\u{FF48}\u{FF54}\u{FF54}\u{FF50}\u{FF53}\u{FF1A}\u{FF0F}\u{FF0F}\u{FF45}\u{FF56}\u{FF49}\u{FF4C}\u{FF0E}\u{FF43}\u{FF4F}\u{FF4D}/x", true),
        ("www prefix", "www.evil.com", true),
        ("subdomain", "https://login.evil.com/path", true),
        ("lookalike suffix host", "https://evil.com.evil.net/", false), // correct negative: different host
        ("similar name", "https://notevil.com/", false),                // correct negative
        ("no tld match", "visit evil.codes today", false),              // TLD not in the extractor's list
    ];
    for (tech, text, want) in domain_cases {
        let got = text_signals(text, false, &rc, &blocked(&["evil.com"])).has("blocked_domain");
        assert_eq!(got, *want, "domain case {tech:?}: {text:?}");
    }

    // (technique, text, flagged today with allowed_invites = ["abc123"])
    let invite_cases: &[(&str, &str, bool)] = &[
        ("allowlisted exact", "join discord.gg/abc123", false),
        ("allowlisted other case", "join discord.gg/ABC123", false), // S8: case-insensitive today
        ("unknown code", "join discord.gg/other999", true),
        ("discord.com/invite", "join https://discord.com/invite/abc123", false),
        ("dsc.gg shortener", "join dsc.gg/abc123", false),
        ("mixed with text", "hey everyone join discord.gg/other999 for fun", true),
    ];
    for (tech, text, want) in invite_cases {
        let got = text_signals(text, false, &rc, &invites(&["abc123"])).has("invite_link");
        assert_eq!(got, *want, "invite case {tech:?}: {text:?}");
    }
}
