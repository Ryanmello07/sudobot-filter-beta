use crate::config::Config;
use crate::ports::{IncidentReport, ModLog};
use crate::store::Store;
use async_trait::async_trait;
use serenity::all::*;
use std::sync::Arc;

pub struct DiscordModLog {
    pub http: Arc<Http>,
    pub cfg: Arc<Config>,
    pub store: Store,
}

impl DiscordModLog {
    /// Mod-log destination: per-guild DB override first, then the config default.
    async fn effective_log_channel(&self, guild: u64) -> Option<u64> {
        self.store.log_channel(guild).await.or(self.cfg.guild(guild).log_channel_id)
    }
}

fn clip(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

/// Safe inside a code block: no fence breaking.
fn code(s: &str, n: usize) -> String {
    format!("```\n{}\n```", clip(&s.replace("```", "'''"), n))
}

/// Humanized "in server" duration: minutes under an hour, hours under two days,
/// days beyond that, "unknown" when the join time is missing.
fn humanize_in_server(minutes: Option<i64>) -> String {
    match minutes {
        None => "unknown".into(),
        Some(m) if m < 60 => format!("{m}m"),
        Some(m) if m < 48 * 60 => format!("{}h", m / 60),
        Some(m) => humanize_days(m / (24 * 60)),
    }
}

/// 1604d is how robots talk; humans say 4.4y. Days are fine under a year.
fn humanize_days(days: i64) -> String {
    if days < 365 {
        format!("{days}d")
    } else {
        format!("{:.1}y", days as f64 / 365.25)
    }
}

/// Grey for observe (nothing actually happened), green-ish for flag, orange for
/// delete/timeout, red for kick/ban and for refused/failed actions.
fn embed_color(r: &IncidentReport) -> u32 {
    const OBSERVE: u32 = 0x95a5a6;
    const FLAG: u32 = 0x57f287;
    const DELETE: u32 = 0xf59e0b;
    const SEVERE: u32 = 0xed4245;
    if r.observe {
        OBSERVE
    } else {
        match r.taken.as_str() {
            "flag" | "none" => FLAG,
            "delete" | "delete+timeout" => DELETE,
            _ => SEVERE,
        }
    }
}

/// e.g. `🚩 Flagged — spam 0.86 (quarantine)`.
fn embed_title(r: &IncidentReport) -> String {
    let what = if r.observe {
        format!("👁️ Observe: would have {}", r.planned)
    } else {
        match r.taken.as_str() {
            "flag" => "🚩 Flagged".to_string(),
            "delete" => "🗑️ Deleted".to_string(),
            "delete+timeout" => "⏰ Deleted + timed out".to_string(),
            "kick" => "👢 Kicked".to_string(),
            "ban" => "🔨 Banned".to_string(),
            "refused" => "⚠️ Action refused".to_string(),
            "failed" => "⚠️ Action failed".to_string(),
            other => format!("⚠️ {other}"),
        }
    };
    clip(&format!("{what} — {} {:.2} ({})", r.label, r.score, r.tier), 250)
}

fn incident_embed(r: &IncidentReport) -> CreateEmbed {
    let mut e = CreateEmbed::new()
        .title(embed_title(r))
        .color(embed_color(r))
        .field("Member", format!("<@{}> (`{}`)", r.user_id, r.user_id), true)
        .field("Account", format!("{} old, {} in server", humanize_days(r.account_age_days), humanize_in_server(r.minutes_in_guild)), true)
        .field("Channel", format!("<#{}>", r.channel_id), true)
        .field(
            "Score",
            format!("{:.2} fused · spam {:.2} · abuse {:.2} · {} source(s)", r.score, r.spam_score, r.abuse_score, r.agreeing_sources),
            true,
        )
        .field("Coverage", clip(&r.coverage, 200), true)
        .field("Message", code(&r.excerpt, 900), false);
    if !r.evidence.is_empty() {
        e = e.field("Evidence", code(&r.evidence.join("\n"), 700), false);
    }
    if let Some(t) = &r.image_text {
        e = e.field("Text read from image", code(t, 900), false);
    }
    if r.swept > 0 {
        e = e.field("Swept", format!("{} earlier message(s) deleted", r.swept), true);
    }
    if !r.failures.is_empty() {
        e = e.field("Problems", code(&r.failures.join("\n"), 700), false);
    }
    e.footer(CreateEmbedFooter::new(format!("incident #{} · {}", r.incident_id, r.tier)))
}

fn incident_buttons(r: &IncidentReport) -> Vec<CreateButton> {
    let id = r.incident_id;
    let mut buttons = vec![CreateButton::new(format!("safe:{id}")).label("Mark safe").style(ButtonStyle::Success)];
    if r.taken != "delete+timeout" {
        buttons.push(CreateButton::new(format!("timeout:{id}")).label("Timeout").style(ButtonStyle::Secondary));
    }
    buttons.push(CreateButton::new(format!("kick:{id}")).label("Kick").style(ButtonStyle::Primary));
    buttons.push(CreateButton::new(format!("ban:{id}")).label("Ban").style(ButtonStyle::Danger));
    buttons
}

#[async_trait]
impl ModLog for DiscordModLog {
    async fn incident(&self, r: &IncidentReport) {
        let Some(ch) = self.effective_log_channel(r.guild_id).await else {
            tracing::info!("incident {} ({} {} {:.2}) -> {}", r.incident_id, r.tier, r.label, r.score, r.taken);
            return;
        };
        let msg = CreateMessage::new()
            .embed(incident_embed(r))
            .components(vec![CreateActionRow::Buttons(incident_buttons(r))])
            .allowed_mentions(CreateAllowedMentions::new());
        if let Err(err) = ChannelId::new(ch).send_message(&self.http, msg).await {
            tracing::warn!("mod-log post failed: {err}");
        }
    }

    async fn notice(&self, guild: u64, text: &str) {
        let Some(ch) = self.effective_log_channel(guild).await else {
            tracing::info!("notice: {text}");
            return;
        };
        let msg = CreateMessage::new().content(clip(text, 1900)).allowed_mentions(CreateAllowedMentions::new());
        if let Err(err) = ChannelId::new(ch).send_message(&self.http, msg).await {
            tracing::warn!("mod-log notice failed: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_in_server_boundaries() {
        assert_eq!(humanize_in_server(None), "unknown");
        assert_eq!(humanize_in_server(Some(0)), "0m");
        assert_eq!(humanize_in_server(Some(45)), "45m");
        assert_eq!(humanize_in_server(Some(59)), "59m");
        assert_eq!(humanize_in_server(Some(60)), "1h");
        assert_eq!(humanize_in_server(Some(47 * 60 + 59)), "47h");
        assert_eq!(humanize_in_server(Some(48 * 60)), "2d");
        assert_eq!(humanize_in_server(Some(82553)), "57d");
        assert_eq!(humanize_days(0), "0d");
        assert_eq!(humanize_days(89), "89d");
        assert_eq!(humanize_days(364), "364d");
        assert_eq!(humanize_days(365), "1.0y");
        assert_eq!(humanize_days(1604), "4.4y");
        assert_eq!(humanize_in_server(Some(82553)), "57d");
    }

    fn report(taken: &str, observe: bool) -> IncidentReport {
        IncidentReport {
            incident_id: 123,
            guild_id: 1,
            user_id: 7,
            channel_id: 50,
            message_id: 99,
            tier: "quarantine".into(),
            label: "spam".into(),
            score: 0.86,
            spam_score: 0.86,
            abuse_score: 0.0,
            agreeing_sources: 2,
            planned: "delete+timeout".into(),
            taken: taken.into(),
            observe,
            account_age_days: 1604,
            minutes_in_guild: Some(82553),
            coverage: "Coverage { paxmod: Ran, jev: Ran, ocr: Skipped }".into(),
            evidence: vec!["rule:blocked_domain (0.80)".into()],
            excerpt: "free nitro at evil.com".into(),
            image_text: None,
            failures: vec![],
            swept: 0,
        }
    }

    #[test]
    fn buttons_hide_timeout_after_auto_timeout() {
        let ids = |r: &IncidentReport| -> Vec<String> {
            incident_buttons(r)
                .iter()
                .map(|b| serde_json::to_value(b).unwrap()["custom_id"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(ids(&report("delete+timeout", false)), ["safe:123", "kick:123", "ban:123"]);
        let all = ["safe:123", "timeout:123", "kick:123", "ban:123"];
        assert_eq!(ids(&report("flag", false)), all);
        assert_eq!(ids(&report("delete", false)), all);
        assert_eq!(ids(&report("would-have", true)), all);
    }

    #[test]
    fn embed_layout_flag_severity() {
        let v = serde_json::to_value(incident_embed(&report("flag", false))).unwrap();
        assert_eq!(v["title"], "🚩 Flagged — spam 0.86 (quarantine)");
        assert_eq!(v["color"], 0x57f287);
        assert_eq!(v["footer"]["text"], "incident #123 · quarantine");
        let fields = v["fields"].as_array().unwrap();
        let get = |name: &str| fields.iter().find(|f| f["name"] == name).map(|f| f["value"].as_str().unwrap().to_string());
        assert_eq!(get("Member").as_deref(), Some("<@7> (`7`)"));
        assert_eq!(get("Account").as_deref(), Some("4.4y old, 57d in server"));
        assert!(get("Score").unwrap().contains("0.86 fused · spam 0.86 · abuse 0.00 · 2 source(s)"));
        assert!(get("Message").unwrap().contains("free nitro at evil.com"));
        assert!(get("Message").unwrap().starts_with("```"));
        assert!(get("Evidence").unwrap().contains("rule:blocked_domain"));
    }

    #[test]
    fn embed_colors_by_severity() {
        let color = |taken: &str, observe: bool| serde_json::to_value(incident_embed(&report(taken, observe))).unwrap()["color"].clone();
        assert_eq!(color("flag", false), 0x57f287);
        assert_eq!(color("delete", false), 0xf59e0b);
        assert_eq!(color("delete+timeout", false), 0xf59e0b);
        assert_eq!(color("kick", false), 0xed4245);
        assert_eq!(color("ban", false), 0xed4245);
        assert_eq!(color("delete+timeout", true), 0x95a5a6, "observe is visually distinct");
        let v = serde_json::to_value(incident_embed(&report("delete+timeout", true))).unwrap();
        assert_eq!(v["title"], "👁️ Observe: would have delete+timeout — spam 0.86 (quarantine)");
    }

    #[test]
    fn embed_missing_join_time_and_escaping() {
        let mut r = report("ban", false);
        r.minutes_in_guild = None;
        r.excerpt = "fence ``` breaker".into();
        let v = serde_json::to_value(incident_embed(&r)).unwrap();
        let fields = v["fields"].as_array().unwrap();
        let account = fields.iter().find(|f| f["name"] == "Account").unwrap()["value"].as_str().unwrap().to_string();
        assert_eq!(account, "4.4y old, unknown in server");
        let msg = fields.iter().find(|f| f["name"] == "Message").unwrap()["value"].as_str().unwrap().to_string();
        assert!(msg.contains("'''"), "inner fence escaped");
        assert!(!msg.contains("fence ```"), "inner fence must not survive inside the code block");
    }
}
