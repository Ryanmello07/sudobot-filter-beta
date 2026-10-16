//! Free local signals. Run on message text and on OCR text.

use crate::config::{GuildConfig, RuleConfig};
use crate::model::{LocalResult, Signal};
use crate::normalize::{extract_domains, extract_invite_codes, hash_text, normalize};
use chrono::{DateTime, Duration, Utc};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

const SHORTENERS: &[&str] = &[
    "bit.ly", "tinyurl.com", "t.co", "goo.gl", "is.gd", "cutt.ly", "rb.gy", "shorturl.at", "ow.ly", "tiny.cc",
];

#[derive(Debug, Clone)]
pub struct RecentMsg {
    pub at: DateTime<Utc>,
    pub channel_id: u64,
    pub hash: String,
    /// First 500 chars of the text, in memory only, for Jev context.
    pub excerpt: String,
}

/// Per-author sliding window, in memory only.
#[derive(Default)]
pub struct RecentTracker {
    inner: Mutex<HashMap<(u64, u64), VecDeque<RecentMsg>>>,
}

const KEEP: Duration = Duration::minutes(10);
const MAX_PER_AUTHOR: usize = 64;

impl RecentTracker {
    pub fn snapshot(&self, guild: u64, user: u64, now: DateTime<Utc>) -> Vec<RecentMsg> {
        let mut g = self.inner.lock().unwrap();
        match g.get_mut(&(guild, user)) {
            Some(q) => {
                while q.front().is_some_and(|m| now - m.at > KEEP) {
                    q.pop_front();
                }
                q.iter().cloned().collect()
            }
            None => vec![],
        }
    }

    pub fn push(&self, guild: u64, user: u64, msg: RecentMsg) {
        let mut g = self.inner.lock().unwrap();
        let q = g.entry((guild, user)).or_default();
        q.push_back(msg);
        while q.len() > MAX_PER_AUTHOR {
            q.pop_front();
        }
    }

    /// Drop authors with no recent activity.
    pub fn gc(&self, now: DateTime<Utc>) {
        let mut g = self.inner.lock().unwrap();
        g.retain(|_, q| q.back().is_some_and(|m| now - m.at <= KEEP));
    }
}

#[derive(Debug, Clone)]
pub struct MessageFacts<'a> {
    pub text: &'a str,
    pub channel_id: u64,
    pub now: DateTime<Utc>,
    pub distinct_mentions: usize,
    pub mentions_everyone_text: bool,
    pub can_mention_everyone: bool,
    pub is_first_message: bool,
}

fn sig(name: &str, w: f64) -> Signal {
    Signal { name: name.to_string(), weight: w }
}

/// Rules that depend on prior messages. `recent` must not include this message.
pub fn evaluate(
    m: &MessageFacts,
    recent: &[RecentMsg],
    rc: &RuleConfig,
    g: &GuildConfig,
) -> LocalResult {
    let mut out = vec![];
    let w = &rc.weights;
    let norm = normalize(m.text);

    // Flood: this message plus recent ones within the window.
    let in_window = recent.iter().filter(|r| m.now - r.at <= Duration::seconds(rc.flood_seconds)).count() + 1;
    if in_window >= rc.flood_messages {
        out.push(sig("flood", w.flood));
    }

    // Duplicate: same normalized text in 2+ channels or 3+ times within the window.
    if norm.chars().count() >= rc.duplicate_min_len {
        let h = hash_text(&norm);
        let dups: Vec<&RecentMsg> = recent
            .iter()
            .filter(|r| r.hash == h && m.now - r.at <= Duration::seconds(rc.duplicate_window_secs))
            .collect();
        let mut channels: Vec<u64> = dups.iter().map(|r| r.channel_id).collect();
        channels.push(m.channel_id);
        channels.sort_unstable();
        channels.dedup();
        if channels.len() >= 2 || dups.len() + 1 >= 3 {
            out.push(sig("duplicate", w.duplicate));
        }
    }

    out.extend(text_signals(m.text, m.is_first_message, rc, g).signals);

    if m.distinct_mentions > rc.mass_mention_limit {
        out.push(sig("mass_mention", w.mass_mention));
    }
    if m.mentions_everyone_text && !m.can_mention_everyone {
        out.push(sig("everyone_attempt", w.everyone_attempt));
    }
    LocalResult::from_signals(out)
}

/// Stateless text rules (links, invites, blocked domains). Also used on OCR text.
pub fn text_signals(text: &str, is_first_message: bool, rc: &RuleConfig, g: &GuildConfig) -> LocalResult {
    let w = &rc.weights;
    let mut out = vec![];
    let domains = extract_domains(text);

    if is_first_message && !domains.is_empty() {
        out.push(sig("first_message_link", w.first_message_link));
    }

    let allowed: Vec<String> = g.allowed_invites.iter().map(|s| s.to_lowercase()).collect();
    if extract_invite_codes(text).iter().any(|c| !allowed.contains(c)) {
        out.push(sig("invite_link", w.invite_link));
    }

    let blocked: Vec<String> = g.blocked_domains.iter().map(|s| s.to_lowercase()).collect();
    let is_blocked = |d: &str| {
        blocked.iter().any(|b| d == b || d.ends_with(&format!(".{b}"))) || SHORTENERS.contains(&d)
    };
    if domains.iter().any(|d| is_blocked(d)) {
        out.push(sig("blocked_domain", w.blocked_domain));
    }
    LocalResult::from_signals(out)
}

/// Signals derived from OCR text. Any link or invite in an image is suspicious.
pub fn image_text_signals(ocr_text: &str, image_only_message: bool, young_author: bool, rc: &RuleConfig, g: &GuildConfig) -> LocalResult {
    let mut r = text_signals(ocr_text, false, rc, g);
    let norm = normalize(ocr_text);
    let has_link = !extract_domains(ocr_text).is_empty() || !extract_invite_codes(ocr_text).is_empty();
    const BAIT: &[&str] = &["giveaway", "airdrop", "free nitro", "claim your", "guaranteed profit", "wallet", "double your", "investment"];
    let bait_words = BAIT.iter().any(|b| norm.contains(b));
    if bait_words || (image_only_message && young_author && has_link) {
        r.merge(LocalResult::from_signals(vec![sig("image_text_bait", rc.weights.image_text_bait)]));
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 14, 12, 0, 0).unwrap()
    }

    fn facts(text: &str) -> MessageFacts<'_> {
        MessageFacts {
            text,
            channel_id: 1,
            now: now(),
            distinct_mentions: 0,
            mentions_everyone_text: false,
            can_mention_everyone: false,
            is_first_message: false,
        }
    }

    fn run(f: &MessageFacts, recent: &[RecentMsg]) -> LocalResult {
        evaluate(f, recent, &RuleConfig::default(), &GuildConfig::default())
    }

    fn rm(secs_ago: i64, ch: u64, text: &str) -> RecentMsg {
        RecentMsg { at: now() - Duration::seconds(secs_ago), channel_id: ch, hash: hash_text(&normalize(text)), excerpt: text.into() }
    }

    #[test]
    fn clean_message_has_no_signals() {
        let r = run(&facts("hey, how is everyone doing today?"), &[]);
        assert!(r.signals.is_empty());
        assert_eq!(r.score, 0.0);
    }

    #[test]
    fn flood() {
        let recent: Vec<_> = (1..=4).map(|i| rm(i, 1, &format!("m{i}"))).collect();
        assert!(run(&facts("hello again"), &recent).has("flood"));
        let recent: Vec<_> = (1..=3).map(|i| rm(i, 1, &format!("m{i}"))).collect();
        assert!(!run(&facts("hello again"), &recent).has("flood"));
        // old messages do not count
        let recent: Vec<_> = (30..34).map(|i| rm(i, 1, &format!("m{i}"))).collect();
        assert!(!run(&facts("hello again"), &recent).has("flood"));
    }

    #[test]
    fn duplicate_across_channels() {
        let recent = vec![rm(20, 2, "Free nitro here for everyone")];
        assert!(run(&facts("free  nitro here for everyone"), &recent).has("duplicate"));
    }

    #[test]
    fn duplicate_same_channel_needs_three() {
        let t = "buy my thing right now";
        assert!(!run(&facts(t), &[rm(10, 1, t)]).has("duplicate"));
        assert!(run(&facts(t), &[rm(10, 1, t), rm(5, 1, t)]).has("duplicate"));
    }

    #[test]
    fn duplicate_ignores_short_text() {
        assert!(!run(&facts("lol"), &[rm(5, 2, "lol")]).has("duplicate"));
    }

    #[test]
    fn duplicate_window_expires() {
        let t = "free nitro here for everyone";
        assert!(!run(&facts(t), &[rm(200, 2, t)]).has("duplicate"));
    }

    #[test]
    fn mass_mention_and_everyone() {
        let mut f = facts("hi all");
        f.distinct_mentions = 6;
        assert!(run(&f, &[]).has("mass_mention"));
        f.distinct_mentions = 5;
        assert!(!run(&f, &[]).has("mass_mention"));
        f.mentions_everyone_text = true;
        assert!(run(&f, &[]).has("everyone_attempt"));
        f.can_mention_everyone = true;
        assert!(!run(&f, &[]).has("everyone_attempt"));
    }

    #[test]
    fn first_message_link() {
        let mut f = facts("check https://example.com");
        assert!(!run(&f, &[]).has("first_message_link"));
        f.is_first_message = true;
        assert!(run(&f, &[]).has("first_message_link"));
    }

    #[test]
    fn invite_links_respect_allowlist() {
        let f = facts("join discord.gg/Spam123");
        assert!(run(&f, &[]).has("invite_link"));
        let g = GuildConfig { allowed_invites: vec!["Spam123".into()], ..Default::default() };
        assert!(!evaluate(&f, &[], &RuleConfig::default(), &g).has("invite_link"));
    }

    #[test]
    fn blocked_domains_and_shorteners() {
        let g = GuildConfig { blocked_domains: vec!["evil.com".into()], ..Default::default() };
        let rc = RuleConfig::default();
        assert!(text_signals("see http://sub.evil.com/x", false, &rc, &g).has("blocked_domain"));
        assert!(text_signals("see https://bit.ly/abc", false, &rc, &g).has("blocked_domain"));
        assert!(!text_signals("see https://good.com", false, &rc, &g).has("blocked_domain"));
        assert!(!text_signals("notevil.com", false, &rc, &g).has("blocked_domain"));
    }

    #[test]
    fn evasive_link_still_caught() {
        let g = GuildConfig { blocked_domains: vec!["evil.com".into()], ..Default::default() };
        assert!(text_signals("go to ev\u{200B}il.com", false, &RuleConfig::default(), &g).has("blocked_domain"));
    }

    #[test]
    fn image_text_bait() {
        let rc = RuleConfig::default();
        let g = GuildConfig::default();
        assert!(image_text_signals("CLAIM YOUR FREE NITRO giveaway", false, false, &rc, &g).has("image_text_bait"));
        assert!(image_text_signals("visit scam-site.xyz", true, true, &rc, &g).has("image_text_bait"));
        assert!(!image_text_signals("visit scam-site.xyz", true, false, &rc, &g).has("image_text_bait"));
        assert!(image_text_signals("a cute cat picture caption", true, true, &rc, &g).signals.is_empty());
    }

    #[test]
    fn tracker_window_and_gc() {
        let t = RecentTracker::default();
        t.push(1, 2, rm(700, 1, "old"));
        t.push(1, 2, rm(5, 1, "new"));
        assert_eq!(t.snapshot(1, 2, now()).len(), 1);
        t.gc(now() + Duration::minutes(30));
        assert!(t.snapshot(1, 2, now()).is_empty());
    }
}
