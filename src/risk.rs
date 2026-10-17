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
/// message" to sampling. Does not change the tier label.
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
        // exactly 7 days in guild, old account -> established
        let p = profile(Duration::days(1000), Duration::days(7));
        assert_eq!(tier(&p, &Default::default(), Default::default()), Tier::Established);
        // account exactly 30 days, in guild a long time -> established
        let p = profile(Duration::days(30), Duration::days(100));
        assert_eq!(tier(&p, &Default::default(), Default::default()), Tier::Established);
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
        assert_eq!(tier(&p, &Default::default(), Default::default()), Tier::Established);
        let h = MemberHistory { active_strikes: 1, ..Default::default() };
        assert_eq!(tier(&p, &h, Default::default()), Tier::Fresh);
        let h = MemberHistory { flagged_recently: true, ..Default::default() };
        assert_eq!(tier(&p, &h, Default::default()), Tier::Fresh);
        let hints = TierHints { risky_first_message: true };
        assert_eq!(tier(&p, &Default::default(), hints), Tier::Fresh);
        // multiple modifiers still only one step
        let h = MemberHistory { active_strikes: 1, join_flag: true, ..Default::default() };
        assert_eq!(tier(&p, &h, hints), Tier::Fresh);
    }

    #[test]
    fn missing_joined_at_falls_back_to_account_age() {
        let mut p = profile(Duration::days(1000), Duration::minutes(1));
        p.joined_at = None;
        assert_eq!(tier(&p, &Default::default(), Default::default()), Tier::Established);
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
