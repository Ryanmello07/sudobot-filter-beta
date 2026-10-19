//! `/guard status|mode` and the mod-log buttons.

use crate::model::Mode;
use crate::pipeline::{Pipeline, Release};
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
            CreateCommandOption::new(CommandOptionType::SubCommand, "safe", "Allowlist a member: stop scanning them and clear their strikes")
                .add_sub_option(CreateCommandOption::new(CommandOptionType::User, "user", "member to mark safe").required(true))
                .add_sub_option(CreateCommandOption::new(CommandOptionType::String, "note", "optional note for the mod log").required(false))
                .add_sub_option(guild_opt()),
        )
        .add_option(
            CreateCommandOption::new(CommandOptionType::SubCommand, "unsafe", "Remove a member from the allowlist (resume normal scanning)")
                .add_sub_option(CreateCommandOption::new(CommandOptionType::User, "user", "member to remove").required(true))
                .add_sub_option(guild_opt()),
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
        "safe" => safe_cmd(ctx, c, p, sub, gid.get(), manager_guild, true).await,
        "unsafe" => safe_cmd(ctx, c, p, sub, gid.get(), manager_guild, false).await,
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

/// Refuse nonsensical allowlist targets: the guild owner (unactionable anyway) and
/// the bot itself (never scanned). `owner` is None when the guild isn't cached.
fn allowlist_target_ok(target: u64, owner: Option<u64>, bot: u64) -> Result<(), String> {
    if Some(target) == owner {
        return Err("That's the server owner — they're effectively trusted already.".into());
    }
    if target == bot {
        return Err("That's me — I don't get scanned.".into());
    }
    Ok(())
}

/// `/guard safe` / `/guard unsafe`: per-guild member allowlist management.
async fn safe_cmd(ctx: &Context, c: &CommandInteraction, p: &Pipeline, sub: &CommandDataOption, invoked: u64, manager_guild: Option<u64>, safe: bool) {
    let opts = match &sub.value {
        CommandDataOptionValue::SubCommand(o) => o.as_slice(),
        _ => &[],
    };
    let user = opts.iter().find(|o| o.name == "user").and_then(|o| match o.value {
        CommandDataOptionValue::User(u) => Some(u),
        _ => None,
    });
    let Some(user) = user else { return reply_cmd(ctx, c, "Missing user.".into()).await };
    let note = opts.iter().find(|o| o.name == "note").and_then(|o| o.value.as_str());
    let guild_opt = opts.iter().find(|o| o.name == "guild").and_then(|o| o.value.as_str());
    let target = match resolve_target(invoked, true, manager_guild, guild_opt) {
        Ok(t) => t,
        Err(e) => return reply_cmd(ctx, c, e).await,
    };
    let scope = if target == invoked { "this server".to_string() } else { format!("server `{target}`") };
    let (uid, who) = (user.get(), c.user.id.get());

    if safe {
        // The owner check needs the guild in cache; cross-guild manager use may miss it.
        // Copy plain values out of the cache guards before any await (they're !Send).
        let owner = ctx.cache.guild(GuildId::new(target)).map(|g| g.owner_id.get());
        let me = ctx.cache.current_user().id.get();
        if let Err(e) = allowlist_target_ok(uid, owner, me) {
            return reply_cmd(ctx, c, e).await;
        }
        if p.store().is_allowlisted(target, uid).await {
            return reply_cmd(ctx, c, format!("<@{uid}> is already on the allowlist for {scope}.")).await;
        }
        if let Err(e) = p.store().set_allowlisted(target, uid, true).await {
            return reply_cmd(ctx, c, format!("Could not save: {e}")).await;
        }
        let cleared = p.store().clear_strikes(target, uid, chrono::Utc::now()).await.unwrap_or(0);
        let note_txt = note.map(|n| format!(" — {n}")).unwrap_or_default();
        p.log_notice(target, &format!("🛡️ <@{uid}> marked safe by <@{who}> (allowlisted, {cleared} active strike(s) cleared){note_txt}")).await;
        reply_cmd(ctx, c, format!("<@{uid}> is now on the allowlist for {scope}: no longer scanned, {cleared} active strike(s) cleared.{note_txt}")).await;
    } else {
        if !p.store().is_allowlisted(target, uid).await {
            return reply_cmd(ctx, c, format!("<@{uid}> is not on the allowlist for {scope}.")).await;
        }
        if let Err(e) = p.store().set_allowlisted(target, uid, false).await {
            return reply_cmd(ctx, c, format!("Could not save: {e}")).await;
        }
        p.log_notice(target, &format!("🔎 <@{uid}> removed from the allowlist by <@{who}>; normal scanning applies.")).await;
        reply_cmd(ctx, c, format!("<@{uid}> is no longer on the allowlist for {scope}; normal scanning applies.")).await;
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

/// Gate for the manual mod-log buttons (ban/kick/timeout). Pure: the guild match
/// (S4) is checked by the caller, and the hierarchy/owner check result comes in as
/// `can_act`. Returns the action name on success, or a user-facing error message.
fn manual_action_gate(
    kind: &str,
    admin: bool,
    perms: Permissions,
    action_taken: &str,
    outcome: &str,
    can_act: &Result<(), String>,
) -> Result<&'static str, String> {
    let (action, perm, perm_name) = match kind {
        "ban" => ("ban", Permissions::BAN_MEMBERS, "Ban Members"),
        "kick" => ("kick", Permissions::KICK_MEMBERS, "Kick Members"),
        "timeout" => ("timeout", Permissions::MODERATE_MEMBERS, "Moderate Members"),
        _ => return Err("Unknown action.".into()),
    };
    if matches!(action_taken, "ban" | "kick") || outcome.starts_with("manual ") {
        return Err("This incident was already actioned.".into());
    }
    if kind == "timeout" && action_taken == "delete+timeout" {
        return Err("This member was already timed out by the bot.".into());
    }
    if !admin && !perms.contains(perm) {
        return Err(format!("You need the {perm_name} permission to {action} members."));
    }
    if let Err(why) = can_act {
        return Err(format!("Cannot {action} that member: {why}."));
    }
    Ok(action)
}

pub async fn component(ctx: &Context, c: &ComponentInteraction, p: &Pipeline) {
    let Some((kind, id)) = c.data.custom_id.split_once(':') else { return };
    let Ok(id) = id.parse::<i64>() else { return };
    let perms = c.member.as_ref().and_then(|m| m.permissions).unwrap_or(Permissions::empty());
    let admin = perms.contains(Permissions::ADMINISTRATOR);
    let Some(inc) = p.store().incident(id).await.ok().flatten() else {
        return reply_comp(ctx, c, "That incident is no longer stored.").await;
    };
    // Buttons travel with the message; only act when the click happened in the
    // guild the incident belongs to.
    if !guild_matches(c.guild_id.map(|g| g.get()), inc.guild_id) {
        return reply_comp(ctx, c, "That incident belongs to a different server.").await;
    }
    let gid = GuildId::new(inc.guild_id);
    let uid = UserId::new(inc.user_id);
    match kind {
        "ban" | "kick" | "timeout" => {
            let can = p.check_can_act(inc.guild_id, inc.user_id).await;
            let action = match manual_action_gate(kind, admin, perms, &inc.action_taken, &inc.outcome, &can) {
                Ok(a) => a,
                Err(e) => return reply_comp(ctx, c, &e).await,
            };
            let reason = format!("chat guard: manual {action} via mod-log (incident #{id}, {} {:.2})", inc.label, inc.score);
            let gc = p.effective_guild_config(inc.guild_id).await;
            let res: Result<String, String> = match action {
                "ban" => {
                    // serenity deletes history in whole days; any positive setting becomes one day.
                    let days = if gc.ladder.ban_delete_seconds > 0 { 1 } else { 0 };
                    gid.ban_with_reason(&ctx.http, uid, days, &reason).await.map(|_| "Banned".to_string()).map_err(|e| e.to_string())
                }
                "kick" => gid.kick_with_reason(&ctx.http, uid, &reason).await.map(|_| "Kicked".to_string()).map_err(|e| e.to_string()),
                _ => {
                    let mins = gc.ladder.timeout_minutes;
                    let until = chrono::Utc::now() + chrono::Duration::minutes(mins as i64);
                    match Timestamp::from_unix_timestamp(until.timestamp()) {
                        Ok(ts) => gid
                            .edit_member(&ctx.http, uid, EditMember::new().disable_communication_until_datetime(ts).audit_log_reason(&reason))
                            .await
                            .map(|_| format!("Timed out for {mins} minutes"))
                            .map_err(|e| e.to_string()),
                        Err(e) => Err(e.to_string()),
                    }
                }
            };
            match res {
                Ok(what) => {
                    let who = c.user.id.get();
                    let _ = p.store().set_outcome(id, &format!("manual {action} by {who}")).await;
                    // Annotate the original embed message (the embed itself is untouched).
                    let msg = &c.message;
                    let _ = msg
                        .channel_id
                        .edit_message(&ctx.http, msg.id, EditMessage::new().content(format!("{what} manually by <@{who}> (incident #{id})")))
                        .await;
                    reply_comp(ctx, c, &format!("{what} <@{}>.", inc.user_id)).await
                }
                Err(e) => reply_comp(ctx, c, &format!("Could not {action}: {e}")).await,
            }
        }
        "safe" => {
            if !(admin || perms.contains(Permissions::MANAGE_GUILD)) {
                return reply_comp(ctx, c, "You need the Manage Server permission.").await;
            }
            let _ = p.store().set_allowlisted(inc.guild_id, inc.user_id, true).await;
            let _ = p.store().clear_strikes_for_incident(id).await;
            let _ = p.store().set_outcome(id, &format!("marked safe by {}", c.user.id.get())).await;
            let release = p.lift_bot_timeout(&inc, c.user.id.get()).await;
            reply_comp(ctx, c, &safe_reply(&release)).await
        }
        // "undo" was removed: the manual Ban/Kick/Timeout buttons plus Mark safe
        // are the mod toolbox.
        _ => {}
    }
}

fn safe_reply(r: &Release) -> String {
    match r {
        Release::NotTimedOut => "Member added to the allowlist and the strike from this incident cleared.".into(),
        Release::Lifted => "Member added to the allowlist, the strike from this incident cleared, and the timeout lifted.".into(),
        Release::Failed(e) => format!("Member added to the allowlist and the strike cleared, but the timeout could not be lifted ({e}). They may have left, or the timeout already expired."),
    }
}

/// S4: a component interaction only applies to an incident of the same guild.
fn guild_matches(clicked_in: Option<u64>, incident_guild: u64) -> bool {
    clicked_in == Some(incident_guild)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_guild_must_match_incident_guild() {
        assert!(guild_matches(Some(1), 1));
        assert!(!guild_matches(Some(2), 1), "click in another guild");
        assert!(!guild_matches(None, 1), "DM or unknown context");
    }

    #[test]
    fn safe_reply_texts() {
        assert_eq!(safe_reply(&Release::NotTimedOut), "Member added to the allowlist and the strike from this incident cleared.");
        assert_eq!(safe_reply(&Release::Lifted), "Member added to the allowlist, the strike from this incident cleared, and the timeout lifted.");
        let failed = safe_reply(&Release::Failed("unknown member".into()));
        assert!(failed.contains("could not be lifted"), "{failed}");
        assert!(failed.contains("unknown member"), "{failed}");
    }

    #[test]
    fn manual_action_gate_branches() {
        let ok: Result<(), String> = Ok(());
        let owner: Result<(), String> = Err("target is the server owner".into());
        // happy paths: exact permission, and admin override
        assert_eq!(manual_action_gate("ban", false, Permissions::BAN_MEMBERS, "flag", "open", &ok), Ok("ban"));
        assert_eq!(manual_action_gate("kick", false, Permissions::KICK_MEMBERS, "flag", "open", &ok), Ok("kick"));
        assert_eq!(manual_action_gate("timeout", false, Permissions::MODERATE_MEMBERS, "flag", "open", &ok), Ok("timeout"));
        assert_eq!(manual_action_gate("ban", true, Permissions::empty(), "flag", "open", &ok), Ok("ban"), "administrator overrides");
        // lacking permission
        let e = manual_action_gate("ban", false, Permissions::empty(), "flag", "open", &ok).unwrap_err();
        assert!(e.contains("Ban Members"), "{e}");
        assert!(manual_action_gate("kick", false, Permissions::BAN_MEMBERS, "flag", "open", &ok).is_err(), "wrong permission");
        // already actioned, by the bot or manually
        let e = manual_action_gate("ban", true, Permissions::empty(), "ban", "open", &ok).unwrap_err();
        assert!(e.contains("already actioned"), "{e}");
        assert!(manual_action_gate("kick", true, Permissions::empty(), "flag", "manual ban by 9", &ok).unwrap_err().contains("already actioned"));
        assert!(manual_action_gate("timeout", true, Permissions::empty(), "delete+timeout", "open", &ok).unwrap_err().contains("already timed out"));
        assert_eq!(manual_action_gate("ban", true, Permissions::empty(), "delete+timeout", "open", &ok), Ok("ban"), "ban allowed after auto-timeout");
        assert_eq!(manual_action_gate("kick", true, Permissions::empty(), "delete+timeout", "open", &ok), Ok("kick"), "kick allowed after auto-timeout");
        assert!(manual_action_gate("ban", true, Permissions::empty(), "would-have", "observe", &ok).is_ok(), "observe outcome is not final");
        // hierarchy / owner target (from can_act)
        let e = manual_action_gate("ban", true, Permissions::empty(), "flag", "open", &owner).unwrap_err();
        assert!(e.contains("server owner"), "{e}");
        // unknown kind
        assert!(manual_action_gate("smite", true, Permissions::ADMINISTRATOR, "flag", "open", &ok).is_err());
    }

    #[test]
    fn allowlist_target_guards() {
        assert!(allowlist_target_ok(7, Some(1), 2).is_ok(), "ordinary member");
        assert!(allowlist_target_ok(1, Some(1), 2).unwrap_err().contains("server owner"));
        assert!(allowlist_target_ok(2, Some(1), 2).unwrap_err().contains("I don't get scanned"));
        assert!(allowlist_target_ok(1, None, 2).is_ok(), "guild not cached: owner check skipped");
    }

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
