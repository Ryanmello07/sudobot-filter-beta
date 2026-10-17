//! `/guard status|mode` and the mod-log buttons.

use crate::model::Mode;
use crate::pipeline::Pipeline;
use serenity::all::*;
use std::sync::atomic::{AtomicU64, Ordering};

pub async fn register(ctx: &Context, guild: GuildId) {
    let guild_opt = || CreateCommandOption::new(CommandOptionType::String, "guild", "guild ID (bot managers in the manager server only)").required(false);
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
        )
        .add_option(
            CreateCommandOption::new(CommandOptionType::SubCommand, "logchannel", "Set or show this server's mod-log channel").add_sub_option(
                CreateCommandOption::new(CommandOptionType::Channel, "channel", "where mod-log posts go; omit to show the current setting").required(false),
            ),
        )
        .add_option(
            CreateCommandOption::new(CommandOptionType::SubCommandGroup, "config", "View or edit server config (stored in the DB, overrides config.toml)")
                .add_sub_option(
                    CreateCommandOption::new(CommandOptionType::SubCommand, "show", "Show the effective config").add_sub_option(guild_opt()),
                )
                .add_sub_option(
                    CreateCommandOption::new(CommandOptionType::SubCommand, "set", "Set one config key")
                        .add_sub_option(CreateCommandOption::new(CommandOptionType::String, "key", "e.g. blocked_domains, ladder.fresh.kick, fusion.w_local").required(true))
                        .add_sub_option(
                            CreateCommandOption::new(CommandOptionType::String, "value", "list (comma-separated), float 0-1, or empty to disable a ladder rung").required(false),
                        )
                        .add_sub_option(guild_opt()),
                )
                .add_sub_option(
                    CreateCommandOption::new(CommandOptionType::SubCommand, "reset", "Remove an override; the key falls back to static config")
                        .add_sub_option(CreateCommandOption::new(CommandOptionType::String, "key", "key to reset").required(true))
                        .add_sub_option(guild_opt()),
                ),
        )
        .add_option(CreateCommandOption::new(CommandOptionType::SubCommand, "guilds", "List guilds with guard state (bot managers only)"));
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

/// Local admins manage their own guild. A different target guild is allowed only
/// for bot managers: the command must be invoked inside the manager guild and the
/// invoker must pass the permission gate there. If no manager guild is configured,
/// cross-guild access is rejected everywhere.
pub fn can_manage_guild(invoked_in: u64, passed_gate: bool, manager_guild: Option<u64>, target: u64) -> bool {
    if target == invoked_in {
        passed_gate
    } else {
        passed_gate && manager_guild.is_some_and(|m| m == invoked_in)
    }
}

/// Resolve the optional `guild` option to a target guild, enforcing the manager rule.
fn resolve_target(invoked: u64, passed_gate: bool, manager_guild: Option<u64>, guild_opt: Option<&str>) -> Result<u64, String> {
    let target = match guild_opt {
        Some(s) => s.trim().parse::<u64>().map_err(|_| format!("{s:?} is not a valid guild ID"))?,
        None => invoked,
    };
    if can_manage_guild(invoked, passed_gate, manager_guild, target) {
        Ok(target)
    } else {
        Err("Only bot managers can manage other servers, and only from the manager server.".into())
    }
}

pub async fn command(ctx: &Context, c: &CommandInteraction, p: &Pipeline, shed: &AtomicU64, manager_guild: Option<u64>) {
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
            let log_ch = p.effective_log_channel(gid.get()).await;
            lines.push(format!("Mod-log channel: {}", log_ch.map(|c| format!("<#{c}>")).unwrap_or_else(|| "not set".into())));
            lines.push(format!("Messages shed under load: {}", shed.load(Ordering::Relaxed)));
            reply_cmd(ctx, c, lines.join("\n")).await
        }
        "logchannel" => {
            let channel = match &sub.value {
                CommandDataOptionValue::SubCommand(opts) => opts.first().and_then(|o| o.value.as_channel_id()),
                _ => None,
            };
            match channel {
                Some(ch) => match p.store().set_log_channel(gid.get(), ch.get(), c.user.id.get()).await {
                    Ok(()) => reply_cmd(ctx, c, format!("Mod-log channel set to <#{}> for this server.", ch.get())).await,
                    Err(e) => reply_cmd(ctx, c, format!("Could not save the log channel: {e}")).await,
                },
                None => {
                    let cur = p.effective_log_channel(gid.get()).await;
                    reply_cmd(ctx, c, match cur {
                        Some(id) => format!("Mod-log channel for this server: <#{id}>."),
                        None => "No mod-log channel set for this server (neither /guard logchannel nor config.toml).".into(),
                    })
                    .await
                }
            }
        }
        "config" => config_cmd(ctx, c, p, sub, gid.get(), manager_guild).await,
        "guilds" => {
            if manager_guild != Some(gid.get()) {
                return reply_cmd(ctx, c, "Only bot managers can use this, and only inside the manager server.".into()).await;
            }
            match p.store().guild_overview().await {
                Ok(rows) if rows.is_empty() => reply_cmd(ctx, c, "No guilds have guard state yet.".into()).await,
                Ok(rows) => {
                    let mut out = format!("{} guild(s) with guard state:\n", rows.len());
                    for r in rows {
                        out.push_str(&format!(
                            "`{}` — {}, {} incident(s), override: {}, log: {}\n",
                            r.guild_id,
                            r.mode.as_str(),
                            r.incidents,
                            if r.has_override { "yes" } else { "no" },
                            r.log_channel_id.map(|c| format!("<#{c}>")).unwrap_or_else(|| "not set".into())
                        ));
                    }
                    out.truncate(1900);
                    reply_cmd(ctx, c, out).await
                }
                Err(e) => reply_cmd(ctx, c, format!("Could not load guild state: {e}")).await,
            }
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

/// `/guard config show|set|reset` — per-guild config overrides stored in the DB.
async fn config_cmd(ctx: &Context, c: &CommandInteraction, p: &Pipeline, sub: &CommandDataOption, invoked: u64, manager_guild: Option<u64>) {
    let CommandDataOptionValue::SubCommandGroup(group) = &sub.value else { return };
    let Some(leaf) = group.first() else { return };
    let opts = match &leaf.value {
        CommandDataOptionValue::SubCommand(o) => o.as_slice(),
        _ => &[],
    };
    let get = |name: &str| opts.iter().find(|o| o.name == name).and_then(|o| o.value.as_str());
    let target = match resolve_target(invoked, true, manager_guild, get("guild")) {
        Ok(t) => t,
        Err(e) => return reply_cmd(ctx, c, e).await,
    };
    let scope = if target == invoked { "this server".to_string() } else { format!("server `{target}`") };

    match leaf.name.as_str() {
        "show" => {
            let gc = p.effective_guild_config(target).await;
            let mode = p.store().mode(target).await;
            let log_ch = p.effective_log_channel(target).await;
            let has_override = p.store().guild_override(target).await.is_some();
            reply_cmd(ctx, c, crate::config::config_summary(mode, log_ch, has_override, &gc)).await;
        }
        "set" => {
            let Some(key) = get("key").map(str::trim) else {
                return reply_cmd(ctx, c, "Missing key.".into()).await;
            };
            let value = get("value").unwrap_or("").trim();
            // log_channel_id shares storage with `/guard logchannel` (guild_state),
            // not the override doc.
            if key == "log_channel_id" {
                let Ok(id) = value.parse::<u64>() else {
                    return reply_cmd(ctx, c, format!("log_channel_id: {value:?} is not a valid channel ID.")).await;
                };
                return match p.store().set_log_channel(target, id, c.user.id.get()).await {
                    Ok(()) => reply_cmd(ctx, c, format!("`log_channel_id` is now <#{id}> for {scope}.")).await,
                    Err(e) => reply_cmd(ctx, c, format!("Could not save: {e}")).await,
                };
            }
            let cur = p.store().guild_override(target).await;
            let new_doc = match crate::config::override_set(cur.as_deref(), key, value) {
                Ok(d) => d,
                Err(e) => return reply_cmd(ctx, c, format!("Not saved: {e}")).await,
            };
            // Validate the merged result before saving (same rules as static config).
            if let Err(e) = p.check_guild_override(target, &new_doc) {
                return reply_cmd(ctx, c, format!("Not saved: {e}")).await;
            }
            match p.store().set_guild_override(target, &new_doc, c.user.id.get()).await {
                Ok(()) => {
                    p.invalidate_guild_config(target);
                    let gc = p.effective_guild_config(target).await;
                    reply_cmd(ctx, c, format!("`{key}` is now `{}` for {scope}.", crate::config::describe_key(&gc, key))).await;
                }
                Err(e) => reply_cmd(ctx, c, format!("Could not save: {e}")).await,
            }
        }
        "reset" => {
            let Some(key) = get("key").map(str::trim) else {
                return reply_cmd(ctx, c, "Missing key.".into()).await;
            };
            if key == "log_channel_id" {
                return match p.store().clear_log_channel(target).await {
                    Ok(()) => reply_cmd(ctx, c, format!("`log_channel_id` reset for {scope}; the config.toml default applies.")).await,
                    Err(e) => reply_cmd(ctx, c, format!("Could not reset: {e}")).await,
                };
            }
            let cur = p.store().guild_override(target).await;
            let new_doc = match crate::config::override_reset(cur.as_deref(), key) {
                Ok(d) => d,
                Err(e) => return reply_cmd(ctx, c, format!("Not reset: {e}")).await,
            };
            let save = if new_doc.trim().is_empty() { p.store().clear_guild_override(target).await } else { p.store().set_guild_override(target, &new_doc, c.user.id.get()).await };
            match save {
                Ok(()) => {
                    p.invalidate_guild_config(target);
                    let gc = p.effective_guild_config(target).await;
                    reply_cmd(ctx, c, format!("`{key}` reset for {scope}; effective value is now `{}`.", crate::config::describe_key(&gc, key))).await;
                }
                Err(e) => reply_cmd(ctx, c, format!("Could not reset: {e}")).await,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manage_own_guild_needs_gate() {
        // target == invoked guild: the normal permission gate decides
        assert!(can_manage_guild(1, true, None, 1));
        assert!(can_manage_guild(1, true, Some(9), 1));
        assert!(!can_manage_guild(1, false, Some(9), 1));
    }

    #[test]
    fn manage_other_guild_only_from_manager_guild() {
        // cross-guild: only when invoked in the manager guild and the gate passed
        assert!(can_manage_guild(9, true, Some(9), 2));
        assert!(!can_manage_guild(9, false, Some(9), 2), "manager gate member without perms");
        assert!(!can_manage_guild(1, true, Some(9), 2), "invoked outside the manager guild");
        assert!(!can_manage_guild(1, true, None, 2), "no manager guild configured");
    }

    #[test]
    fn resolve_target_parses_and_enforces() {
        assert_eq!(resolve_target(1, true, None, None), Ok(1));
        assert!(resolve_target(1, true, None, Some("abc")).is_err());
        assert_eq!(resolve_target(9, true, Some(9), Some("2")), Ok(2));
        assert!(resolve_target(1, true, None, Some("2")).is_err());
        assert!(resolve_target(1, false, None, None).is_err(), "gate failure blocks even own guild");
    }
}
