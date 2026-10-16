//! Configuration: `config.toml` for behavior, environment for secrets.

use crate::secret::Secret;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TierConfig {
    pub quarantine_minutes_in_guild: i64,
    pub quarantine_account_hours: i64,
    pub fresh_days_in_guild: i64,
    pub fresh_account_days: i64,
    pub graduation_clean_messages: u32,
}

impl Default for TierConfig {
    fn default() -> Self {
        Self {
            quarantine_minutes_in_guild: 30,
            quarantine_account_hours: 24,
            fresh_days_in_guild: 7,
            fresh_account_days: 30,
            graduation_clean_messages: 10,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RuleWeights {
    pub flood: f64,
    pub duplicate: f64,
    pub mass_mention: f64,
    pub everyone_attempt: f64,
    pub first_message_link: f64,
    pub invite_link: f64,
    pub blocked_domain: f64,
    pub image_text_bait: f64,
}

impl Default for RuleWeights {
    fn default() -> Self {
        Self {
            flood: 0.5,
            duplicate: 0.6,
            mass_mention: 0.7,
            everyone_attempt: 0.6,
            first_message_link: 0.35,
            invite_link: 0.5,
            blocked_domain: 0.8,
            image_text_bait: 0.6,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RuleConfig {
    pub flood_messages: usize,
    pub flood_seconds: i64,
    pub duplicate_window_secs: i64,
    pub duplicate_min_len: usize,
    pub mass_mention_limit: usize,
    pub weights: RuleWeights,
}

impl Default for RuleConfig {
    fn default() -> Self {
        Self {
            flood_messages: 5,
            flood_seconds: 8,
            duplicate_window_secs: 120,
            duplicate_min_len: 8,
            mass_mention_limit: 5,
            weights: RuleWeights::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct FusionConfig {
    pub w_local: f64,
    pub w_paxmod: f64,
    pub w_jev: f64,
}

impl Default for FusionConfig {
    fn default() -> Self {
        Self { w_local: 0.6, w_paxmod: 0.9, w_jev: 0.9 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PlanConfig {
    pub local_hit_threshold: f64,
    pub fresh_sample_rate: f64,
    pub established_sample_rate: f64,
}

impl Default for PlanConfig {
    fn default() -> Self {
        Self { local_hit_threshold: 0.30, fresh_sample_rate: 0.25, established_sample_rate: 0.05 }
    }
}

/// Fused-score thresholds for one tier. `None` disables the rung for that tier.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Rungs {
    pub flag: Option<f64>,
    pub delete: Option<f64>,
    pub timeout: Option<f64>,
    pub kick: Option<f64>,
    pub ban: Option<f64>,
}

impl Rungs {
    fn new(flag: f64, delete: f64, timeout: f64, kick: Option<f64>, ban: Option<f64>) -> Self {
        Self { flag: Some(flag), delete: Some(delete), timeout: Some(timeout), kick, ban }
    }
}

impl Default for Rungs {
    fn default() -> Self {
        Rungs::new(0.5, 0.7, 0.8, Some(0.92), Some(0.97))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LadderConfig {
    pub quarantine: Rungs,
    pub fresh: Rungs,
    pub established: Rungs,
    pub enable_delete: bool,
    pub enable_timeout: bool,
    pub enable_kick: bool,
    pub enable_ban: bool,
    pub timeout_minutes: u32,
    pub ban_delete_seconds: u32,
    pub sweep_minutes: i64,
    pub strike_days: i64,
    pub repeat_offender_single_source: bool,
}

impl Default for LadderConfig {
    fn default() -> Self {
        Self {
            quarantine: Rungs::new(0.40, 0.60, 0.70, Some(0.85), Some(0.95)),
            fresh: Rungs::new(0.50, 0.70, 0.80, Some(0.92), Some(0.97)),
            established: Rungs::new(0.65, 0.85, 0.92, None, None),
            enable_delete: true,
            enable_timeout: true,
            enable_kick: true,
            enable_ban: true,
            timeout_minutes: 60,
            ban_delete_seconds: 3600,
            sweep_minutes: 10,
            strike_days: 30,
            repeat_offender_single_source: false,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct GuildConfig {
    pub log_channel_id: Option<u64>,
    pub trusted_role_ids: Vec<u64>,
    pub allowlisted_user_ids: Vec<u64>,
    pub blocked_domains: Vec<String>,
    pub allowed_invites: Vec<String>,
    pub tiers: TierConfig,
    pub rules: RuleConfig,
    pub fusion: FusionConfig,
    pub plan: PlanConfig,
    pub ladder: LadderConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextFeed {
    Scanned,
    ActiveChannels,
    All,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PaxmodSettings {
    pub context_feed: ContextFeed,
    pub feed_window_minutes: i64,
}

impl Default for PaxmodSettings {
    fn default() -> Self {
        Self { context_feed: ContextFeed::ActiveChannels, feed_window_minutes: 10 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct OcrSettings {
    pub max_images_per_message: usize,
    pub max_download_bytes: usize,
    pub min_text_chars: usize,
    pub cache_ttl_secs: u64,
}

impl Default for OcrSettings {
    fn default() -> Self {
        Self {
            max_images_per_message: 4,
            max_download_bytes: 8 * 1024 * 1024,
            min_text_chars: 4,
            cache_ttl_secs: 3600,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RuntimeSettings {
    pub queue_size: usize,
    pub workers: usize,
    pub retention_days: i64,
    pub jev_cache_ttl_secs: u64,
    pub jev_text_chars: usize,
    pub jev_prior_messages: usize,
}

impl Default for RuntimeSettings {
    fn default() -> Self {
        Self {
            queue_size: 256,
            workers: 8,
            retention_days: 14,
            jev_cache_ttl_secs: 600,
            jev_text_chars: 2000,
            jev_prior_messages: 3,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub paxmod: PaxmodSettings,
    pub ocr: OcrSettings,
    pub runtime: RuntimeSettings,
    defaults: GuildConfig,
    guilds: HashMap<u64, GuildConfig>,
}

impl Config {
    pub fn guild(&self, guild_id: u64) -> &GuildConfig {
        self.guilds.get(&guild_id).unwrap_or(&self.defaults)
    }

    pub fn from_toml_str(s: &str) -> anyhow::Result<Config> {
        let root: toml::Table = toml::from_str(s)?;
        let sub = |k: &str| root.get(k).cloned().unwrap_or(toml::Value::Table(Default::default()));
        let paxmod: PaxmodSettings = sub("paxmod").try_into()?;
        let ocr: OcrSettings = sub("ocr").try_into()?;
        let runtime: RuntimeSettings = sub("runtime").try_into()?;
        let defaults_tbl = match sub("defaults") {
            toml::Value::Table(t) => t,
            _ => anyhow::bail!("[defaults] must be a table"),
        };
        let defaults: GuildConfig = toml::Value::Table(defaults_tbl.clone()).try_into()?;
        let mut guilds = HashMap::new();
        if let Some(toml::Value::Table(g)) = root.get("guild") {
            for (id, v) in g {
                let id: u64 = id.parse().map_err(|_| anyhow::anyhow!("bad guild id {id}"))?;
                let toml::Value::Table(over) = v else {
                    anyhow::bail!("[guild.{id}] must be a table");
                };
                let mut merged = defaults_tbl.clone();
                merge_tables(&mut merged, over);
                guilds.insert(id, toml::Value::Table(merged).try_into()?);
            }
        }
        let cfg = Config { paxmod, ocr, runtime, defaults, guilds };
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn load(path: &str) -> anyhow::Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(s) => Self::from_toml_str(&s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!("config file {path} not found, using defaults");
                Ok(Config::default())
            }
            Err(e) => Err(e.into()),
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!((7..=30).contains(&self.runtime.retention_days), "runtime.retention_days must be 7-30");
        anyhow::ensure!(self.runtime.workers >= 1 && self.runtime.queue_size >= self.runtime.workers, "bad worker/queue sizes");
        for g in std::iter::once(&self.defaults).chain(self.guilds.values()) {
            for r in [&g.ladder.quarantine, &g.ladder.fresh, &g.ladder.established] {
                for v in [r.flag, r.delete, r.timeout, r.kick, r.ban].into_iter().flatten() {
                    anyhow::ensure!((0.0..=1.0).contains(&v), "ladder threshold {v} out of range");
                }
            }
        }
        Ok(())
    }
}

fn merge_tables(base: &mut toml::Table, over: &toml::Table) {
    for (k, v) in over {
        match (base.get_mut(k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge_tables(b, o),
            _ => {
                base.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Secrets and operational values from the environment.
#[derive(Debug, Clone)]
pub struct Env {
    pub discord_token: Secret,
    pub paxmod_key: Secret,
    pub paxmod_base: String,
    pub paxmod_cap: u64,
    pub typesafe_key: Secret,
    pub typesafe_base: String,
    pub jev_model: String,
    pub jev_cap: u64,
    pub ocr_key: Secret,
    pub ocr_base: String,
    pub ocr_engine: u8,
    pub ocr_cap: u64,
    pub ocr_timeout_ms: u64,
    pub ocr_max_upload_bytes: usize,
    pub provider_timeout_ms: u64,
    pub reserved_share: f64,
    pub database_url: String,
    pub config_path: String,
}

fn var(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}

fn num<T: std::str::FromStr>(name: &str, default: T) -> anyhow::Result<T> {
    match std::env::var(name) {
        Ok(v) => v.parse().map_err(|_| anyhow::anyhow!("{name} is not a valid number")),
        Err(_) => Ok(default),
    }
}

impl Env {
    pub fn from_env() -> anyhow::Result<Env> {
        let discord_token = Secret::new(var("DISCORD_TOKEN", ""));
        anyhow::ensure!(!discord_token.is_empty(), "DISCORD_TOKEN is required");
        let reserved_share: f64 = num("BUDGET_RESERVED_SHARE", 0.5)?;
        anyhow::ensure!((0.0..=1.0).contains(&reserved_share), "BUDGET_RESERVED_SHARE must be 0-1");
        Ok(Env {
            discord_token,
            paxmod_key: Secret::new(var("PAXMOD_API_KEY", "")),
            paxmod_base: var("PAXMOD_BASE_URL", "https://www.paxmod.com/api/v1"),
            paxmod_cap: num("PAXMOD_DAILY_CAP", 2000)?,
            typesafe_key: Secret::new(var("TYPESAFE_API_KEY", "")),
            typesafe_base: var("TYPESAFE_BASE_URL", "https://api.typesafe.ai/v1"),
            jev_model: var("JEV_MODEL", "jev-latest"),
            jev_cap: num("JEV_DAILY_CAP", 2000)?,
            ocr_key: Secret::new(var("OCR_API_KEY", "")),
            ocr_base: var("OCR_BASE_URL", "https://api.ocr.space"),
            ocr_engine: num("OCR_ENGINE", 2)?,
            ocr_cap: num("OCR_DAILY_CAP", 400)?,
            ocr_timeout_ms: num("OCR_TIMEOUT_MS", 4000)?,
            ocr_max_upload_bytes: num("OCR_MAX_UPLOAD_BYTES", 1_000_000)?,
            provider_timeout_ms: num("PROVIDER_TIMEOUT_MS", 1500)?,
            reserved_share,
            database_url: var("DATABASE_URL", "sqlite://data/guard.db"),
            config_path: var("CONFIG_PATH", "config.toml"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_gives_defaults() {
        let c = Config::from_toml_str("").unwrap();
        assert_eq!(c.guild(1).tiers.quarantine_minutes_in_guild, 30);
        assert_eq!(c.paxmod.context_feed, ContextFeed::ActiveChannels);
    }

    #[test]
    fn guild_override_merges_deeply() {
        let c = Config::from_toml_str(
            r#"
            [defaults.tiers]
            fresh_days_in_guild = 5
            [defaults.ladder]
            timeout_minutes = 30
            [guild.42.ladder]
            timeout_minutes = 10
            [guild.42]
            log_channel_id = 99
            "#,
        )
        .unwrap();
        assert_eq!(c.guild(42).ladder.timeout_minutes, 10);
        assert_eq!(c.guild(42).tiers.fresh_days_in_guild, 5);
        assert_eq!(c.guild(42).log_channel_id, Some(99));
        assert_eq!(c.guild(7).ladder.timeout_minutes, 30);
        assert_eq!(c.guild(7).log_channel_id, None);
    }

    #[test]
    fn rejects_out_of_range_threshold() {
        let e = Config::from_toml_str("[defaults.ladder.fresh]\ndelete = 1.5\n");
        assert!(e.is_err());
    }

    #[test]
    fn rejects_bad_retention() {
        assert!(Config::from_toml_str("[runtime]\nretention_days = 90\n").is_err());
    }

    #[test]
    fn context_feed_parses() {
        let c = Config::from_toml_str("[paxmod]\ncontext_feed = \"all\"\n").unwrap();
        assert_eq!(c.paxmod.context_feed, ContextFeed::All);
    }
}
