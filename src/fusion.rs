//! Fuse local, Paxmod and Jev results into one verdict.

use crate::config::FusionConfig;
use crate::model::*;

#[derive(Debug, Clone, Default)]
pub struct Inputs<'a> {
    pub local: Option<&'a LocalResult>,
    pub paxmod: Option<&'a PaxmodResult>,
    pub jev: Option<&'a JevResult>,
    pub coverage: Coverage,
}

fn cat(p: &PaxmodResult, name: &str) -> f64 {
    p.categories.get(name).copied().unwrap_or(0.0)
}

pub fn paxmod_risks(p: &PaxmodResult) -> (f64, f64) {
    // Standard-mode projects score named categories. Advanced-mode projects return a
    // single "Policy Violation" score for the operator's written policy; treat it as
    // spam/scam risk (the policy text is where "what counts" is defined).
    let spam = if p.ban_hit {
        1.0
    } else {
        cat(p, "Spam").max(cat(p, "Dangerous and Criminal Content")).max(cat(p, "Policy Violation"))
    };
    let abuse = cat(p, "Harassment")
        .max(cat(p, "Hate and Discrimination"))
        .max(cat(p, "Threatening"))
        .max(cat(p, "Personally Identifiable Information"));
    (spam, abuse)
}

pub fn jev_risks(j: &JevResult) -> (f64, f64) {
    let p = |name: &str| j.class_probs.get(name).copied();
    let p_legit = p("legitimate").unwrap_or(if j.class == "legitimate" { 1.0 } else { 0.0 });
    // Spam risk comes from the spam-ish classes. The headline class is the fallback
    // when the response carries no spam-ish probabilities at all. (1 - p_legit) must
    // NOT be used unconditionally: an abuse verdict has low p_legit too, and counting
    // it as spam mislabels pure abuse as spam.
    let class_spam = ["spam", "scam_phishing", "promo_solicitation", "nsfw_lure"].iter().filter_map(|n| p(n)).fold(0.0, f64::max);
    let class_spam = if class_spam > 0.0 {
        class_spam
    } else if matches!(j.class.as_str(), "spam" | "scam_phishing" | "promo_solicitation" | "nsfw_lure") {
        1.0 - p_legit
    } else {
        0.0
    };
    let spam = class_spam.max(0.9 * j.dm_lure.max(j.crypto_giveaway)).max(0.7 * j.evasion).clamp(0.0, 1.0);
    // Symmetric fallback: trust the headline class when other_abuse has no probability.
    let abuse = p("other_abuse")
        .unwrap_or(if j.class == "other_abuse" { 1.0 - p_legit } else { 0.0 })
        .clamp(0.0, 1.0);
    (spam, abuse)
}

/// `clean_below`: scores under this are labeled Clean.
pub fn fuse(inp: &Inputs, cfg: &FusionConfig, clean_below: f64) -> Verdict {
    let mut spam_terms = vec![];
    let mut abuse_terms = vec![];
    let mut agreeing = 0u8;
    let mut evidence = vec![];

    if let Some(l) = inp.local {
        // When no other source contributed, the local score stands on its own:
        // applying w_local would cap local-only verdicts at 0.6, below the quarantine
        // delete threshold — even flagging would be unreachable during a provider
        // outage. Multi-source fusion is unchanged (the weight still applies then).
        let w = if inp.paxmod.is_none() && inp.jev.is_none() { 1.0 } else { cfg.w_local };
        spam_terms.push(w * l.score);
        if l.score >= 0.5 {
            agreeing += 1;
        }
        for s in &l.signals {
            evidence.push(format!("rule:{} ({:.2})", s.name, s.weight));
        }
    }
    if let Some(p) = inp.paxmod {
        let (s, a) = paxmod_risks(p);
        spam_terms.push(cfg.w_paxmod * s);
        abuse_terms.push(cfg.w_paxmod * a);
        if s.max(a) >= 0.5 {
            agreeing += 1;
        }
        let mut top: Vec<_> = p.categories.iter().filter(|(_, v)| **v >= 0.3).collect();
        top.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap());
        for (k, v) in top.into_iter().take(4) {
            evidence.push(format!("paxmod:{k} {v:.2}"));
        }
        if p.ban_hit {
            evidence.push("paxmod:console-ban".into());
        }
        if let Some(r) = p.reason.as_deref().filter(|_| p.flagged) {
            evidence.push(format!("paxmod reason: {}", r.chars().take(160).collect::<String>()));
        }
    }
    if let Some(j) = inp.jev {
        let (s, a) = jev_risks(j);
        spam_terms.push(cfg.w_jev * s);
        abuse_terms.push(cfg.w_jev * a);
        if s.max(a) >= 0.5 {
            agreeing += 1;
        }
        evidence.push(format!(
            "jev:class={} dm={:.2} crypto={:.2} evasion={:.2} bot={:.2}",
            j.class, j.dm_lure, j.crypto_giveaway, j.evasion, j.bot_like
        ));
    }

    let spam_score = noisy_or(spam_terms.into_iter());
    let abuse_score = noisy_or(abuse_terms.into_iter());
    let score = spam_score.max(abuse_score);
    let label = if score < clean_below {
        Label::Clean
    } else if abuse_score > spam_score {
        Label::Abuse
    } else if inp.jev.is_some_and(|j| j.class == "scam_phishing" || j.class == "nsfw_lure") {
        Label::Scam
    } else {
        Label::Spam
    };
    Verdict { score, spam_score, abuse_score, label, agreeing_sources: agreeing, coverage: inp.coverage, evidence }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pax(pairs: &[(&str, f64)]) -> PaxmodResult {
        PaxmodResult { categories: pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect(), ..Default::default() }
    }

    fn jev(class: &str, legit: f64) -> JevResult {
        JevResult {
            class: class.into(),
            class_probs: [("legitimate".to_string(), legit)].into_iter().collect(),
            class_confidence: 0.9,
            ..Default::default()
        }
    }

    fn cfg() -> FusionConfig {
        FusionConfig::default()
    }

    #[test]
    fn nothing_is_clean() {
        let v = fuse(&Inputs::default(), &cfg(), 0.4);
        assert_eq!(v.score, 0.0);
        assert_eq!(v.label, Label::Clean);
        assert_eq!(v.agreeing_sources, 0);
    }

    #[test]
    fn single_source_is_capped_by_weight() {
        let p = pax(&[("Spam", 1.0)]);
        let v = fuse(&Inputs { paxmod: Some(&p), ..Default::default() }, &cfg(), 0.4);
        assert!((v.score - 0.9).abs() < 1e-9);
        assert_eq!(v.agreeing_sources, 1);
        assert_eq!(v.label, Label::Spam);
    }

    #[test]
    fn sources_combine_noisy_or() {
        let p = pax(&[("Spam", 0.8)]);
        let j = jev("spam", 0.1);
        let l = LocalResult::from_signals(vec![Signal { name: "duplicate".into(), weight: 0.6 }]);
        let v = fuse(&Inputs { local: Some(&l), paxmod: Some(&p), jev: Some(&j), ..Default::default() }, &cfg(), 0.4);
        // 1 - (1-0.36)(1-0.72)(1-0.81)
        let expect = 1.0 - (1.0 - 0.36) * (1.0 - 0.72) * (1.0 - 0.81);
        assert!((v.score - expect).abs() < 1e-9, "{} vs {expect}", v.score);
        assert_eq!(v.agreeing_sources, 3);
    }

    #[test]
    fn ban_hit_forces_max_spam_risk() {
        let p = PaxmodResult { ban_hit: true, ..Default::default() };
        let (s, _) = paxmod_risks(&p);
        assert_eq!(s, 1.0);
    }

    #[test]
    fn jev_pushes_dm_lure_and_evasion() {
        let mut j = jev("legitimate", 1.0);
        j.dm_lure = 0.9;
        assert!((jev_risks(&j).0 - 0.81).abs() < 1e-9);
        let mut j = jev("legitimate", 1.0);
        j.evasion = 1.0;
        assert!((jev_risks(&j).0 - 0.7).abs() < 1e-9);
    }

    #[test]
    fn jev_missing_probs_fall_back_to_class() {
        let mut j = JevResult { class: "legitimate".into(), ..Default::default() };
        assert_eq!(jev_risks(&j).0, 0.0);
        j.class = "spam".into();
        assert_eq!(jev_risks(&j).0, 1.0);
    }

    #[test]
    fn abuse_label_and_separate_fusion() {
        let p = pax(&[("Harassment", 0.9), ("Threatening", 0.8)]);
        let v = fuse(&Inputs { paxmod: Some(&p), ..Default::default() }, &cfg(), 0.4);
        assert_eq!(v.label, Label::Abuse);
        assert!(v.abuse_score > v.spam_score);
    }

    #[test]
    fn advanced_mode_policy_violation_counts_as_spam_risk() {
        let p = pax(&[("Policy Violation", 1.0)]);
        let (s, a) = paxmod_risks(&p);
        assert_eq!((s, a), (1.0, 0.0));
    }

    #[test]
    fn threatening_does_not_count_as_spam() {
        let p = pax(&[("Threatening", 0.9)]);
        let (s, a) = paxmod_risks(&p);
        assert_eq!(s, 0.0);
        assert_eq!(a, 0.9);
    }

    #[test]
    fn scam_label_from_jev_class() {
        let j = jev("scam_phishing", 0.05);
        let v = fuse(&Inputs { jev: Some(&j), ..Default::default() }, &cfg(), 0.4);
        assert_eq!(v.label, Label::Scam);
    }

    #[test]
    fn below_threshold_is_clean() {
        let l = LocalResult::from_signals(vec![Signal { name: "x".into(), weight: 0.3 }]);
        let v = fuse(&Inputs { local: Some(&l), ..Default::default() }, &cfg(), 0.4);
        assert_eq!(v.label, Label::Clean);
    }

    #[test]
    fn evidence_lists_rules_and_top_categories() {
        let p = pax(&[("Spam", 0.9), ("Harassment", 0.1)]);
        let l = LocalResult::from_signals(vec![Signal { name: "flood".into(), weight: 0.5 }]);
        let v = fuse(&Inputs { local: Some(&l), paxmod: Some(&p), ..Default::default() }, &cfg(), 0.4);
        assert!(v.evidence.iter().any(|e| e.starts_with("rule:flood")));
        assert!(v.evidence.iter().any(|e| e.starts_with("paxmod:Spam")));
        assert!(!v.evidence.iter().any(|e| e.contains("Harassment")));
    }

    // Regression M1 (fixed): a local-only verdict is no longer capped by w_local —
    // with no other source contributing, the local score fuses at full strength, so
    // flag/delete stay reachable during provider outages. Multi-source math is
    // unchanged (see sources_combine_noisy_or).
    #[test]
    fn m1_local_only_max_score_reaches_quarantine_delete_threshold() {
        let delete = crate::config::LadderConfig::default().quarantine.delete.unwrap();
        // Sanity floor: an idealized local score of exactly 1.0 fuses to w_local.
        let perfect = LocalResult::from_signals(vec![Signal { name: "x".into(), weight: 1.0 }]);
        let v = fuse(&Inputs { local: Some(&perfect), ..Default::default() }, &cfg(), 0.4);
        assert!(v.score >= delete, "perfect local score fuses to {:.6}, below delete {delete}", v.score);
        // The realistic ceiling: every local rule firing at full weight.
        let w = crate::config::RuleWeights::default();
        let maxed = LocalResult::from_signals(vec![
            Signal { name: "flood".into(), weight: w.flood },
            Signal { name: "duplicate".into(), weight: w.duplicate },
            Signal { name: "mass_mention".into(), weight: w.mass_mention },
            Signal { name: "everyone_attempt".into(), weight: w.everyone_attempt },
            Signal { name: "first_message_link".into(), weight: w.first_message_link },
            Signal { name: "invite_link".into(), weight: w.invite_link },
            Signal { name: "blocked_domain".into(), weight: w.blocked_domain },
            Signal { name: "image_text_bait".into(), weight: w.image_text_bait },
        ]);
        let v = fuse(&Inputs { local: Some(&maxed), ..Default::default() }, &cfg(), 0.4);
        assert!(v.score >= delete, "max local score {:.4} fuses to {:.6}, below delete {delete}", maxed.score, v.score);
    }

    // Regression M4 (fixed): a Jev-only other_abuse verdict is labeled Abuse — spam
    // risk is derived from the spam-ish classes (with the headline class as
    // fallback), not from 1 - p_legitimate, which abuse verdicts also depress.
    #[test]
    fn m4_jev_only_other_abuse_is_labeled_abuse() {
        let j = JevResult {
            class: "other_abuse".into(),
            class_probs: [
                ("legitimate".to_string(), 0.05),
                ("other_abuse".to_string(), 0.9),
                ("dm_lure".to_string(), 0.0),
                ("crypto_giveaway".to_string(), 0.0),
                ("evasion".to_string(), 0.0),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let v = fuse(&Inputs { jev: Some(&j), ..Default::default() }, &cfg(), 0.4);
        assert_eq!(v.label, Label::Abuse, "spam {:.3} abuse {:.3}", v.spam_score, v.abuse_score);
    }

    #[test]
    fn jev_spam_comes_from_spam_classes_with_symmetric_fallbacks() {
        // class probabilities present: spam only from spam-ish classes
        let j = JevResult {
            class: "other_abuse".into(),
            class_probs: [("legitimate".to_string(), 0.05), ("other_abuse".to_string(), 0.9), ("spam".to_string(), 0.02)].into_iter().collect(),
            ..Default::default()
        };
        let (s, a) = jev_risks(&j);
        assert!((s - 0.02).abs() < 1e-9, "spam {s}");
        assert!((a - 0.9).abs() < 1e-9, "abuse {a}");
        // headline-class fallback when no spam-ish probabilities are present
        let j = JevResult { class: "scam_phishing".into(), class_probs: [("legitimate".to_string(), 0.1)].into_iter().collect(), ..Default::default() };
        assert!((jev_risks(&j).0 - 0.9).abs() < 1e-9);
        // symmetric abuse fallback
        let j = JevResult { class: "other_abuse".into(), ..Default::default() };
        assert!((jev_risks(&j).1 - 1.0).abs() < 1e-9);
        assert_eq!(jev_risks(&j).0, 0.0);
    }
}
