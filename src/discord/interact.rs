//! `/guard status|mode` and the mod-log buttons.

use crate::model::Mode;
use crate::pipeline::Pipeline;
use serenity::all::*;
use std::sync::atomic::{AtomicU64, Ordering};

pub async fn register(ctx: &Context, guild: GuildId) {
    let cmd = CreateCommand::new("guard")
        .description("Chat guard controls")
        .default_member_permissions(Permissions::MANAGE_GUILD)
        .dm_permission(false)
        .add_option(CreateCommandOption::new(CommandOptionType::SubCommand, "status", "Show mode and API usage"))
        .add_option(
            CreateCommandOption::new(CommandOptionType::SubCommand, "mode", "Switch between observe and enforce").add_sub_option(
                CreateCommandOption::new(CommandOptionType::String, "value", "observe logs only; enforce acts")
                    .required(true)
                    .add_string_choice("observe", "observe")
                    .add_string_choice("enforce", "enforce"),
            ),
        );
    if let Err(e) = guild.set_commands(&ctx.http, vec![cmd]).await {
        tracing::warn!("registering commands in {guild} failed: {e}");
    }
}

async fn reply_cmd(ctx: &Context, c: &CommandInteraction, text: String) {
    let msg = CreateInteractionResponseMessage::new().content(text).ephemeral(true).allowed_mentions(CreateAllowedMentions::new());
    if let Err(e) = c.create_response(&ctx.http, CreateInteractionResponse::Message(msg)).await {
        tracing::warn!("interaction reply failed: {e}");
    }
}

pub async fn command(ctx: &Context, c: &CommandInteraction, p: &Pipeline, shed: &AtomicU64) {
    let Some(gid) = c.guild_id else { return };
    let allowed = c.member.as_ref().and_then(|m| m.permissions).is_some_and(|p| p.contains(Permissions::MANAGE_GUILD) || p.contains(Permissions::ADMINISTRATOR));
    if !allowed {
        return reply_cmd(ctx, c, "You need the Manage Server permission.".into()).await;
    }
    let Some(sub) = c.data.options.first() else { return };
    match sub.name.as_str() {
        "status" => {
            let mode = p.store().mode(gid.get()).await;
            let now = chrono::Utc::now();
            let mut lines = vec![format!("Mode: **{}**", mode.as_str())];
            for (name, cap) in p.caps() {
                let (calls, fails) = p.store().usage_today(name, now).await;
                lines.push(format!("{name}: {calls}/{cap} calls today, {fails} failures"));
            }
            lines.push(format!("Messages shed under load: {}", shed.load(Ordering::Relaxed)));
            reply_cmd(ctx, c, lines.join("\n")).await
        }
        "mode" => {
            let value = match &sub.value {
                CommandDataOptionValue::SubCommand(opts) => opts.first().and_then(|o| o.value.as_str()).map(str::to_string),
                _ => None,
            };
            match value.as_deref().and_then(Mode::parse) {
                Some(mode) => match p.store().set_mode(gid.get(), mode, c.user.id.get()).await {
                    Ok(()) => reply_cmd(ctx, c, format!("Mode set to **{}**.", mode.as_str())).await,
                    Err(e) => reply_cmd(ctx, c, format!("Could not save mode: {e}")).await,
                },
                None => reply_cmd(ctx, c, "Mode must be observe or enforce.".into()).await,
            }
        }
        _ => {}
    }
}

async fn reply_comp(ctx: &Context, c: &ComponentInteraction, text: &str) {
    let msg = CreateInteractionResponseMessage::new().content(text).ephemeral(true).allowed_mentions(CreateAllowedMentions::new());
    if let Err(e) = c.create_response(&ctx.http, CreateInteractionResponse::Message(msg)).await {
        tracing::warn!("component reply failed: {e}");
    }
}

pub async fn component(ctx: &Context, c: &ComponentInteraction, p: &Pipeline) {
    let Some((kind, id)) = c.data.custom_id.split_once(':') else { return };
    let Ok(id) = id.parse::<i64>() else { return };
    let perms = c.member.as_ref().and_then(|m| m.permissions).unwrap_or(Permissions::empty());
    let admin = perms.contains(Permissions::ADMINISTRATOR);
    let Some(inc) = p.store().incident(id).await.ok().flatten() else {
        return reply_comp(ctx, c, "That incident is no longer stored.").await;
    };
    let gid = GuildId::new(inc.guild_id);
    let uid = UserId::new(inc.user_id);
    match kind {
        "undo" => {
            let who = c.user.id.get();
            let res: Result<&str, String> = match inc.action_taken.as_str() {
                "delete+timeout" if admin || perms.contains(Permissions::MODERATE_MEMBERS) => gid
                    .edit_member(&ctx.http, uid, EditMember::new().enable_communication().audit_log_reason("chat guard undo"))
                    .await
                    .map(|_| "Timeout removed. Deleted messages cannot be restored.")
                    .map_err(|e| e.to_string()),
                "ban" if admin || perms.contains(Permissions::BAN_MEMBERS) => {
                    gid.unban(&ctx.http, uid).await.map(|_| "Unbanned.").map_err(|e| e.to_string())
                }
                "delete+timeout" | "ban" => Err("You lack the permission needed to undo this.".into()),
                "kick" => Ok("A kick cannot be undone; the member can rejoin."),
                "delete" => Ok("Deleted messages cannot be restored."),
                _ => Ok("Nothing to undo for this incident."),
            };
            match res {
                Ok(text) => {
                    let _ = p.store().set_outcome(id, &format!("undone by {who}")).await;
                    reply_comp(ctx, c, text).await
                }
                Err(e) => reply_comp(ctx, c, &format!("Undo failed: {e}")).await,
            }
        }
        "safe" => {
            if !(admin || perms.contains(Permissions::MANAGE_GUILD)) {
                return reply_comp(ctx, c, "You need the Manage Server permission.").await;
            }
            let _ = p.store().set_allowlisted(inc.guild_id, inc.user_id, true).await;
            let _ = p.store().clear_strikes_for_incident(id).await;
            let _ = p.store().set_outcome(id, &format!("marked safe by {}", c.user.id.get())).await;
            reply_comp(ctx, c, "Member added to the allowlist and the strike from this incident cleared.").await
        }
        _ => {}
    }
}
