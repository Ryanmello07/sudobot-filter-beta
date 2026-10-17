//! Live API quality checks against the real Paxmod, Jev (TypeSafe) and OCR.space
//! endpoints. These are NOT part of the default test run.
//!
//! How to run:
//!   1. `cp .env.example .env` and fill in real keys (PAXMOD_API_KEY,
//!      TYPESAFE_API_KEY, OCR_API_KEY; DISCORD_TOKEN is not needed here).
//!   2. `cargo test --test live_api -- --ignored --nocapture`
//!      (add a filter, e.g. `live_paxmod`, to run just one).
//!
//! WARNING: every run consumes real API quota and needs network access. Each test
//! early-returns (passes) with a SKIP message when its provider's key is missing.
//! Actual scores and latencies are printed; use --nocapture to see them.

use std::sync::Once;
use std::time::{Duration, Instant};
use sudobot_chat_guard::config::Env;
use sudobot_chat_guard::fusion::{jev_risks, paxmod_risks};
use sudobot_chat_guard::providers::jev::{JevClient, JevState};
use sudobot_chat_guard::providers::ocr::OcrClient;
use sudobot_chat_guard::providers::paxmod::PaxmodClient;

static INIT: Once = Once::new();

/// Load .env (if present) and build the real Env. Returns None only if Env itself
/// fails to parse; individual key checks happen per test.
fn env() -> Option<Env> {
    INIT.call_once(|| {
        let _ = dotenvy::dotenv();
        // Env::from_env requires DISCORD_TOKEN; it is unused by these tests.
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

fn skip(what: &str) {
    println!("SKIP: {what} key not set; fill .env to run this check");
}

// ---------- Paxmod ----------

#[tokio::test]
#[ignore = "live API: consumes real Paxmod quota"]
async fn live_paxmod_scam_scores_above_benign() {
    let Some(e) = env() else { return };
    if e.paxmod_key.is_empty() {
        return skip("PAXMOD_API_KEY");
    }
    let client = PaxmodClient::new(&e.paxmod_base, e.paxmod_key.clone(), Duration::from_millis(e.provider_timeout_ms.max(10_000)));

    let scam = "FREE NITRO click bit.ly/xxx claim now";
    let benign = "hey, are we still on for the match tomorrow?";
    let borderline = "check out my server, link in bio, we do giveaways sometimes";

    let mut spam_risks = Vec::new();
    for text in [scam, benign, borderline] {
        let t0 = Instant::now();
        let r = client.moderate(text, None, None).await.expect("paxmod request must succeed and parse");
        let latency = t0.elapsed();
        let (spam, abuse) = paxmod_risks(&r);
        println!("paxmod {text:?}: spam={spam:.3} abuse={abuse:.3} flagged={} degraded={} latency={latency:.2?}", r.flagged, r.degraded);
        for (k, v) in &r.categories {
            assert!((0.0..=1.0).contains(v), "category {k} score {v} out of [0,1]");
        }
        assert!(latency < Duration::from_secs(5), "paxmod latency {latency:.2?} >= 5s");
        spam_risks.push(spam);
    }
    assert!(
        spam_risks[0] > spam_risks[1],
        "scam spam-risk {:.3} should exceed benign {:.3}",
        spam_risks[0],
        spam_risks[1]
    );
}

// ---------- Jev / TypeSafe ----------

#[tokio::test]
#[ignore = "live API: consumes real Jev/TypeSafe quota"]
async fn live_jev_scam_scores_above_benign() {
    let Some(e) = env() else { return };
    if e.typesafe_key.is_empty() {
        return skip("TYPESAFE_API_KEY");
    }
    let client = JevClient::new(&e.typesafe_base, e.typesafe_key.clone(), &e.jev_model, Duration::from_millis(e.provider_timeout_ms.max(10_000)));

    let scam = JevState {
        message: "FREE NITRO click bit.ly/xxx claim now".into(),
        account_age_days: 1,
        minutes_in_guild: Some(5),
        has_default_avatar: true,
        role_count: 0,
        ..Default::default()
    };
    let benign = JevState {
        message: "hey, are we still on for the match tomorrow?".into(),
        account_age_days: 900,
        minutes_in_guild: Some(60_000),
        has_default_avatar: false,
        role_count: 3,
        ..Default::default()
    };

    let mut spam_risks = Vec::new();
    for st in [&scam, &benign] {
        let t0 = Instant::now();
        let r = client.assess(st).await.expect("jev request must succeed and parse");
        let latency = t0.elapsed();
        let (spam, abuse) = jev_risks(&r);
        println!(
            "jev {:?}: class={} probs={:?} spam={spam:.3} abuse={abuse:.3} latency={latency:.2?}",
            st.message, r.class, r.class_probs
        );
        assert!(!r.class_probs.is_empty(), "class_probs keys must be present");
        assert!(r.class_probs.values().all(|v| (0.0..=1.0).contains(v)), "class_probs out of [0,1]");
        assert!(latency < Duration::from_secs(5), "jev latency {latency:.2?} >= 5s");
        spam_risks.push(spam);
    }
    assert!(
        spam_risks[0] > spam_risks[1],
        "scam spam-risk {:.3} should exceed benign {:.3}",
        spam_risks[0],
        spam_risks[1]
    );
}

// ---------- OCR.space ----------

#[tokio::test]
#[ignore = "live API: consumes real OCR.space quota"]
async fn live_ocr_reads_nitro_fixture() {
    let Some(e) = env() else { return };
    if e.ocr_key.is_empty() {
        return skip("OCR_API_KEY");
    }
    let client = OcrClient::new(&e.ocr_base, e.ocr_key.clone(), e.ocr_engine, Duration::from_millis(e.ocr_timeout_ms.max(15_000)));
    let png: &[u8] = include_bytes!("fixtures/nitro.png"); // rendered "FREE NITRO"

    let t0 = Instant::now();
    // Ok (not Rejected) means OCRExitCode / IsErroredOnProcessing were handled.
    let text = client.read(png, "image/png").await.expect("ocr request must succeed and parse");
    let latency = t0.elapsed();
    println!("ocr extracted {text:?} in {latency:.2?}");
    assert!(text.to_lowercase().contains("nitro"), "expected NITRO in {text:?}");
    assert!(latency < Duration::from_secs(10), "ocr latency {latency:.2?} >= 10s");
}

// ---------- end-to-end quality gate ----------

/// Small labeled corpus through whichever providers have keys. Prints
/// precision/recall/F1 for a (score >= 0.5) classifier; asserts the benign
/// false-positive rate is zero. Scam recall is informational only.
#[tokio::test]
#[ignore = "live API: consumes real quota on every provider with a key"]
async fn live_quality_gate_small_corpus() {
    const SCAM: &[&str] = &[
        "FREE NITRO click bit.ly/xxx claim now",
        "Congrats! You won a Steam gift card, verify at steam-community.ru/login",
        "dm me your wallet seed to double your crypto, guaranteed profit",
        "join discord.gg/free-nitro-now for a free airdrop, ends today",
        "your account will be suspended, confirm at discord-verify.top within 24h",
    ];
    const BENIGN: &[&str] = &[
        "hey, are we still on for the match tomorrow?",
        "just finished the raid, that boss fight was insane",
        "can someone review my PR when you get a chance?",
        "happy birthday! hope you have a great day",
        "the update notes are up on the blog, patch 14.2 looks good",
    ];

    let Some(e) = env() else { return };
    let pax = (!e.paxmod_key.is_empty()).then(|| {
        PaxmodClient::new(&e.paxmod_base, e.paxmod_key.clone(), Duration::from_millis(e.provider_timeout_ms.max(10_000)))
    });
    let jev = (!e.typesafe_key.is_empty()).then(|| {
        JevClient::new(&e.typesafe_base, e.typesafe_key.clone(), &e.jev_model, Duration::from_millis(e.provider_timeout_ms.max(10_000)))
    });
    if pax.is_none() && jev.is_none() {
        return println!("SKIP: no provider keys set; fill .env to run the quality gate");
    }

    let mut tp = 0u32;
    let mut fp = 0u32;
    let mut fn_ = 0u32;
    let mut tn = 0u32;
    for (text, is_scam) in SCAM.iter().map(|t| (*t, true)).chain(BENIGN.iter().map(|t| (*t, false))) {
        let mut score: f64 = 0.0;
        if let Some(p) = &pax {
            match p.moderate(text, None, None).await {
                Ok(r) => {
                    let (s, a) = paxmod_risks(&r);
                    score = score.max(s.max(a));
                }
                Err(err) => println!("paxmod error on {text:?}: {err}"),
            }
        }
        if let Some(j) = &jev {
            match j.assess(&JevState { message: text.into(), ..Default::default() }).await {
                Ok(r) => {
                    let (s, a) = jev_risks(&r);
                    score = score.max(s.max(a));
                }
                Err(err) => println!("jev error on {text:?}: {err}"),
            }
        }
        let predicted = score >= 0.5;
        println!("{} score={score:.3} {text:?}", if is_scam { "scam  " } else { "benign" });
        match (is_scam, predicted) {
            (true, true) => tp += 1,
            (false, true) => fp += 1,
            (true, false) => fn_ += 1,
            (false, false) => tn += 1,
        }
    }

    let precision = if tp + fp > 0 { tp as f64 / (tp + fp) as f64 } else { f64::NAN };
    let recall = if tp + fn_ > 0 { tp as f64 / (tp + fn_) as f64 } else { f64::NAN };
    let f1 = if precision + recall > 0.0 { 2.0 * precision * recall / (precision + recall) } else { f64::NAN };
    let fpr = fp as f64 / BENIGN.len() as f64;
    println!("quality gate: TP={tp} FP={fp} FN={fn_} TN={tn} | precision={precision:.2} recall={recall:.2} F1={f1:.2} FPR={fpr:.2}");
    assert_eq!(fp, 0, "benign false-positive rate must be 0 (FPR {fpr:.2})");
}
