//! Core domain types shared across modules.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tier {
    Trusted,
    Quarantine,
    Fresh,
    Established,
}

impl Tier {
    /// One step toward Quarantine. Trusted never changes.
    pub fn escalate(self) -> Tier {
        match self {
            Tier::Established => Tier::Fresh,
            Tier::Fresh => Tier::Quarantine,
            other => other,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Tier::Trusted => "trusted",
            Tier::Quarantine => "quarantine",
            Tier::Fresh => "fresh",
            Tier::Established => "established",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    Observe,
    Enforce,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Observe => "observe",
            Mode::Enforce => "enforce",
        }
    }
    pub fn parse(s: &str) -> Option<Mode> {
        match s {
            "observe" => Some(Mode::Observe),
            "enforce" => Some(Mode::Enforce),
            _ => None,
        }
    }
}

/// Facts about the author that do not depend on stored history.
#[derive(Debug, Clone)]
pub struct MemberProfile {
    pub guild_id: u64,
    pub user_id: u64,
    pub account_created: DateTime<Utc>,
    pub joined_at: Option<DateTime<Utc>>,
    /// Trusted role, Manage Messages or higher, owner, or allowlisted.
    pub is_trusted: bool,
    pub has_default_avatar: bool,
    pub role_count: usize,
}

/// Stored behavior used for tier modifiers.
#[derive(Debug, Clone, Default)]
pub struct MemberHistory {
    pub clean_scans: u32,
    pub active_strikes: u32,
    pub flagged_recently: bool,
    pub join_flag: bool,
    pub messages_seen: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Action {
    None,
    Flag,
    Delete,
    DeleteTimeout,
    Kick,
    Ban,
}

impl Action {
    pub fn name(self) -> &'static str {
        match self {
            Action::None => "none",
            Action::Flag => "flag",
            Action::Delete => "delete",
            Action::DeleteTimeout => "delete+timeout",
            Action::Kick => "kick",
            Action::Ban => "ban",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Label {
    Clean,
    Spam,
    Scam,
    Abuse,
}

impl Label {
    pub fn name(self) -> &'static str {
        match self {
            Label::Clean => "clean",
            Label::Spam => "spam",
            Label::Scam => "scam",
            Label::Abuse => "abuse",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Signal {
    pub name: String,
    pub weight: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LocalResult {
    pub score: f64,
    pub signals: Vec<Signal>,
}

impl LocalResult {
    pub fn from_signals(signals: Vec<Signal>) -> Self {
        let score = noisy_or(signals.iter().map(|s| s.weight));
        LocalResult { score, signals }
    }
    pub fn merge(&mut self, other: LocalResult) {
        for s in other.signals {
            if !self.signals.iter().any(|x| x.name == s.name) {
                self.signals.push(s);
            }
        }
        self.score = noisy_or(self.signals.iter().map(|s| s.weight));
    }
    pub fn has(&self, name: &str) -> bool {
        self.signals.iter().any(|s| s.name == name)
    }
}

/// 1 - prod(1 - x), inputs clamped to [0,1].
pub fn noisy_or(values: impl Iterator<Item = f64>) -> f64 {
    1.0 - values.fold(1.0, |acc, v| acc * (1.0 - v.clamp(0.0, 1.0)))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaxmodResult {
    pub id: Option<String>,
    pub categories: HashMap<String, f64>,
    pub flagged: bool,
    pub reason: Option<String>,
    pub degraded: bool,
    pub ban_hit: bool,
    /// Unknown top-level response fields (for example Player Context data).
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JevResult {
    pub class: String,
    pub class_probs: HashMap<String, f64>,
    pub class_confidence: f64,
    pub dm_lure: f64,
    pub crypto_giveaway: f64,
    pub evasion: f64,
    pub bot_like: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Cov {
    #[default]
    Skipped,
    Ran,
    Degraded,
    Failed,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Coverage {
    pub paxmod: Cov,
    pub jev: Cov,
    pub ocr: Cov,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub score: f64,
    pub spam_score: f64,
    pub abuse_score: f64,
    pub label: Label,
    pub agreeing_sources: u8,
    pub coverage: Coverage,
    pub evidence: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noisy_or_combines() {
        assert!((noisy_or([0.5, 0.5].into_iter()) - 0.75).abs() < 1e-9);
        assert_eq!(noisy_or(std::iter::empty()), 0.0);
        assert_eq!(noisy_or([2.0].into_iter()), 1.0);
    }

    #[test]
    fn escalate_stops_at_quarantine_and_trusted() {
        assert_eq!(Tier::Established.escalate(), Tier::Fresh);
        assert_eq!(Tier::Fresh.escalate(), Tier::Quarantine);
        assert_eq!(Tier::Quarantine.escalate(), Tier::Quarantine);
        assert_eq!(Tier::Trusted.escalate(), Tier::Trusted);
    }

    #[test]
    fn merge_dedups_by_name() {
        let mut a = LocalResult::from_signals(vec![Signal { name: "x".into(), weight: 0.5 }]);
        let b = LocalResult::from_signals(vec![
            Signal { name: "x".into(), weight: 0.5 },
            Signal { name: "y".into(), weight: 0.5 },
        ]);
        a.merge(b);
        assert_eq!(a.signals.len(), 2);
        assert!((a.score - 0.75).abs() < 1e-9);
    }
}
