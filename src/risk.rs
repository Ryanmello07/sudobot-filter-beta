//! New-account vigilance: tier computation and detection-plan helpers.

use crate::config::{PlanConfig, TierConfig};
use crate::model::{MemberHistory, MemberProfile, Tier};
use chrono::{DateTime, Duration, Utc};

/// Extra, message-derived signals that may escalate a tier.
#[derive(Debug, Clone, Copy, Default)]
pub struct TierHints {
    /// First message from this member in the guild contains a link or mention.
    pub risky_first_message: bool,
}

pub fn base_tier(p: &MemberProfile, cfg: &TierConfig, now: DateTime<Utc>) -> Tier {
    if p.is_trusted {
        return Tier::Trusted;
    }
    let account_age = now - p.account_created;
    let in_guild = p.joined_at.map(|j| now - j);

    let q = in_guild.is_some_and(|g| g < Duration::minutes(cfg.quarantine_minutes_in_guild))
        || account_age < Duration::hours(cfg.quarantine_account_hours);
    if q {
        return Tier::Quarantine;
    }
    let f = in_guild.is_some_and(|g| g < Duration::days(cfg.fresh_days_in_guild))
        || account_age < Duration::days(cfg.fresh_account_days);
    if f {
        Tier::Fresh
    } else {
        Tier::Established
    }
}

/// Order of operations: time-based `base_tier` → Trusted early return →
/// **downgrade rule** (Established must be earned by history) → escalation
/// (one step toward Quarantine from strikes / recent flags / join flag / risky
/// first message).
///
/// Downgrade rule: an otherwise-Established member with fewer than
/// `cfg.graduation_clean_messages` clean scanned messages stays Fresh (and thus
/// ungraduated, fully scanned). Scammers park "sleeper" alts in servers to age
/// past the time gates; time alone must never grant the cheap sampled tier. This
/// also covers missing `joined_at` (which falls back to account age): without
/// history the member is Fresh, not Established. Fail-safe: history DB errors
/// yield `MemberHistory::default()` (clean_scans = 0), which downgrades — the
/// safe direction.
pub fn compute_tier(
    p: &MemberProfile,
    h: &MemberHistory,
    hints: TierHints,
    cfg: &TierConfig,
    now: DateTime<Utc>,
) -> Tier {
    let base = base_tier(p, cfg, now);
    if base == Tier::Trusted {
        return base;
    }
    let base = if base == Tier::Established && h.clean_scans < cfg.graduation_clean_messages { Tier::Fresh } else { base };
    let escalate = h.active_strikes > 0
        || h.flagged_recently
        || h.join_flag
        || hints.risky_first_message;
    if escalate {
        base.escalate()
    } else {
        base
    }
}

/// Fresh members with enough clean scanned messages move from "scan every
/// message" to sampling. Does not change the tier label. Note the same
/// threshold (`graduation_clean_messages`) also gates reaching the Established
/// tier at all — see `compute_tier`.
pub fn graduated(tier: Tier, h: &MemberHistory, cfg: &TierConfig) -> bool {
    tier == Tier::Fresh && h.clean_scans >= cfg.graduation_clean_messages
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub providers: bool,
    pub ocr: bool,
}

impl Plan {
    pub const NONE: Plan = Plan { providers: false, ocr: false };
    pub const ALL: Plan = Plan { providers: true, ocr: true };
}

/// `roll` is a uniform sample in [0,1) supplied by the caller (testable).
/// `weak_image_message` is an image-only message with a link, mention or very
/// short text.
pub fn detection_plan(
    tier: Tier,
    graduated: bool,
    local_score: f64,
    weak_image_message: bool,
    roll: f64,
    cfg: &PlanConfig,
) -> Plan {
    let hit = local_score >= cfg.local_hit_threshold;
    match tier {
        Tier::Trusted => Plan::NONE,
        Tier::Quarantine => Plan::ALL,
        Tier::Fresh if !graduated => Plan::ALL,
        Tier::Fresh => {
            let on = hit || roll < cfg.fresh_sample_rate;
            Plan { providers: on, ocr: on }
        }
        Tier::Established => Plan {
            providers: hit || roll < cfg.established_sample_rate,
            ocr: hit || weak_image_message || roll < cfg.established_sample_rate,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 14, 12, 0, 0).unwrap()
    }

    fn profile(account_age: Duration, in_guild: Duration) -> MemberProfile {
        MemberProfile {
            guild_id: 1,
            user_id: 2,
            account_created: now() - account_age,
            joined_at: Some(now() - in_guild),
            is_trusted: false,
            has_default_avatar: false,
            role_count: 0,
        }
    }

    fn tier(p: &MemberProfile, h: &MemberHistory, hints: TierHints) -> Tier {
        compute_tier(p, h, hints, &TierConfig::default(), now())
    }

    #[test]
    fn old_account_just_joined_is_quarantine() {
        let p = profile(Duration::days(2000), Duration::minutes(5));
        assert_eq!(tier(&p, &Default::default(), Default::default()), Tier::Quarantine);
    }

    #[test]
    fn brand_new_account_in_guild_a_day_is_quarantine_until_24h() {
        let p = profile(Duration::hours(23), Duration::days(1));
        assert_eq!(tier(&p, &Default::default(), Default::default()), Tier::Quarantine);
        let p = profile(Duration::hours(25), Duration::days(1));
        assert_eq!(tier(&p, &Default::default(), Default::default()), Tier::Fresh);
    }

    #[test]
    fn boundaries() {
        // exactly 30 minutes in guild is no longer quarantine
        let p = profile(Duration::days(1000), Duration::minutes(30));
        assert_eq!(tier(&p, &Default::default(), Default::default()), Tier::Fresh);
        // exactly 7 days in guild, old account + earned history -> established
        let p = profile(Duration::days(1000), Duration::days(7));
        let h = MemberHistory { clean_scans: 10, ..Default::default() };
        assert_eq!(tier(&p, &h, Default::default()), Tier::Established);
        // account exactly 30 days, in guild a long time + earned history -> established
        let p = profile(Duration::days(30), Duration::days(100));
        assert_eq!(tier(&p, &h, Default::default()), Tier::Established);
    }

    #[test]
    fn trusted_never_escalates() {
        let mut p = profile(Duration::days(1), Duration::minutes(1));
        p.is_trusted = true;
        let h = MemberHistory { active_strikes: 3, ..Default::default() };
        assert_eq!(tier(&p, &h, TierHints { risky_first_message: true }), Tier::Trusted);
    }

    #[test]
    fn modifiers_escalate_one_step() {
        let p = profile(Duration::days(1000), Duration::days(100));
        // with enough clean history the member is Established; modifiers drop one step
        let clean = MemberHistory { clean_scans: 10, ..Default::default() };
        assert_eq!(tier(&p, &clean, Default::default()), Tier::Established);
        let h = MemberHistory { active_strikes: 1, clean_scans: 10, ..Default::default() };
        assert_eq!(tier(&p, &h, Default::default()), Tier::Fresh);
        let h = MemberHistory { flagged_recently: true, clean_scans: 10, ..Default::default() };
        assert_eq!(tier(&p, &h, Default::default()), Tier::Fresh);
        let hints = TierHints { risky_first_message: true };
        assert_eq!(tier(&p, &clean, hints), Tier::Fresh);
        // multiple modifiers still only one step
        let h = MemberHistory { active_strikes: 1, join_flag: true, clean_scans: 10, ..Default::default() };
        assert_eq!(tier(&p, &h, hints), Tier::Fresh);
    }

    #[test]
    fn established_must_be_earned_by_history() {
        // (a) aged account, years in the guild, zero clean scans: a sleeper alt stays Fresh
        let p = profile(Duration::days(2000), Duration::days(365));
        assert_eq!(tier(&p, &Default::default(), Default::default()), Tier::Fresh);
        // (b) same member once they reach the graduation threshold: Established
        let h = MemberHistory { clean_scans: 10, ..Default::default() };
        assert_eq!(tier(&p, &h, Default::default()), Tier::Established);
        // (e) one short of the threshold: still Fresh
        let h = MemberHistory { clean_scans: 9, ..Default::default() };
        assert_eq!(tier(&p, &h, Default::default()), Tier::Fresh);
        // (c) sleeper alt with an active strike: downgrade to Fresh, then escalate to Quarantine
        let h = MemberHistory { active_strikes: 1, ..Default::default() };
        assert_eq!(tier(&p, &h, Default::default()), Tier::Quarantine);
        // (d) Trusted is unaffected by the downgrade rule
        let mut t = profile(Duration::days(2000), Duration::days(365));
        t.is_trusted = true;
        assert_eq!(tier(&t, &Default::default(), Default::default()), Tier::Trusted);
    }

    #[test]
    fn missing_joined_at_falls_back_to_account_age() {
        let mut p = profile(Duration::days(1000), Duration::minutes(1));
        p.joined_at = None;
        // without earned history the account age alone lands at Fresh, not Established
        assert_eq!(tier(&p, &Default::default(), Default::default()), Tier::Fresh);
        let h = MemberHistory { clean_scans: 10, ..Default::default() };
        assert_eq!(tier(&p, &h, Default::default()), Tier::Established);
    }

    #[test]
    fn graduation() {
        let cfg = TierConfig::default();
        let h = MemberHistory { clean_scans: 10, ..Default::default() };
        assert!(graduated(Tier::Fresh, &h, &cfg));
        assert!(!graduated(Tier::Quarantine, &h, &cfg));
        let h = MemberHistory { clean_scans: 9, ..Default::default() };
        assert!(!graduated(Tier::Fresh, &h, &cfg));
    }

    #[test]
    fn plans() {
        let c = PlanConfig::default();
        assert_eq!(detection_plan(Tier::Trusted, false, 1.0, true, 0.0, &c), Plan::NONE);
        assert_eq!(detection_plan(Tier::Quarantine, false, 0.0, false, 0.99, &c), Plan::ALL);
        assert_eq!(detection_plan(Tier::Fresh, false, 0.0, false, 0.99, &c), Plan::ALL);
        // graduated fresh: sampled
        assert_eq!(detection_plan(Tier::Fresh, true, 0.0, false, 0.99, &c), Plan::NONE);
        assert_eq!(detection_plan(Tier::Fresh, true, 0.0, false, 0.10, &c), Plan::ALL);
        assert_eq!(detection_plan(Tier::Fresh, true, 0.4, false, 0.99, &c), Plan::ALL);
        // established
        assert_eq!(detection_plan(Tier::Established, false, 0.0, false, 0.99, &c), Plan::NONE);
        assert_eq!(detection_plan(Tier::Established, false, 0.3, false, 0.99, &c), Plan::ALL);
        assert_eq!(detection_plan(Tier::Established, false, 0.0, false, 0.01, &c), Plan::ALL);
        // weak image message: OCR only
        assert_eq!(
            detection_plan(Tier::Established, false, 0.0, true, 0.99, &c),
            Plan { providers: false, ocr: true }
        );
    }
}
