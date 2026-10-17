use super::convert::{build, to_utc, Parts};
use super::dispatch::Dispatcher;
use super::interact;
use crate::config::Config;
use crate::model::MemberProfile;
use crate::pipeline::{JoinEvent, Pipeline};
use crate::risk::base_tier;
use async_trait::async_trait;
use serenity::all::*;
use std::sync::{Arc, OnceLock};

pub struct State {
    pub pipeline: Arc<Pipeline>,
    pub dispatcher: Dispatcher,
    pub cfg: Arc<Config>,
}

#[derive(Default)]
pub struct Handler {
    pub state: OnceLock<State>,
}

fn regular(kind: MessageType) -> bool {
    matches!(kind, MessageType::Regular | MessageType::InlineReply)
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        tracing::info!("connected as {} in {} guild(s)", ready.user.name, ready.guilds.len());
        for g in &ready.guilds {
            interact::register(&ctx, g.id).await;
        }
    }

    async fn guild_create(&self, ctx: Context, guild: Guild, is_new: Option<bool>) {
        if is_new == Some(true) {
            interact::register(&ctx, guild.id).await;
        }
    }

    async fn message(&self, ctx: Context, msg: Message) {
        let Some(st) = self.state.get() else { return };
        let Some(gid) = msg.guild_id else { return };
        if msg.author.bot || msg.webhook_id.is_some() || !regular(msg.kind) {
            return;
        }
        let (roles, joined) = match &msg.member {
            Some(m) => (m.roles.clone(), m.joined_at),
            None => (vec![], None),
        };
        let trusted = &st.cfg.guild(gid.get()).trusted_role_ids;
        let Some(m) = build(
            &ctx.cache,
            trusted,
            Parts {
                guild_id: gid,
                channel_id: msg.channel_id,
                message_id: msg.id,
                author: &msg.author,
                roles: &roles,
                joined_at: joined,
                content: &msg.content,
                attachments: &msg.attachments,
                embeds: &msg.embeds,
                distinct_mentions: msg.mentions.len() + msg.mention_roles.len(),
                mention_everyone: msg.mention_everyone,
                is_edit: false,
                created_at: to_utc(msg.timestamp),
            },
        ) else {
            return;
        };
        let tier = base_tier(&m.profile, &st.cfg.guild(gid.get()).tiers, chrono::Utc::now());
        st.dispatcher.submit(m, tier).await;
    }

    async fn message_update(&self, ctx: Context, _old: Option<Message>, _new: Option<Message>, ev: MessageUpdateEvent) {
        let Some(st) = self.state.get() else { return };
        let Some(gid) = ev.guild_id else { return };
        // Spammers edit links in after posting. Partial updates are skipped.
        let (Some(content), Some(author), Some(member)) = (ev.content.as_ref(), ev.author.as_ref(), ev.member.as_ref().and_then(|m| m.as_deref())) else {
            return;
        };
        if author.bot {
            return;
        }
        let empty_a: Vec<Attachment> = vec![];
        let empty_e: Vec<Embed> = vec![];
        let trusted = &st.cfg.guild(gid.get()).trusted_role_ids;
        let created = ev.timestamp.map(to_utc).unwrap_or_else(chrono::Utc::now);
        let Some(m) = build(
            &ctx.cache,
            trusted,
            Parts {
                guild_id: gid,
                channel_id: ev.channel_id,
                message_id: ev.id,
                author,
                roles: &member.roles,
                joined_at: member.joined_at,
                content,
                attachments: ev.attachments.as_ref().unwrap_or(&empty_a),
                embeds: ev.embeds.as_ref().unwrap_or(&empty_e),
                distinct_mentions: ev.mentions.as_ref().map(|m| m.len()).unwrap_or(0) + ev.mention_roles.as_ref().map(|m| m.len()).unwrap_or(0),
                mention_everyone: ev.mention_everyone.unwrap_or(false),
                is_edit: true,
                created_at: created,
            },
        ) else {
            return;
        };
        let tier = base_tier(&m.profile, &st.cfg.guild(gid.get()).tiers, chrono::Utc::now());
        st.dispatcher.submit(m, tier).await;
    }

    async fn guild_member_addition(&self, _ctx: Context, member: Member) {
        let Some(st) = self.state.get() else { return };
        if member.user.bot {
            return;
        }
        let ev = JoinEvent {
            guild_id: member.guild_id.get(),
            user_id: member.user.id.get(),
            username: member.user.name.clone(),
            display_name: member.user.global_name.clone().or_else(|| member.nick.clone()).unwrap_or_default(),
            profile: MemberProfile {
                guild_id: member.guild_id.get(),
                user_id: member.user.id.get(),
                account_created: to_utc(member.user.id.created_at()),
                joined_at: member.joined_at.map(to_utc),
                is_trusted: false,
                has_default_avatar: member.user.avatar.is_none(),
                role_count: member.roles.len(),
            },
        };
        let p = st.pipeline.clone();
        tokio::spawn(async move {
            p.handle_join(ev, chrono::Utc::now()).await;
        });
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        let Some(st) = self.state.get() else { return };
        match interaction {
            Interaction::Command(c) if c.data.name == "guard" => interact::command(&ctx, &c, &st.pipeline, &st.dispatcher.shed).await,
            Interaction::Component(c) => interact::component(&ctx, &c, &st.pipeline).await,
            _ => {}
        }
    }
}
