use crate::ports::ActionSink;
use async_trait::async_trait;
use serenity::all::*;
use std::sync::Arc;

pub struct DiscordSink {
    pub http: Arc<Http>,
    pub cache: Arc<Cache>,
}

fn highest_position(roles: &std::collections::HashMap<RoleId, Role>, member_roles: &[RoleId]) -> u16 {
    member_roles.iter().filter_map(|r| roles.get(r)).map(|r| r.position).max().unwrap_or(0)
}

#[async_trait]
impl ActionSink for DiscordSink {
    async fn can_act(&self, guild: u64, user: u64) -> Result<(), String> {
        let gid = GuildId::new(guild);
        let uid = UserId::new(user);
        let me = self.cache.current_user().id;
        // Plain values copied out of the cache guard before any await.
        let (owner, roles) = match self.cache.guild(gid) {
            Some(g) => (g.owner_id, g.roles.clone()),
            None => return Err("guild not in cache".into()),
        };
        if owner == uid {
            return Err("target is the server owner".into());
        }
        let target = gid.member(&self.http, uid).await.map_err(|e| format!("cannot load member: {e}"))?;
        let bot = gid.member(&self.http, me).await.map_err(|e| format!("cannot load bot member: {e}"))?;
        if highest_position(&roles, &target.roles) >= highest_position(&roles, &bot.roles) {
            return Err("target's highest role is not below the bot's".into());
        }
        Ok(())
    }

    async fn delete(&self, channel: u64, message: u64) -> anyhow::Result<()> {
        self.http
            .delete_message(ChannelId::new(channel), MessageId::new(message), Some("sudobot-chat-guard"))
            .await?;
        Ok(())
    }

    async fn bulk_delete(&self, channel: u64, messages: &[u64]) -> anyhow::Result<()> {
        let ch = ChannelId::new(channel);
        match messages {
            [] => {}
            [one] => self.http.delete_message(ch, MessageId::new(*one), Some("sudobot-chat-guard sweep")).await?,
            many => {
                let ids: Vec<MessageId> = many.iter().map(|m| MessageId::new(*m)).collect();
                ch.delete_messages(&self.http, ids).await?;
            }
        }
        Ok(())
    }

    async fn timeout(&self, guild: u64, user: u64, minutes: u32) -> anyhow::Result<()> {
        let until = chrono::Utc::now() + chrono::Duration::minutes(minutes as i64);
        let ts = Timestamp::from_unix_timestamp(until.timestamp())?;
        GuildId::new(guild)
            .edit_member(&self.http, UserId::new(user), EditMember::new().disable_communication_until_datetime(ts).audit_log_reason("sudobot-chat-guard"))
            .await?;
        Ok(())
    }

    async fn kick(&self, guild: u64, user: u64, reason: &str) -> anyhow::Result<()> {
        GuildId::new(guild).kick_with_reason(&self.http, UserId::new(user), reason).await?;
        Ok(())
    }

    async fn ban(&self, guild: u64, user: u64, delete_seconds: u32, reason: &str) -> anyhow::Result<()> {
        // serenity deletes history in whole days; any positive setting becomes one day.
        let days = if delete_seconds > 0 { 1 } else { 0 };
        GuildId::new(guild).ban_with_reason(&self.http, UserId::new(user), days, reason).await?;
        Ok(())
    }
}
