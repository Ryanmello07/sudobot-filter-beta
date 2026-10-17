//! Configuration: `config.toml` for behavior, environment for secrets.

use crate::model::Mode;
use crate::secret::Secret;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
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
/// No container-level serde default: omitted rungs must stay `None` (disabled),
/// not inherit `Rungs::default()`'s values.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rungs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flag: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delete: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kick: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
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

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct GuildConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
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
            validate_guild(g)?;
        }
        Ok(())
    }

    /// The effective config for a guild: [defaults] merged with [guild.<id>] (done at
    /// load) further deep-merged with the guild's DB override doc. A ladder rung set
    /// to the string "off" in the override doc disables that rung (TOML has no null).
    pub fn apply_override(&self, guild: u64, doc: &str) -> anyhow::Result<GuildConfig> {
        let mut over: toml::Table = toml::from_str(doc).map_err(|e| anyhow::anyhow!("override doc does not parse: {e}"))?;
        let disabled = take_disabled_rungs(&mut over);
        let base = toml::Value::try_from(self.guild(guild)).map_err(|e| anyhow::anyhow!("cannot serialize static config: {e}"))?;
        let toml::Value::Table(mut base) = base else { anyhow::bail!("static config is not a table") };
        merge_tables(&mut base, &over);
        let mut g: GuildConfig = toml::Value::Table(base).try_into().map_err(|e| anyhow::anyhow!("override is invalid: {e}"))?;
        for (tier, rung) in disabled {
            let r = match tier.as_str() {
                "quarantine" => &mut g.ladder.quarantine,
                "fresh" => &mut g.ladder.fresh,
                _ => &mut g.ladder.established,
            };
            match rung.as_str() {
                "flag" => r.flag = None,
                "delete" => r.delete = None,
                "timeout" => r.timeout = None,
                "kick" => r.kick = None,
                _ => r.ban = None,
            }
        }
        validate_guild(&g)?;
        Ok(g)
    }
}

fn validate_guild(g: &GuildConfig) -> anyhow::Result<()> {
    for r in [&g.ladder.quarantine, &g.ladder.fresh, &g.ladder.established] {
        for v in [r.flag, r.delete, r.timeout, r.kick, r.ban].into_iter().flatten() {
            anyhow::ensure!((0.0..=1.0).contains(&v), "ladder threshold {v} out of range");
        }
    }
    for (name, v) in [
        ("plan.local_hit_threshold", g.plan.local_hit_threshold),
        ("plan.fresh_sample_rate", g.plan.fresh_sample_rate),
        ("plan.established_sample_rate", g.plan.established_sample_rate),
        ("fusion.w_local", g.fusion.w_local),
        ("fusion.w_paxmod", g.fusion.w_paxmod),
        ("fusion.w_jev", g.fusion.w_jev),
    ] {
        anyhow::ensure!((0.0..=1.0).contains(&v), "{name} must be between 0 and 1");
    }
    Ok(())
}

/// Pull "off" sentinels out of ladder.<tier>.<rung> entries before merging; the
/// caller turns them into `None` rungs after deserialization.
fn take_disabled_rungs(over: &mut toml::Table) -> Vec<(String, String)> {
    let mut out = vec![];
    let Some(toml::Value::Table(ladder)) = over.get_mut("ladder") else { return out };
    for (tier, tv) in ladder.iter_mut() {
        let toml::Value::Table(tt) = tv else { continue };
        let off: Vec<String> = tt
            .iter()
            .filter(|(_, v)| matches!(v, toml::Value::String(s) if s == RUNG_DISABLED))
            .map(|(k, _)| k.clone())
            .collect();
        for rung in off {
            tt.remove(&rung);
            out.push((tier.clone(), rung));
        }
    }
    out
}

// ---------- runtime override editing (`/guard config set|reset`) ----------

/// Sentinel stored in an override doc for a disabled ladder rung.
const RUNG_DISABLED: &str = "off";

const LIST_STR_KEYS: &[&str] = &["blocked_domains", "allowed_invites"];
const LIST_ID_KEYS: &[&str] = &["trusted_role_ids", "allowlisted_user_ids"];
const LADDER_TIERS: &[&str] = &["quarantine", "fresh", "established"];
const LADDER_RUNGS: &[&str] = &["flag", "delete", "timeout", "kick", "ban"];
const FLOAT_KEYS: &[&str] = &[
    "plan.local_hit_threshold",
    "plan.fresh_sample_rate",
    "plan.established_sample_rate",
    "fusion.w_local",
    "fusion.w_paxmod",
    "fusion.w_jev",
];

/// Keys `/guard config set` and `reset` accept (`log_channel_id` is handled
/// separately, stored in guild_state like `/guard logchannel`).
pub const OVERRIDE_KEYS: &[&str] = &[
    "blocked_domains",
    "allowed_invites",
    "trusted_role_ids",
    "allowlisted_user_ids",
    "ladder.<tier>.<rung>",
    "plan.local_hit_threshold",
    "plan.fresh_sample_rate",
    "plan.established_sample_rate",
    "fusion.w_local",
    "fusion.w_paxmod",
    "fusion.w_jev",
];

fn parse_doc(doc: Option<&str>) -> anyhow::Result<toml::Table> {
    match doc {
        Some(d) if !d.trim().is_empty() => Ok(toml::from_str(d).map_err(|e| anyhow::anyhow!("stored override does not parse: {e}"))?),
        _ => Ok(toml::Table::new()),
    }
}

fn nested<'a>(t: &'a mut toml::Table, section: &str) -> anyhow::Result<&'a mut toml::Table> {
    let v = t.entry(section.to_string()).or_insert_with(|| toml::Value::Table(toml::Table::new()));
    v.as_table_mut().ok_or_else(|| anyhow::anyhow!("{section} is not a table in the stored override"))
}

/// Serialize an override table in a canonical order (TOML wants values before tables).
fn finish_doc(t: toml::Table) -> anyhow::Result<String> {
    const ORDER: &[&str] = &["blocked_domains", "allowed_invites", "trusted_role_ids", "allowlisted_user_ids", "ladder", "plan", "fusion"];
    let mut out = toml::Table::new();
    for k in ORDER {
        if let Some(v) = t.get(*k) {
            out.insert(k.to_string(), v.clone());
        }
    }
    debug_assert_eq!(out.len(), t.len(), "unknown keys should have been rejected earlier");
    if out.is_empty() {
        return Ok(String::new());
    }
    Ok(toml::to_string(&toml::Value::Table(out))?)
}

fn parse_float(key: &str, value: &str) -> anyhow::Result<f64> {
    let f: f64 = value.parse().map_err(|_| anyhow::anyhow!("{key}: {value:?} is not a number"))?;
    anyhow::ensure!((0.0..=1.0).contains(&f), "{key} must be between 0 and 1");
    Ok(f)
}

/// Set one curated key in a guild override doc, returning the new doc. An empty
/// `value` on a ladder rung disables that rung; on a list key it clears the list.
pub fn override_set(doc: Option<&str>, key: &str, value: &str) -> anyhow::Result<String> {
    let mut t = parse_doc(doc)?;
    let value = value.trim();
    if LIST_STR_KEYS.contains(&key) {
        let items: Vec<&str> = value.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        for it in &items {
            anyhow::ensure!(!it.chars().any(char::is_whitespace), "{key}: {it:?} must not contain spaces");
        }
        t.insert(key.to_string(), toml::Value::Array(items.into_iter().map(|s| toml::Value::String(s.to_string())).collect()));
    } else if LIST_ID_KEYS.contains(&key) {
        let mut ids = vec![];
        for part in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let id: u64 = part.parse().map_err(|_| anyhow::anyhow!("{key}: {part:?} is not a valid Discord ID"))?;
            anyhow::ensure!(id <= i64::MAX as u64, "{key}: {part:?} does not fit the storage format");
            ids.push(toml::Value::Integer(id as i64));
        }
        t.insert(key.to_string(), toml::Value::Array(ids));
    } else if let Some(rest) = key.strip_prefix("ladder.") {
        let (tier, rung) = rest.split_once('.').ok_or_else(|| anyhow::anyhow!("ladder keys look like ladder.<tier>.<rung>"))?;
        anyhow::ensure!(LADDER_TIERS.contains(&tier), "unknown tier {tier:?} (quarantine, fresh, established)");
        anyhow::ensure!(LADDER_RUNGS.contains(&rung), "unknown rung {rung:?} (flag, delete, timeout, kick, ban)");
        let tt = nested(nested(&mut t, "ladder")?, tier)?;
        if value.is_empty() {
            tt.insert(rung.to_string(), toml::Value::String(RUNG_DISABLED.into()));
        } else {
            tt.insert(rung.to_string(), toml::Value::Float(parse_float(key, value)?));
        }
    } else if FLOAT_KEYS.contains(&key) {
        let (section, name) = key.split_once('.').unwrap();
        let f = parse_float(key, value)?;
        nested(&mut t, section)?.insert(name.to_string(), toml::Value::Float(f));
    } else {
        anyhow::bail!("unknown key {key:?}; supported keys: {}", OVERRIDE_KEYS.join(", "));
    }
    finish_doc(t)
}

/// Remove one curated key from a guild override doc (the value falls back to the
/// static config). Returns the new doc, possibly empty.
pub fn override_reset(doc: Option<&str>, key: &str) -> anyhow::Result<String> {
    let mut t = parse_doc(doc)?;
    if LIST_STR_KEYS.contains(&key) || LIST_ID_KEYS.contains(&key) {
        t.remove(key);
    } else if let Some(rest) = key.strip_prefix("ladder.") {
        let (tier, rung) = rest.split_once('.').ok_or_else(|| anyhow::anyhow!("ladder keys look like ladder.<tier>.<rung>"))?;
        anyhow::ensure!(LADDER_TIERS.contains(&tier), "unknown tier {tier:?}");
        anyhow::ensure!(LADDER_RUNGS.contains(&rung), "unknown rung {rung:?}");
        if let Some(toml::Value::Table(l)) = t.get_mut("ladder") {
            if let Some(toml::Value::Table(tt)) = l.get_mut(tier) {
                tt.remove(rung);
            }
        }
    } else if FLOAT_KEYS.contains(&key) {
        let (section, name) = key.split_once('.').unwrap();
        if let Some(toml::Value::Table(st)) = t.get_mut(section) {
            st.remove(name);
        }
    } else {
        anyhow::bail!("unknown key {key:?}; supported keys: {}", OVERRIDE_KEYS.join(", "));
    }
    finish_doc(t)
}

/// Human-readable current value of a curated key in an effective config.
pub fn describe_key(g: &GuildConfig, key: &str) -> String {
    let list = |xs: &[String]| if xs.is_empty() { "(empty)".to_string() } else { xs.join(", ") };
    let ids = |xs: &[u64]| if xs.is_empty() { "(empty)".to_string() } else { xs.iter().map(u64::to_string).collect::<Vec<_>>().join(", ") };
    let rung_fmt = |v: Option<f64>| v.map(|f| format!("{f:.2}")).unwrap_or_else(|| "off".into());
    match key {
        "blocked_domains" => list(&g.blocked_domains),
        "allowed_invites" => list(&g.allowed_invites),
        "trusted_role_ids" => ids(&g.trusted_role_ids),
        "allowlisted_user_ids" => ids(&g.allowlisted_user_ids),
        "plan.local_hit_threshold" => format!("{:.2}", g.plan.local_hit_threshold),
        "plan.fresh_sample_rate" => format!("{:.2}", g.plan.fresh_sample_rate),
        "plan.established_sample_rate" => format!("{:.2}", g.plan.established_sample_rate),
        "fusion.w_local" => format!("{:.2}", g.fusion.w_local),
        "fusion.w_paxmod" => format!("{:.2}", g.fusion.w_paxmod),
        "fusion.w_jev" => format!("{:.2}", g.fusion.w_jev),
        _ => key
            .strip_prefix("ladder.")
            .and_then(|r| r.split_once('.'))
            .map(|(tier, rung)| {
                let r = match tier {
                    "quarantine" => &g.ladder.quarantine,
                    "fresh" => &g.ladder.fresh,
                    _ => &g.ladder.established,
                };
                match rung {
                    "flag" => rung_fmt(r.flag),
                    "delete" => rung_fmt(r.delete),
                    "timeout" => rung_fmt(r.timeout),
                    "kick" => rung_fmt(r.kick),
                    _ => rung_fmt(r.ban),
                }
            })
            .unwrap_or_else(|| "?".into()),
    }
}

/// Human-readable summary of a guild's effective config, for `/guard config show`.
pub fn config_summary(mode: Mode, log_channel: Option<u64>, has_override: bool, g: &GuildConfig) -> String {
    let rungs = |name: &str, r: &Rungs| {
        let r2 = |v: Option<f64>| v.map(|f| format!("{f:.2}")).unwrap_or_else(|| "off".into());
        format!(
            "{name}: flag {} delete {} timeout {} kick {} ban {}",
            r2(r.flag),
            r2(r.delete),
            r2(r.timeout),
            r2(r.kick),
            r2(r.ban)
        )
    };
    let list = |name: &str, xs: &[String]| format!("{name}: {}", if xs.is_empty() { "(none)".into() } else { xs.join(", ") });
    let ids = |name: &str, xs: &[u64]| format!("{name}: {}", if xs.is_empty() { "(none)".into() } else { xs.iter().map(u64::to_string).collect::<Vec<_>>().join(", ") });
    [
        format!("Mode: **{}**", mode.as_str()),
        format!("Mod-log channel: {}", log_channel.map(|c| format!("<#{c}>")).unwrap_or_else(|| "not set".into())),
        format!("DB override: {}", if has_override { "yes" } else { "no" }),
        list("Blocked domains", &g.blocked_domains),
        list("Allowed invites", &g.allowed_invites),
        ids("Trusted roles", &g.trusted_role_ids),
        ids("Allowlisted users", &g.allowlisted_user_ids),
        rungs("Quarantine", &g.ladder.quarantine),
        rungs("Fresh", &g.ladder.fresh),
        rungs("Established", &g.ladder.established),
        format!(
            "Plan: local hit {:.2}, fresh sample {:.2}, established sample {:.2}",
            g.plan.local_hit_threshold, g.plan.fresh_sample_rate, g.plan.established_sample_rate
        ),
        format!("Fusion weights: local {:.2}, paxmod {:.2}, jev {:.2}", g.fusion.w_local, g.fusion.w_paxmod, g.fusion.w_jev),
    ]
    .join("\n")
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
    /// Guild whose admins may manage other servers' configs (`/guard config … guild:`).
    pub manager_guild_id: Option<u64>,
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
            manager_guild_id: match std::env::var("MANAGER_GUILD_ID") {
                Ok(v) if !v.trim().is_empty() => {
                    let id: u64 = v.trim().parse().map_err(|_| anyhow::anyhow!("MANAGER_GUILD_ID is not a valid guild ID"))?;
                    Some(id)
                }
                _ => None,
            },
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

    #[test]
    fn override_set_lists_ids_floats_and_rungs() {
        let d = override_set(None, "blocked_domains", "evil.com, scam.xyz").unwrap();
        let d = override_set(Some(&d), "trusted_role_ids", "123, 456").unwrap();
        let d = override_set(Some(&d), "plan.local_hit_threshold", "0.5").unwrap();
        let d = override_set(Some(&d), "ladder.fresh.kick", "0.9").unwrap();
        let d = override_set(Some(&d), "ladder.quarantine.ban", "").unwrap(); // empty disables the rung
        let c = Config::from_toml_str("").unwrap();
        let g = c.apply_override(1, &d).unwrap();
        assert_eq!(g.blocked_domains, ["evil.com", "scam.xyz"]);
        assert_eq!(g.trusted_role_ids, [123, 456]);
        assert_eq!(g.plan.local_hit_threshold, 0.5);
        assert_eq!(g.ladder.fresh.kick, Some(0.9));
        assert_eq!(g.ladder.quarantine.ban, None, "disabled rung");
        assert_eq!(g.ladder.quarantine.kick, Some(0.85), "untouched rung keeps its default");
    }

    #[test]
    fn override_set_rejects_bad_input() {
        assert!(override_set(None, "nonsense", "1").is_err(), "unknown key");
        assert!(override_set(None, "log_channel_id", "123").is_err(), "handled via guild_state, not the doc");
        assert!(override_set(None, "plan.local_hit_threshold", "1.5").is_err(), "out of range");
        assert!(override_set(None, "plan.local_hit_threshold", "abc").is_err(), "not a float");
        assert!(override_set(None, "ladder.fresh.kick", "2").is_err());
        assert!(override_set(None, "ladder.nope.kick", "0.5").is_err(), "bad tier");
        assert!(override_set(None, "ladder.fresh.smite", "0.5").is_err(), "bad rung");
        assert!(override_set(None, "ladder.fresh", "0.5").is_err(), "missing rung");
        assert!(override_set(None, "trusted_role_ids", "12x").is_err(), "bad snowflake");
        assert!(override_set(None, "blocked_domains", "evil domain.com").is_err(), "space in domain");
    }

    #[test]
    fn override_reset_removes_key_and_falls_back() {
        let d = override_set(None, "blocked_domains", "evil.com").unwrap();
        let d = override_set(Some(&d), "plan.local_hit_threshold", "0.5").unwrap();
        let d = override_reset(Some(&d), "blocked_domains").unwrap();
        let c = Config::from_toml_str("").unwrap();
        let g = c.apply_override(1, &d).unwrap();
        assert!(g.blocked_domains.is_empty(), "reset falls back to static (empty) list");
        assert_eq!(g.plan.local_hit_threshold, 0.5, "other keys survive the reset");
        assert!(override_reset(None, "blocked_domains").unwrap().is_empty(), "reset on empty doc");
        assert!(override_reset(None, "bogus",).is_err());
    }

    #[test]
    fn apply_override_merge_order_db_beats_static_beats_default() {
        let c = Config::from_toml_str(
            "[defaults]\nblocked_domains = [\"default.com\"]\n[guild.7]\nblocked_domains = [\"static.com\"]\n[guild.7.plan]\nlocal_hit_threshold = 0.4\n",
        )
        .unwrap();
        assert_eq!(c.apply_override(1, "").unwrap().blocked_domains, ["default.com"], "defaults only");
        assert_eq!(c.apply_override(7, "").unwrap().blocked_domains, ["static.com"], "static [guild.7] beats defaults");
        let d = override_set(None, "blocked_domains", "db.com").unwrap();
        let g = c.apply_override(7, &d).unwrap();
        assert_eq!(g.blocked_domains, ["db.com"], "DB doc beats static");
        assert_eq!(g.plan.local_hit_threshold, 0.4, "static merge preserved under the DB override");
    }

    #[test]
    fn apply_override_rejects_unknown_keys_and_bad_values() {
        let c = Config::from_toml_str("").unwrap();
        assert!(c.apply_override(1, "nonsense_key = 1").is_err(), "unknown key");
        assert!(c.apply_override(1, "[ladder.fresh]\nkick = 1.5").is_err(), "out of range");
        assert!(c.apply_override(1, "not toml = [").is_err(), "unparseable");
        assert!(c.apply_override(1, "ladder = 42").is_err(), "wrong type");
    }

    #[test]
    fn apply_override_disabled_rung_survives_merge() {
        let c = Config::from_toml_str("").unwrap();
        let g = c.apply_override(1, "[ladder.fresh]\nkick = \"off\"").unwrap();
        assert_eq!(g.ladder.fresh.kick, None);
        assert_eq!(g.ladder.fresh.ban, Some(0.97));
    }

    #[test]
    fn config_summary_smoke() {
        let g = GuildConfig::default();
        let s = config_summary(Mode::Observe, Some(42), true, &g);
        assert!(s.contains("observe") && s.contains("<#42>") && s.contains("Quarantine") && s.contains("Fusion weights"));
        assert!(s.contains("DB override: yes"));
    }

    // Regression C1 (fixed): a [defaults.ladder.established] table that omits kick/ban
    // leaves those rungs DISABLED (None); Rungs uses field-level defaults, not the
    // container-level Rungs::default().
    #[test]
    fn c1_omitted_established_kick_ban_stay_disabled() {
        let c = Config::from_toml_str(
            "[defaults.ladder.established]\nflag = 0.65\ndelete = 0.85\ntimeout = 0.92\n",
        )
        .unwrap();
        let e = &c.guild(1).ladder.established;
        assert_eq!((e.kick, e.ban), (None, None));
    }

    // Regression C1 (fixed), against the shipped config.toml: it omits kick/ban under
    // [defaults.ladder.established], so both must deserialize to None.
    #[test]
    fn c1_shipped_config_keeps_established_kick_ban_disabled() {
        let s = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/config.toml")).unwrap();
        let c = Config::from_toml_str(&s).unwrap();
        let e = &c.guild(1).ladder.established;
        assert_eq!((e.kick, e.ban), (None, None));
    }
}
