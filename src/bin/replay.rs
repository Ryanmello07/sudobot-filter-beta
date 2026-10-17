//! Replay harness: run a labeled JSONL file through the pipeline in Observe mode and
//! print precision/recall and the action distribution per tier. Use it to tune
//! thresholds before enabling Enforce.
//!
//!   replay samples/replay-sample.jsonl [--local-only]
//!
//! Each line: {"text": "...", "label": "spam"|"ham", "account_age_hours": 5,
//!             "minutes_in_guild": 3, "user": 123}   (user is optional)
//!
//! Without --local-only the real Paxmod and Jev APIs are called (keys from .env) and
//! count against your quota. OCR is not exercised (text only).

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use sudobot_chat_guard::budget::Budget;
use sudobot_chat_guard::config::{Config, Env};
use sudobot_chat_guard::model::*;
use sudobot_chat_guard::pipeline::{Budgets, Deps, IncomingMessage, Pipeline};
use sudobot_chat_guard::ports::*;
use sudobot_chat_guard::providers::jev::{JevClient, JevState};
use sudobot_chat_guard::providers::paxmod::PaxmodClient;
use sudobot_chat_guard::providers::ProviderError;
use sudobot_chat_guard::store::Store;

#[derive(Deserialize)]
struct Row {
    text: String,
    label: String,
    #[serde(default = "d_age")]
    account_age_hours: i64,
    #[serde(default = "d_guild")]
    minutes_in_guild: i64,
    user: Option<u64>,
}
fn d_age() -> i64 {
    2
}
fn d_guild() -> i64 {
    5
}

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
#[async_trait]
impl ActionSink for Offline {
    async fn can_act(&self, _: u64, _: u64) -> Result<(), String> {
        Err("replay".into())
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
#[async_trait]
impl ModLog for Offline {
    async fn incident(&self, _: &IncidentReport) {}
    async fn notice(&self, _: u64, _: &str) {}
}

#[derive(Default)]
struct Stats {
    tp: u32,
    fp: u32,
    fn_: u32,
    tn: u32,
    actions: BTreeMap<&'static str, u32>,
}

impl Stats {
    fn add(&mut self, spam: bool, predicted: bool, action: Action) {
        match (spam, predicted) {
            (true, true) => self.tp += 1,
            (false, true) => self.fp += 1,
            (true, false) => self.fn_ += 1,
            (false, false) => self.tn += 1,
        }
        *self.actions.entry(action.name()).or_default() += 1;
    }
    fn line(&self) -> String {
        let p = if self.tp + self.fp > 0 { self.tp as f64 / (self.tp + self.fp) as f64 } else { f64::NAN };
        let r = if self.tp + self.fn_ > 0 { self.tp as f64 / (self.tp + self.fn_) as f64 } else { f64::NAN };
        format!("TP {} FP {} FN {} TN {} | precision {:.2} recall {:.2} | actions {:?}", self.tp, self.fp, self.fn_, self.tn, p, r, self.actions)
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.iter().find(|a| !a.starts_with("--")) else {
        anyhow::bail!("usage: replay <file.jsonl> [--local-only]");
    };
    let local_only = args.iter().any(|a| a == "--local-only");

    let rows: Vec<Row> = std::fs::read_to_string(path)?
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
        .map(|(i, l)| serde_json::from_str(l).map_err(|e| anyhow::anyhow!("line {}: {e}", i + 1)))
        .collect::<Result<_, _>>()?;

    let cfg = Arc::new(Config::load(&std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.toml".into()))?);
    let big = || Arc::new(Budget::new(u64::MAX / 4, 0.0));
    let offline = Arc::new(Offline);
    let (pax, jev): (Arc<dyn TextModerator>, Arc<dyn Classifier>) = if local_only {
        (offline.clone(), offline.clone())
    } else {
        std::env::set_var("DISCORD_TOKEN", std::env::var("DISCORD_TOKEN").unwrap_or_else(|_| "unused".into()));
        let env = Env::from_env()?;
        let t = std::time::Duration::from_millis(env.provider_timeout_ms);
        (
            Arc::new(PaxmodClient::new(&env.paxmod_base, env.paxmod_key.clone(), t)),
            Arc::new(JevClient::new(&env.typesafe_base, env.typesafe_key.clone(), &env.jev_model, t)),
        )
    };
    let pipeline = Pipeline::new(Deps {
        cfg,
        store: Store::memory().await?,
        paxmod: pax,
        jev,
        ocr: offline.clone(),
        fetcher: offline.clone(),
        sink: offline.clone(),
        modlog: offline.clone(),
        budgets: Budgets { paxmod: big(), jev: big(), ocr: big() },
        ocr_max_upload: 1_000_000,
        roll: Arc::new(rand::random::<f64>),
    });

    println!("replaying {} message(s) in Observe mode{}", rows.len(), if local_only { " (local rules only)" } else { "" });
    let mut overall = Stats::default();
    let mut per_tier: BTreeMap<&'static str, Stats> = BTreeMap::new();
    let now = Utc::now();
    for (i, r) in rows.iter().enumerate() {
        let user = r.user.unwrap_or(1_000_000 + i as u64);
        let m = IncomingMessage {
            guild_id: 1,
            channel_id: 10,
            message_id: 5_000_000 + i as u64,
            author_id: user,
            content: r.text.clone(),
            created_at: now,
            image_urls: vec![],
            distinct_mentions: r.text.matches("<@").count(),
            mentions_everyone_text: r.text.contains("@everyone") || r.text.contains("@here"),
            can_mention_everyone: false,
            is_edit: false,
            profile: MemberProfile {
                guild_id: 1,
                user_id: user,
                account_created: now - Duration::hours(r.account_age_hours),
                joined_at: Some(now - Duration::minutes(r.minutes_in_guild)),
                is_trusted: false,
                has_default_avatar: false,
                role_count: 0,
            },
        };
        let o = pipeline.handle(m, now).await;
        let spam = r.label == "spam";
        let predicted = o.planned >= Action::Delete;
        overall.add(spam, predicted, o.planned);
        per_tier.entry(o.tier.name()).or_default().add(spam, predicted, o.planned);
        if spam != predicted {
            println!(
                "  {} [{}] score {:.2}: {}",
                if spam { "MISSED " } else { "FALSE+ " },
                o.tier.name(),
                o.verdict.as_ref().map(|v| v.score).unwrap_or(0.0),
                r.text.chars().take(80).collect::<String>()
            );
        }
    }
    println!("\n(positive = planned action of delete or stronger)");
    println!("overall: {}", overall.line());
    for (t, s) in per_tier {
        println!("{t:>12}: {}", s.line());
    }
    Ok(())
}
