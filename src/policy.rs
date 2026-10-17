//! Map (tier, verdict, history) to an action.

use crate::config::{LadderConfig, Rungs};
use crate::model::*;

fn rungs(tier: Tier, l: &LadderConfig) -> Option<&Rungs> {
    match tier {
        Tier::Trusted => None,
        Tier::Quarantine => Some(&l.quarantine),
        Tier::Fresh => Some(&l.fresh),
        Tier::Established => Some(&l.established),
    }
}

/// Scores below this are labeled Clean.
pub fn flag_threshold(tier: Tier, l: &LadderConfig) -> f64 {
    rungs(tier, l).and_then(|r| r.flag.or(r.delete)).unwrap_or(0.5)
}

pub fn decide(tier: Tier, v: &Verdict, h: &MemberHistory, l: &LadderConfig) -> Action {
    let Some(r) = rungs(tier, l) else { return Action::None };
    if v.label == Label::Clean {
        return Action::None;
    }
    let reach = |t: Option<f64>| t.is_some_and(|t| v.score >= t);
    let multi = v.agreeing_sources >= 2 || (l.repeat_offender_single_source && h.active_strikes > 0);
    let severe_label = matches!(v.label, Label::Spam | Label::Scam);

    if l.enable_ban && severe_label && multi && reach(r.ban) {
        return Action::Ban;
    }
    if l.enable_kick && severe_label && multi && reach(r.kick) {
        return Action::Kick;
    }
    if l.enable_timeout && reach(r.timeout) {
        return Action::DeleteTimeout;
    }
    if l.enable_delete && reach(r.delete) {
        return Action::Delete;
    }
    if reach(r.flag) {
        return Action::Flag;
    }
    Action::None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(score: f64, label: Label, agree: u8) -> Verdict {
        Verdict {
            score,
            spam_score: score,
            abuse_score: 0.0,
            label,
            agreeing_sources: agree,
            coverage: Coverage::default(),
            evidence: vec![],
        }
    }

    fn d(tier: Tier, v: &Verdict) -> Action {
        decide(tier, v, &MemberHistory::default(), &LadderConfig::default())
    }

    #[test]
    fn same_score_is_harsher_for_newer_tiers() {
        let v = verdict(0.72, Label::Spam, 1);
        assert_eq!(d(Tier::Quarantine, &v), Action::DeleteTimeout);
        assert_eq!(d(Tier::Fresh, &v), Action::Delete);
        assert_eq!(d(Tier::Established, &v), Action::Flag);
    }

    #[test]
    fn established_low_score_only_logs_or_nothing() {
        assert_eq!(d(Tier::Established, &verdict(0.6, Label::Spam, 1)), Action::None);
        assert_eq!(d(Tier::Established, &verdict(0.66, Label::Spam, 1)), Action::Flag);
    }

    #[test]
    fn trusted_never_actioned() {
        assert_eq!(d(Tier::Trusted, &verdict(1.0, Label::Spam, 3)), Action::None);
    }

    #[test]
    fn clean_label_never_actioned() {
        assert_eq!(d(Tier::Quarantine, &verdict(0.99, Label::Clean, 3)), Action::None);
    }

    #[test]
    fn kick_and_ban_need_two_sources() {
        assert_eq!(d(Tier::Quarantine, &verdict(0.99, Label::Spam, 1)), Action::DeleteTimeout);
        assert_eq!(d(Tier::Quarantine, &verdict(0.99, Label::Spam, 2)), Action::Ban);
        assert_eq!(d(Tier::Quarantine, &verdict(0.90, Label::Scam, 2)), Action::Kick);
    }

    #[test]
    fn established_never_kicked_or_banned() {
        assert_eq!(d(Tier::Established, &verdict(1.0, Label::Spam, 3)), Action::DeleteTimeout);
    }

    #[test]
    fn abuse_caps_at_timeout() {
        assert_eq!(d(Tier::Quarantine, &verdict(0.99, Label::Abuse, 3)), Action::DeleteTimeout);
    }

    #[test]
    fn disabled_rungs_fall_back() {
        let mut l = LadderConfig { enable_ban: false, ..Default::default() };
        let v = verdict(0.99, Label::Spam, 2);
        assert_eq!(decide(Tier::Quarantine, &v, &Default::default(), &l), Action::Kick);
        l.enable_kick = false;
        assert_eq!(decide(Tier::Quarantine, &v, &Default::default(), &l), Action::DeleteTimeout);
        l.enable_timeout = false;
        assert_eq!(decide(Tier::Quarantine, &v, &Default::default(), &l), Action::Delete);
        l.enable_delete = false;
        assert_eq!(decide(Tier::Quarantine, &v, &Default::default(), &l), Action::Flag);
    }

    #[test]
    fn repeat_offender_waiver_is_opt_in() {
        let h = MemberHistory { active_strikes: 1, ..Default::default() };
        let v = verdict(0.99, Label::Spam, 1);
        let mut l = LadderConfig::default();
        assert_eq!(decide(Tier::Quarantine, &v, &h, &l), Action::DeleteTimeout);
        l.repeat_offender_single_source = true;
        assert_eq!(decide(Tier::Quarantine, &v, &h, &l), Action::Ban);
    }
}
