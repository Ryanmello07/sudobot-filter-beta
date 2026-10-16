use crate::config::Config;
use crate::ports::{IncidentReport, ModLog};
use async_trait::async_trait;
use serenity::all::*;
use std::sync::Arc;

pub struct DiscordModLog {
    pub http: Arc<Http>,
    pub cfg: Arc<Config>,
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

#[async_trait]
impl ModLog for DiscordModLog {
    async fn incident(&self, r: &IncidentReport) {
        let Some(ch) = self.cfg.guild(r.guild_id).log_channel_id else {
            tracing::info!("incident {} ({} {} {:.2}) -> {}", r.incident_id, r.tier, r.label, r.score, r.taken);
            return;
        };
        let color = if r.observe { 0x3b82f6 } else if r.taken == "flag" { 0xf59e0b } else { 0xdc2626 };
        let title = if r.observe { format!("Would have: {}", r.planned) } else { format!("{}: {}", r.label, r.taken) };
        let mut e = CreateEmbed::new()
            .title(clip(&title, 250))
            .color(color)
            .field("Member", format!("<@{}> (`{}`)", r.user_id, r.user_id), true)
            .field("Tier", &r.tier, true)
            .field("Score", format!("{:.2} ({})", r.score, r.label), true)
            .field(
                "Account",
                format!("{}d old, {} in server", r.account_age_days, r.minutes_in_guild.map(|m| format!("{m}m")).unwrap_or_else(|| "?".into())),
                true,
            )
            .field("Channel", format!("<#{}>", r.channel_id), true)
            .field("Coverage", clip(&r.coverage, 200), false)
            .field("Message", code(&r.excerpt, 900), false);
        if !r.evidence.is_empty() {
            e = e.field("Evidence", code(&r.evidence.join("\n"), 900), false);
        }
        if let Some(t) = &r.image_text {
            e = e.field("Text read from image", code(t, 900), false);
        }
        if r.swept > 0 {
            e = e.field("Swept", format!("{} earlier message(s) deleted", r.swept), true);
        }
        if !r.failures.is_empty() {
            e = e.field("Problems", code(&r.failures.join("\n"), 900), false);
        }
        e = e.footer(CreateEmbedFooter::new(format!("incident {}", r.incident_id)));
        let buttons = vec![
            CreateButton::new(format!("undo:{}", r.incident_id)).label("Undo").style(ButtonStyle::Secondary),
            CreateButton::new(format!("safe:{}", r.incident_id)).label("Mark safe").style(ButtonStyle::Success),
        ];
        let msg = CreateMessage::new()
            .embed(e)
            .components(vec![CreateActionRow::Buttons(buttons)])
            .allowed_mentions(CreateAllowedMentions::new());
        if let Err(err) = ChannelId::new(ch).send_message(&self.http, msg).await {
            tracing::warn!("mod-log post failed: {err}");
        }
    }

    async fn notice(&self, guild: u64, text: &str) {
        let Some(ch) = self.cfg.guild(guild).log_channel_id else {
            tracing::info!("notice: {text}");
            return;
        };
        let msg = CreateMessage::new().content(clip(text, 1900)).allowed_mentions(CreateAllowedMentions::new());
        if let Err(err) = ChannelId::new(ch).send_message(&self.http, msg).await {
            tracing::warn!("mod-log notice failed: {err}");
        }
    }
}
