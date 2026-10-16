//! Discord events -> platform-independent structs.

use crate::imaging::{is_allowed_host, looks_like_image};
use crate::model::MemberProfile;
use crate::pipeline::IncomingMessage;
use chrono::{DateTime, Utc};
use serenity::all::*;
use serenity::cache::Cache;

pub fn to_utc(ts: Timestamp) -> DateTime<Utc> {
    DateTime::from_timestamp(ts.unix_timestamp(), 0).unwrap_or_else(Utc::now)
}

/// Role permissions that make someone a moderator for the bot's purposes.
const MOD_PERMS: Permissions = Permissions::ADMINISTRATOR
    .union(Permissions::MANAGE_GUILD)
    .union(Permissions::MANAGE_MESSAGES)
    .union(Permissions::KICK_MEMBERS)
    .union(Permissions::BAN_MEMBERS)
    .union(Permissions::MODERATE_MEMBERS);

pub struct RoleFacts {
    pub is_owner: bool,
    pub is_mod: bool,
    pub can_mention_everyone: bool,
    pub has_trusted_role: bool,
}

/// Reads cached guild data synchronously (no awaits while the cache guard is held).
pub fn role_facts(cache: &Cache, guild: GuildId, user: UserId, roles: &[RoleId], trusted_roles: &[u64]) -> Option<RoleFacts> {
    let g = cache.guild(guild)?;
    let everyone = g.roles.get(&RoleId::new(guild.get())).map(|r| r.permissions).unwrap_or(Permissions::empty());
    let mut perms = everyone;
    for r in roles {
        if let Some(role) = g.roles.get(r) {
            perms |= role.permissions;
        }
    }
    let admin = perms.contains(Permissions::ADMINISTRATOR);
    Some(RoleFacts {
        is_owner: g.owner_id == user,
        is_mod: perms.intersects(MOD_PERMS),
        can_mention_everyone: admin || perms.contains(Permissions::MENTION_EVERYONE),
        has_trusted_role: roles.iter().any(|r| trusted_roles.contains(&r.get())),
    })
}

pub fn image_urls(attachments: &[Attachment], embeds: &[Embed]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for a in attachments {
        if looks_like_image(a.content_type.as_deref(), &a.filename) && is_allowed_host(&a.url) {
            out.push(a.url.clone());
        }
    }
    for e in embeds {
        // proxy_url is on Discord's CDN; the original URL is attacker-controlled.
        for img in [e.image.as_ref().and_then(|i| i.proxy_url.clone()), e.thumbnail.as_ref().and_then(|i| i.proxy_url.clone())].into_iter().flatten() {
            if is_allowed_host(&img) && !out.contains(&img) {
                out.push(img);
            }
        }
    }
    out
}

pub struct Parts<'a> {
    pub guild_id: GuildId,
    pub channel_id: ChannelId,
    pub message_id: MessageId,
    pub author: &'a User,
    pub roles: &'a [RoleId],
    pub joined_at: Option<Timestamp>,
    pub content: &'a str,
    pub attachments: &'a [Attachment],
    pub embeds: &'a [Embed],
    pub distinct_mentions: usize,
    pub mention_everyone: bool,
    pub is_edit: bool,
    pub created_at: DateTime<Utc>,
}

pub fn build(cache: &Cache, trusted_roles: &[u64], p: Parts) -> Option<IncomingMessage> {
    let facts = role_facts(cache, p.guild_id, p.author.id, p.roles, trusted_roles)?;
    let urls = image_urls(p.attachments, p.embeds);
    if p.content.trim().is_empty() && urls.is_empty() {
        return None;
    }
    let text_everyone = p.content.contains("@everyone") || p.content.contains("@here") || p.mention_everyone;
    Some(IncomingMessage {
        guild_id: p.guild_id.get(),
        channel_id: p.channel_id.get(),
        message_id: p.message_id.get(),
        author_id: p.author.id.get(),
        content: p.content.to_string(),
        created_at: p.created_at,
        image_urls: urls,
        distinct_mentions: p.distinct_mentions,
        mentions_everyone_text: text_everyone,
        can_mention_everyone: facts.can_mention_everyone,
        is_edit: p.is_edit,
        profile: MemberProfile {
            guild_id: p.guild_id.get(),
            user_id: p.author.id.get(),
            account_created: to_utc(p.author.id.created_at()),
            joined_at: p.joined_at.map(to_utc),
            is_trusted: facts.is_owner || facts.is_mod || facts.has_trusted_role,
            has_default_avatar: p.author.avatar.is_none(),
            role_count: p.roles.len(),
        },
    })
}
