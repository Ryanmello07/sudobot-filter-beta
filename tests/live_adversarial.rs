//! Live adversarial-corpus quality check against the real Paxmod and Jev (TypeSafe)
//! endpoints. NOT part of the default test run.
//!
//! How to run:
//!   1. `cp .env.example .env` and fill in PAXMOD_API_KEY and/or TYPESAFE_API_KEY.
//!   2. `cargo test --test live_adversarial -- --ignored --nocapture`
//!
//! The corpus lives at `tests/fixtures/adversarial-corpus.jsonl`, one JSON object
//! per line: {"text": "...", "label": "scam"|"benign", "category": "...", "technique": "..."}.
//! Extend it with new evasion samples as they are seen in the wild.
//!
//! WARNING: this consumes real API quota (one call per sample per keyed provider)
//! and needs network access. The test passes (with a SKIP message) when the corpus
//! file is missing or no provider key is set. Detection rates are printed for
//! information only; the hard asserts are parseable responses and scores in [0,1].

use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Once;
use std::time::Duration;
use sudobot_chat_guard::config::Env;
use sudobot_chat_guard::fusion::{jev_risks, paxmod_risks};
use sudobot_chat_guard::providers::jev::{JevClient, JevState};
use sudobot_chat_guard::providers::paxmod::PaxmodClient;

const CORPUS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/adversarial-corpus.jsonl");
const CALL_GAP: Duration = Duration::from_millis(150);

static INIT: Once = Once::new();

/// Load .env (if present) and build the real Env, mirroring tests/live_api.rs.
fn env() -> Option<Env> {
    INIT.call_once(|| {
        let _ = dotenvy::dotenv();
        // Env::from_env requires DISCORD_TOKEN; it is unused by this test.
        if std::env::var("DISCORD_TOKEN").is_err() {
            std::env::set_var("DISCORD_TOKEN", "unused");
        }
    });
    match Env::from_env() {
        Ok(e) => Some(e),
        Err(e) => {
            println!("SKIP: could not load environment: {e}");
            None
        }
    }
}

#[derive(Debug, Deserialize)]
struct Sample {
    text: String,
    label: String,
    category: String,
    technique: String,
}

/// Make control/invisible characters visible for the per-sample log lines.
fn escape_visible(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars().take(60) {
        match c {
            '\u{200B}' => out.push_str("<ZWSP>"),
            '\u{200C}' => out.push_str("<ZWNJ>"),
            '\u{200D}' => out.push_str("<ZWJ>"),
            '\u{FEFF}' => out.push_str("<BOM>"),
            '\u{00AD}' => out.push_str("<SHY>"),
            '\u{202A}'..='\u{202E}' => out.push_str("<BIDI>"),
            '\u{2060}'..='\u{2064}' => out.push_str("<WJ>"),
            c if c.is_control() => out.push_str(&format!("<{:02X}>", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[derive(Default)]
struct Cell {
    n: u32,
    detected: u32,
}

#[tokio::test]
#[ignore = "live API: consumes real Paxmod/Jev quota"]
async fn live_adversarial_corpus_detection_rates() {
    let raw = match std::fs::read_to_string(CORPUS_PATH) {
        Ok(s) => s,
        Err(e) => return println!("SKIP: corpus {} unreadable: {e}", CORPUS_PATH),
    };
    let samples: Vec<Sample> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| serde_json::from_str(l).unwrap_or_else(|e| panic!("corpus line {}: {e}", i + 1)))
        .collect();
    if samples.is_empty() {
        return println!("SKIP: corpus {} is empty", CORPUS_PATH);
    }

    let Some(e) = env() else { return };
    let pax = (!e.paxmod_key.is_empty()).then(|| {
        PaxmodClient::new(&e.paxmod_base, e.paxmod_key.clone(), Duration::from_millis(e.provider_timeout_ms.max(10_000)))
    });
    let jev = (!e.typesafe_key.is_empty()).then(|| {
        JevClient::new(&e.typesafe_base, e.typesafe_key.clone(), &e.jev_model, Duration::from_millis(e.provider_timeout_ms.max(10_000)))
    });
    if pax.is_none() && jev.is_none() {
        return println!("SKIP: no provider keys set; fill .env to run the adversarial corpus");
    }

    println!(
        "running {} sample(s) through {}{}",
        samples.len(),
        if pax.is_some() { "paxmod " } else { "" },
        if jev.is_some() { "jev" } else { "" }
    );

    let mut table: BTreeMap<(String, String), Cell> = BTreeMap::new(); // (label, category) -> cell
    let (mut tp, mut fp, mut fn_, mut tn) = (0u32, 0u32, 0u32, 0u32);
    let mut calls_made = 0u32;
    for s in &samples {
        let scam = s.label == "scam";
        let mut score: f64 = 0.0;

        if let Some(p) = &pax {
            if calls_made > 0 {
                tokio::time::sleep(CALL_GAP).await; // rate-limit courtesy
            }
            calls_made += 1;
            let r = p
                .moderate(&s.text, None, None)
                .await
                .unwrap_or_else(|e| panic!("paxmod failed to give a parseable response for {:?}: {e}", s.text));
            for v in r.categories.values() {
                assert!((0.0..=1.0).contains(v), "paxmod score {v} out of [0,1] for {:?}", s.text);
            }
            let (sp, ab) = paxmod_risks(&r);
            score = score.max(sp.max(ab));
        }

        if let Some(j) = &jev {
            if calls_made > 0 {
                tokio::time::sleep(CALL_GAP).await; // rate-limit courtesy
            }
            calls_made += 1;
            let r = j
                .assess(&JevState { message: s.text.clone(), ..Default::default() })
                .await
                .unwrap_or_else(|e| panic!("jev failed to give a parseable response for {:?}: {e}", s.text));
            for v in r.class_probs.values() {
                assert!((0.0..=1.0).contains(v), "jev score {v} out of [0,1] for {:?}", s.text);
            }
            let (sp, ab) = jev_risks(&r);
            score = score.max(sp.max(ab));
        }

        assert!((0.0..=1.0).contains(&score), "combined risk {score} out of [0,1]");

        let detected = score >= 0.5; // same convention as the live_api quality gate
        println!(
            "{} {:<16} {:<24} score={score:.3} detected={}  {}",
            if scam { "scam  " } else { "benign" },
            s.category,
            s.technique,
            if detected { "yes" } else { "no " },
            escape_visible(&s.text)
        );
        let cell = table.entry((s.label.clone(), s.category.clone())).or_default();
        cell.n += 1;
        cell.detected += detected as u32;
        match (scam, detected) {
            (true, true) => tp += 1,
            (false, true) => fp += 1,
            (true, false) => fn_ += 1,
            (false, false) => tn += 1,
        }
    }

    println!("\nper-category summary (informational):");
    println!("  {:<8} {:<20} {:>4} {:>9} {:>9}", "label", "category", "n", "detected", "rate");
    for ((label, category), c) in &table {
        println!("  {label:<8} {category:<20} {:>4} {:>9} {:>8.0}%", c.n, c.detected, 100.0 * c.detected as f64 / c.n as f64);
    }

    let precision = if tp + fp > 0 { tp as f64 / (tp + fp) as f64 } else { f64::NAN };
    let recall = if tp + fn_ > 0 { tp as f64 / (tp + fn_) as f64 } else { f64::NAN };
    let f1 = if precision + recall > 0.0 { 2.0 * precision * recall / (precision + recall) } else { f64::NAN };
    let fpr = if fp + tn > 0 { fp as f64 / (fp + tn) as f64 } else { f64::NAN };
    println!("\nconfusion matrix: TP={tp} FP={fp} FN={fn_} TN={tn}");
    println!("precision={precision:.2} recall={recall:.2} F1={f1:.2} FPR={fpr:.2}");
    println!("(detection rates are informational; no assertion is made on them)");
}
