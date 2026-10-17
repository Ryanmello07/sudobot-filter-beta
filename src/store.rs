//! SQLite persistence.

use crate::model::{Mode, MemberHistory};
use chrono::{DateTime, Duration, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

#[derive(Debug, Clone)]
pub struct IncidentRow {
    pub id: i64,
    pub guild_id: u64,
    pub user_id: u64,
    pub channel_id: u64,
    pub message_id: u64,
    pub tier: String,
    pub label: String,
    pub score: f64,
    pub action_planned: String,
    pub action_taken: String,
    pub outcome: String,
    pub text_excerpt: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GuildOverview {
    pub guild_id: u64,
    pub mode: Mode,
    pub log_channel_id: Option<u64>,
    pub has_override: bool,
    pub incidents: i64,
}

#[derive(Debug, Clone, Default)]
pub struct NewIncident {
    pub guild_id: u64,
    pub user_id: u64,
    pub channel_id: u64,
    pub message_id: u64,
    pub tier: String,
    pub label: String,
    pub score: f64,
    pub local_signals: String,
    pub paxmod_json: Option<String>,
    pub jev_json: Option<String>,
    pub ocr_json: Option<String>,
    pub verdict_json: String,
    pub action_planned: String,
    pub action_taken: String,
    pub outcome: String,
    pub text_excerpt: String,
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339()
}

impl Store {
    pub async fn connect(url: &str) -> anyhow::Result<Store> {
        let opts = SqliteConnectOptions::from_str(url)?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5));
        let pool = SqlitePoolOptions::new().max_connections(5).connect_with(opts).await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Store { pool })
    }

    pub async fn memory() -> anyhow::Result<Store> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::from_str("sqlite::memory:")?)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Store { pool })
    }

    // ---- mode ----

    pub async fn mode(&self, guild: u64) -> Mode {
        let r = sqlx::query("SELECT mode FROM guild_state WHERE guild_id = ?")
            .bind(guild as i64)
            .fetch_optional(&self.pool)
            .await;
        match r {
            Ok(Some(row)) => Mode::parse(row.get::<String, _>("mode").as_str()).unwrap_or(Mode::Observe),
            // Unknown or error: the safe choice is Observe.
            _ => Mode::Observe,
        }
    }

    pub async fn set_mode(&self, guild: u64, mode: Mode, by: u64) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO guild_state (guild_id, mode, updated_by, updated_at) VALUES (?,?,?,?)
             ON CONFLICT(guild_id) DO UPDATE SET mode=excluded.mode, updated_by=excluded.updated_by, updated_at=excluded.updated_at",
        )
        .bind(guild as i64)
        .bind(mode.as_str())
        .bind(by as i64)
        .bind(ts(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Per-guild mod-log channel override, set at runtime via `/guard logchannel`.
    pub async fn set_log_channel(&self, guild: u64, channel: u64, by: u64) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO guild_state (guild_id, log_channel_id, updated_by, updated_at) VALUES (?,?,?,?)
             ON CONFLICT(guild_id) DO UPDATE SET log_channel_id=excluded.log_channel_id, updated_by=excluded.updated_by, updated_at=excluded.updated_at",
        )
        .bind(guild as i64)
        .bind(channel as i64)
        .bind(by as i64)
        .bind(ts(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The DB override for the guild's mod-log channel. Fail-safe: None on error.
    pub async fn log_channel(&self, guild: u64) -> Option<u64> {
        sqlx::query("SELECT log_channel_id FROM guild_state WHERE guild_id = ?")
            .bind(guild as i64)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .and_then(|r| r.get::<Option<i64>, _>("log_channel_id"))
            .map(|c| c as u64)
    }

    pub async fn clear_log_channel(&self, guild: u64) -> anyhow::Result<()> {
        sqlx::query("UPDATE guild_state SET log_channel_id = NULL WHERE guild_id = ?").bind(guild as i64).execute(&self.pool).await?;
        Ok(())
    }

    // ---- guild config overrides (`/guard config set|reset`) ----

    /// Store the per-guild override TOML doc. Caller must have validated it already
    /// (see `Config::apply_override`).
    pub async fn set_guild_override(&self, guild: u64, toml_doc: &str, by: u64) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO guild_overrides (guild_id, toml, updated_by, updated_at) VALUES (?,?,?,?)
             ON CONFLICT(guild_id) DO UPDATE SET toml=excluded.toml, updated_by=excluded.updated_by, updated_at=excluded.updated_at",
        )
        .bind(guild as i64)
        .bind(toml_doc)
        .bind(by as i64)
        .bind(ts(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The guild's override TOML doc, if any. Fail-safe: None on error.
    pub async fn guild_override(&self, guild: u64) -> Option<String> {
        sqlx::query("SELECT toml FROM guild_overrides WHERE guild_id = ?")
            .bind(guild as i64)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .map(|r| r.get::<String, _>("toml"))
    }

    pub async fn clear_guild_override(&self, guild: u64) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM guild_overrides WHERE guild_id = ?").bind(guild as i64).execute(&self.pool).await?;
        Ok(())
    }

    /// One row per guild that has any guard state, for `/guard guilds`.
    pub async fn guild_overview(&self) -> anyhow::Result<Vec<GuildOverview>> {
        use std::collections::{HashMap, HashSet};
        let states = sqlx::query("SELECT guild_id, mode, log_channel_id FROM guild_state").fetch_all(&self.pool).await?;
        let overrides = sqlx::query("SELECT guild_id FROM guild_overrides").fetch_all(&self.pool).await?;
        let incidents = sqlx::query("SELECT guild_id, COUNT(*) AS c FROM incidents GROUP BY guild_id").fetch_all(&self.pool).await?;

        let with_override: HashSet<i64> = overrides.iter().map(|r| r.get::<i64, _>("guild_id")).collect();
        let mut by_guild: HashMap<i64, GuildOverview> = HashMap::new();
        for r in states {
            let g = r.get::<i64, _>("guild_id");
            by_guild.insert(
                g,
                GuildOverview {
                    guild_id: g as u64,
                    mode: Mode::parse(r.get::<String, _>("mode").as_str()).unwrap_or(Mode::Observe),
                    log_channel_id: r.get::<Option<i64>, _>("log_channel_id").map(|c| c as u64),
                    has_override: with_override.contains(&g),
                    incidents: 0,
                },
            );
        }
        for r in incidents {
            let g = r.get::<i64, _>("guild_id");
            by_guild
                .entry(g)
                .or_insert_with(|| GuildOverview { guild_id: g as u64, mode: Mode::Observe, log_channel_id: None, has_override: with_override.contains(&g), incidents: 0 })
                .incidents = r.get::<i64, _>("c");
        }
        for g in &with_override {
            by_guild.entry(*g).or_insert_with(|| GuildOverview { guild_id: *g as u64, mode: Mode::Observe, log_channel_id: None, has_override: true, incidents: 0 });
        }
        let mut out: Vec<GuildOverview> = by_guild.into_values().collect();
        out.sort_by_key(|o| o.guild_id);
        Ok(out)
    }

    // ---- members ----

    async fn ensure_member(&self, g: u64, u: u64) -> anyhow::Result<()> {
        sqlx::query("INSERT OR IGNORE INTO members (guild_id, user_id) VALUES (?,?)")
            .bind(g as i64)
            .bind(u as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn history(&self, g: u64, u: u64, now: DateTime<Utc>) -> anyhow::Result<MemberHistory> {
        let row = sqlx::query("SELECT clean_scans, messages, last_flag_at, join_flag FROM members WHERE guild_id=? AND user_id=?")
            .bind(g as i64)
            .bind(u as i64)
            .fetch_optional(&self.pool)
            .await?;
        let strikes: i64 = sqlx::query("SELECT COUNT(*) AS c FROM strikes WHERE guild_id=? AND user_id=? AND expires_at > ?")
            .bind(g as i64)
            .bind(u as i64)
            .bind(ts(now))
            .fetch_one(&self.pool)
            .await?
            .get("c");
        let mut h = MemberHistory { active_strikes: strikes as u32, ..Default::default() };
        if let Some(r) = row {
            h.clean_scans = r.get::<i64, _>("clean_scans") as u32;
            h.messages_seen = r.get::<i64, _>("messages") as u32;
            h.join_flag = r.get::<i64, _>("join_flag") != 0;
            if let Some(t) = r.get::<Option<String>, _>("last_flag_at") {
                if let Ok(t) = DateTime::parse_from_rfc3339(&t) {
                    h.flagged_recently = now - t.with_timezone(&Utc) < Duration::days(30);
                }
            }
        }
        Ok(h)
    }

    pub async fn note_message(&self, g: u64, u: u64) -> anyhow::Result<()> {
        self.ensure_member(g, u).await?;
        sqlx::query("UPDATE members SET messages = messages + 1 WHERE guild_id=? AND user_id=?")
            .bind(g as i64)
            .bind(u as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn note_clean_scan(&self, g: u64, u: u64) -> anyhow::Result<()> {
        self.ensure_member(g, u).await?;
        sqlx::query("UPDATE members SET clean_scans = clean_scans + 1 WHERE guild_id=? AND user_id=?")
            .bind(g as i64)
            .bind(u as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Any flag resets the clean-scan count.
    pub async fn note_flag(&self, g: u64, u: u64, now: DateTime<Utc>) -> anyhow::Result<()> {
        self.ensure_member(g, u).await?;
        sqlx::query("UPDATE members SET clean_scans = 0, last_flag_at = ? WHERE guild_id=? AND user_id=?")
            .bind(ts(now))
            .bind(g as i64)
            .bind(u as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_join_flag(&self, g: u64, u: u64, v: bool) -> anyhow::Result<()> {
        self.ensure_member(g, u).await?;
        sqlx::query("UPDATE members SET join_flag = ? WHERE guild_id=? AND user_id=?")
            .bind(v as i64)
            .bind(g as i64)
            .bind(u as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_allowlisted(&self, g: u64, u: u64, v: bool) -> anyhow::Result<()> {
        self.ensure_member(g, u).await?;
        sqlx::query("UPDATE members SET allowlisted = ? WHERE guild_id=? AND user_id=?")
            .bind(v as i64)
            .bind(g as i64)
            .bind(u as i64)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn is_allowlisted(&self, g: u64, u: u64) -> bool {
        sqlx::query("SELECT allowlisted FROM members WHERE guild_id=? AND user_id=?")
            .bind(g as i64)
            .bind(u as i64)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .is_some_and(|r| r.get::<i64, _>("allowlisted") != 0)
    }

    // ---- strikes ----

    pub async fn add_strike(&self, g: u64, u: u64, kind: &str, days: i64, incident: Option<i64>, now: DateTime<Utc>) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO strikes (guild_id,user_id,kind,created_at,expires_at,incident_id) VALUES (?,?,?,?,?,?)")
            .bind(g as i64)
            .bind(u as i64)
            .bind(kind)
            .bind(ts(now))
            .bind(ts(now + Duration::days(days)))
            .bind(incident)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn clear_strikes_for_incident(&self, incident: i64) -> anyhow::Result<u64> {
        Ok(sqlx::query("DELETE FROM strikes WHERE incident_id = ?").bind(incident).execute(&self.pool).await?.rows_affected())
    }

    /// Clear a member's ACTIVE strikes (expired rows are left for prune). Used by
    /// `/guard safe`. Returns the number cleared.
    pub async fn clear_strikes(&self, g: u64, u: u64, now: DateTime<Utc>) -> anyhow::Result<u64> {
        Ok(sqlx::query("DELETE FROM strikes WHERE guild_id=? AND user_id=? AND expires_at > ?")
            .bind(g as i64)
            .bind(u as i64)
            .bind(ts(now))
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    // ---- recent messages (for sweeps) ----

    pub async fn add_recent(&self, g: u64, u: u64, ch: u64, msg: u64, at: DateTime<Utc>) -> anyhow::Result<()> {
        sqlx::query("INSERT OR IGNORE INTO recent_messages (guild_id,user_id,channel_id,message_id,created_at) VALUES (?,?,?,?,?)")
            .bind(g as i64)
            .bind(u as i64)
            .bind(ch as i64)
            .bind(msg as i64)
            .bind(ts(at))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// (channel_id, message_id) pairs since `since`, newest first.
    pub async fn recent_since(&self, g: u64, u: u64, since: DateTime<Utc>) -> anyhow::Result<Vec<(u64, u64)>> {
        let rows = sqlx::query("SELECT channel_id, message_id FROM recent_messages WHERE guild_id=? AND user_id=? AND created_at >= ? ORDER BY created_at DESC")
            .bind(g as i64)
            .bind(u as i64)
            .bind(ts(since))
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.iter().map(|r| (r.get::<i64, _>("channel_id") as u64, r.get::<i64, _>("message_id") as u64)).collect())
    }

    pub async fn forget_recent(&self, g: u64, message_ids: &[u64]) -> anyhow::Result<()> {
        for m in message_ids {
            sqlx::query("DELETE FROM recent_messages WHERE guild_id=? AND message_id=?").bind(g as i64).bind(*m as i64).execute(&self.pool).await?;
        }
        Ok(())
    }

    // ---- incidents ----

    pub async fn insert_incident(&self, i: &NewIncident, now: DateTime<Utc>) -> anyhow::Result<i64> {
        let r = sqlx::query(
            "INSERT INTO incidents (guild_id,user_id,channel_id,message_id,tier,label,score,local_signals,paxmod_json,jev_json,ocr_json,verdict_json,action_planned,action_taken,outcome,text_excerpt,created_at)
             VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT(guild_id, message_id) DO UPDATE SET tier=excluded.tier, label=excluded.label, score=excluded.score,
               local_signals=excluded.local_signals, paxmod_json=excluded.paxmod_json, jev_json=excluded.jev_json, ocr_json=excluded.ocr_json,
               verdict_json=excluded.verdict_json, action_planned=excluded.action_planned, action_taken=excluded.action_taken,
               outcome=excluded.outcome, text_excerpt=excluded.text_excerpt
             RETURNING id",
        )
        .bind(i.guild_id as i64)
        .bind(i.user_id as i64)
        .bind(i.channel_id as i64)
        .bind(i.message_id as i64)
        .bind(&i.tier)
        .bind(&i.label)
        .bind(i.score)
        .bind(&i.local_signals)
        .bind(&i.paxmod_json)
        .bind(&i.jev_json)
        .bind(&i.ocr_json)
        .bind(&i.verdict_json)
        .bind(&i.action_planned)
        .bind(&i.action_taken)
        .bind(&i.outcome)
        .bind(&i.text_excerpt)
        .bind(ts(now))
        .fetch_one(&self.pool)
        .await?;
        Ok(r.get("id"))
    }

    pub async fn incident(&self, id: i64) -> anyhow::Result<Option<IncidentRow>> {
        let r = sqlx::query("SELECT * FROM incidents WHERE id = ?").bind(id).fetch_optional(&self.pool).await?;
        Ok(r.map(|r| IncidentRow {
            id: r.get("id"),
            guild_id: r.get::<i64, _>("guild_id") as u64,
            user_id: r.get::<i64, _>("user_id") as u64,
            channel_id: r.get::<i64, _>("channel_id") as u64,
            message_id: r.get::<i64, _>("message_id") as u64,
            tier: r.get("tier"),
            label: r.get("label"),
            score: r.get("score"),
            action_planned: r.get("action_planned"),
            action_taken: r.get("action_taken"),
            outcome: r.get("outcome"),
            text_excerpt: r.get("text_excerpt"),
        }))
    }

    pub async fn set_outcome(&self, id: i64, outcome: &str) -> anyhow::Result<()> {
        sqlx::query("UPDATE incidents SET outcome = ? WHERE id = ?").bind(outcome).bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn incident_exists(&self, g: u64, message_id: u64) -> bool {
        sqlx::query("SELECT 1 FROM incidents WHERE guild_id=? AND message_id=? AND action_taken IN ('delete','delete+timeout','kick','ban')")
            .bind(g as i64)
            .bind(message_id as i64)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .is_some()
    }

    // ---- usage / maintenance ----

    pub async fn record_usage(&self, provider: &str, calls: i64, failures: i64, now: DateTime<Utc>) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO api_usage (day, provider, calls, failures) VALUES (?,?,?,?)
             ON CONFLICT(day, provider) DO UPDATE SET calls = calls + excluded.calls, failures = failures + excluded.failures",
        )
        .bind(now.format("%Y-%m-%d").to_string())
        .bind(provider)
        .bind(calls)
        .bind(failures)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn usage_today(&self, provider: &str, now: DateTime<Utc>) -> (u64, u64) {
        sqlx::query("SELECT calls, failures FROM api_usage WHERE day=? AND provider=?")
            .bind(now.format("%Y-%m-%d").to_string())
            .bind(provider)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .map(|r| (r.get::<i64, _>("calls") as u64, r.get::<i64, _>("failures") as u64))
            .unwrap_or((0, 0))
    }

    pub async fn prune(&self, retention_days: i64, now: DateTime<Utc>) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM incidents WHERE created_at < ?").bind(ts(now - Duration::days(retention_days))).execute(&self.pool).await?;
        sqlx::query("DELETE FROM recent_messages WHERE created_at < ?").bind(ts(now - Duration::hours(1))).execute(&self.pool).await?;
        sqlx::query("DELETE FROM strikes WHERE expires_at < ?").bind(ts(now)).execute(&self.pool).await?;
        sqlx::query("DELETE FROM api_usage WHERE day < ?").bind((now - Duration::days(30)).format("%Y-%m-%d").to_string()).execute(&self.pool).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    #[tokio::test]
    async fn mode_defaults_to_observe_and_persists() {
        let s = Store::memory().await.unwrap();
        assert_eq!(s.mode(1).await, Mode::Observe);
        s.set_mode(1, Mode::Enforce, 9).await.unwrap();
        assert_eq!(s.mode(1).await, Mode::Enforce);
        assert_eq!(s.mode(2).await, Mode::Observe);
        s.set_mode(1, Mode::Observe, 9).await.unwrap();
        assert_eq!(s.mode(1).await, Mode::Observe);
    }

    #[tokio::test]
    async fn log_channel_round_trip_and_unset_is_none() {
        let s = Store::memory().await.unwrap();
        // unset returns None
        assert_eq!(s.log_channel(1).await, None);
        // set/get round-trip
        s.set_log_channel(1, 99, 7).await.unwrap();
        assert_eq!(s.log_channel(1).await, Some(99));
        // overwrite wins
        s.set_log_channel(1, 100, 7).await.unwrap();
        assert_eq!(s.log_channel(1).await, Some(100));
        // per-guild isolation
        assert_eq!(s.log_channel(2).await, None);
        // setting the log channel must not clobber the mode, and vice versa
        s.set_mode(1, Mode::Enforce, 7).await.unwrap();
        assert_eq!(s.log_channel(1).await, Some(100));
        assert_eq!(s.mode(1).await, Mode::Enforce);
        // clear falls back to None
        s.clear_log_channel(1).await.unwrap();
        assert_eq!(s.log_channel(1).await, None);
    }

    #[tokio::test]
    async fn guild_override_round_trip_and_clear() {
        let s = Store::memory().await.unwrap();
        assert_eq!(s.guild_override(1).await, None);
        s.set_guild_override(1, "blocked_domains = [\"evil.com\"]", 7).await.unwrap();
        assert_eq!(s.guild_override(1).await.as_deref(), Some("blocked_domains = [\"evil.com\"]"));
        s.set_guild_override(1, "plan.local_hit_threshold = 0.5", 8).await.unwrap();
        assert_eq!(s.guild_override(1).await.as_deref(), Some("plan.local_hit_threshold = 0.5"));
        assert_eq!(s.guild_override(2).await, None);
        s.clear_guild_override(1).await.unwrap();
        assert_eq!(s.guild_override(1).await, None);
    }

    #[tokio::test]
    async fn guild_overview_combines_state_overrides_and_incidents() {
        let s = Store::memory().await.unwrap();
        let t = now();
        assert!(s.guild_overview().await.unwrap().is_empty());
        s.set_mode(1, Mode::Enforce, 9).await.unwrap();
        s.set_log_channel(1, 55, 9).await.unwrap();
        s.set_guild_override(2, "blocked_domains = []", 9).await.unwrap();
        let i = NewIncident { guild_id: 1, message_id: 4, tier: "fresh".into(), label: "spam".into(), score: 0.8, action_planned: "delete".into(), action_taken: "delete".into(), outcome: "open".into(), verdict_json: "{}".into(), local_signals: "[]".into(), ..Default::default() };
        s.insert_incident(&i, t).await.unwrap();
        let rows = s.guild_overview().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].guild_id, 1);
        assert_eq!(rows[0].mode, Mode::Enforce);
        assert_eq!(rows[0].log_channel_id, Some(55));
        assert_eq!(rows[0].incidents, 1);
        assert!(!rows[0].has_override);
        assert_eq!(rows[1].guild_id, 2);
        assert!(rows[1].has_override);
        assert_eq!(rows[1].incidents, 0);
    }

    #[tokio::test]
    async fn history_counts_and_flag_reset() {
        let s = Store::memory().await.unwrap();
        let t = now();
        let h = s.history(1, 2, t).await.unwrap();
        assert_eq!((h.clean_scans, h.active_strikes, h.flagged_recently), (0, 0, false));
        for _ in 0..3 {
            s.note_clean_scan(1, 2).await.unwrap();
        }
        s.note_message(1, 2).await.unwrap();
        let h = s.history(1, 2, t).await.unwrap();
        assert_eq!((h.clean_scans, h.messages_seen), (3, 1));
        s.note_flag(1, 2, t).await.unwrap();
        let h = s.history(1, 2, t).await.unwrap();
        assert_eq!(h.clean_scans, 0);
        assert!(h.flagged_recently);
        // flag older than 30 days no longer counts
        assert!(!s.history(1, 2, t + Duration::days(31)).await.unwrap().flagged_recently);
    }

    #[tokio::test]
    async fn strikes_expire_and_can_be_cleared() {
        let s = Store::memory().await.unwrap();
        let t = now();
        s.add_strike(1, 2, "timeout", 30, Some(5), t).await.unwrap();
        assert_eq!(s.history(1, 2, t).await.unwrap().active_strikes, 1);
        assert_eq!(s.history(1, 2, t + Duration::days(31)).await.unwrap().active_strikes, 0);
        assert_eq!(s.clear_strikes_for_incident(5).await.unwrap(), 1);
        assert_eq!(s.history(1, 2, t).await.unwrap().active_strikes, 0);
    }

    #[tokio::test]
    async fn clear_strikes_clears_only_active_for_that_member() {
        let s = Store::memory().await.unwrap();
        let t = now();
        s.add_strike(1, 2, "timeout", 30, None, t).await.unwrap();
        s.add_strike(1, 2, "kick", 30, None, t).await.unwrap();
        s.add_strike(1, 2, "timeout", 1, None, t - Duration::days(2)).await.unwrap(); // expired
        s.add_strike(2, 2, "timeout", 30, None, t).await.unwrap(); // other guild
        s.add_strike(1, 3, "timeout", 30, None, t).await.unwrap(); // other member
        assert_eq!(s.history(1, 2, t).await.unwrap().active_strikes, 2);
        assert_eq!(s.clear_strikes(1, 2, t).await.unwrap(), 2, "two active strikes cleared");
        assert_eq!(s.history(1, 2, t).await.unwrap().active_strikes, 0);
        assert_eq!(s.history(2, 2, t).await.unwrap().active_strikes, 1, "other guild untouched");
        assert_eq!(s.history(1, 3, t).await.unwrap().active_strikes, 1, "other member untouched");
        // expired row remains until prune
        assert_eq!(s.clear_strikes(1, 2, t).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn allowlist_and_join_flag() {
        let s = Store::memory().await.unwrap();
        assert!(!s.is_allowlisted(1, 2).await);
        s.set_allowlisted(1, 2, true).await.unwrap();
        assert!(s.is_allowlisted(1, 2).await);
        s.set_join_flag(1, 2, true).await.unwrap();
        assert!(s.history(1, 2, now()).await.unwrap().join_flag);
    }

    #[tokio::test]
    async fn recent_messages_window_and_dedup() {
        let s = Store::memory().await.unwrap();
        let t = now();
        s.add_recent(1, 2, 10, 100, t - Duration::minutes(2)).await.unwrap();
        s.add_recent(1, 2, 10, 100, t).await.unwrap(); // edit: no second row
        s.add_recent(1, 2, 11, 101, t - Duration::minutes(30)).await.unwrap();
        let r = s.recent_since(1, 2, t - Duration::minutes(10)).await.unwrap();
        assert_eq!(r, vec![(10, 100)]);
        s.forget_recent(1, &[100]).await.unwrap();
        assert!(s.recent_since(1, 2, t - Duration::minutes(10)).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn incident_upsert_by_message_and_prune() {
        let s = Store::memory().await.unwrap();
        let t = now();
        let mut i = NewIncident { guild_id: 1, user_id: 2, channel_id: 3, message_id: 4, tier: "fresh".into(), label: "spam".into(), score: 0.8, action_planned: "delete".into(), action_taken: "would-have".into(), outcome: "observe".into(), verdict_json: "{}".into(), local_signals: "[]".into(), ..Default::default() };
        let a = s.insert_incident(&i, t).await.unwrap();
        i.action_taken = "delete".into();
        let b = s.insert_incident(&i, t).await.unwrap();
        assert_eq!(a, b);
        assert_eq!(s.incident(a).await.unwrap().unwrap().action_taken, "delete");
        s.set_outcome(a, "undone").await.unwrap();
        assert_eq!(s.incident(a).await.unwrap().unwrap().outcome, "undone");
        s.prune(14, t + Duration::days(15)).await.unwrap();
        assert!(s.incident(a).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn usage_accumulates() {
        let s = Store::memory().await.unwrap();
        let t = now();
        s.record_usage("jev", 2, 1, t).await.unwrap();
        s.record_usage("jev", 3, 0, t).await.unwrap();
        assert_eq!(s.usage_today("jev", t).await, (5, 1));
        assert_eq!(s.usage_today("paxmod", t).await, (0, 0));
    }
}
