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
    let p_legit = j
        .class_probs
        .get("legitimate")
        .copied()
        .unwrap_or(if j.class == "legitimate" { 1.0 } else { 0.0 });
    let spam = (1.0 - p_legit)
        .max(0.9 * j.dm_lure.max(j.crypto_giveaway))
        .max(0.7 * j.evasion)
        .clamp(0.0, 1.0);
    let abuse = j.class_probs.get("other_abuse").copied().unwrap_or(0.0).clamp(0.0, 1.0);
    (spam, abuse)
}

/// `clean_below`: scores under this are labeled Clean.
pub fn fuse(inp: &Inputs, cfg: &FusionConfig, clean_below: f64) -> Verdict {
    let mut spam_terms = vec![];
    let mut abuse_terms = vec![];
    let mut agreeing = 0u8;
    let mut evidence = vec![];

    if let Some(l) = inp.local {
        spam_terms.push(cfg.w_local * l.score);
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
}
