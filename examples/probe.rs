use sudobot_chat_guard::config::Config;
use sudobot_chat_guard::rules::{evaluate, MessageFacts};

fn main() -> anyhow::Result<()> {
    let cfg = Config::load("config.toml")?;
    let text = std::env::args().nth(1).unwrap_or_else(|| {
        "FREE NITRO for everyone!! claim now at https://bit.ly/3xYz discord.gg/freenitro-now @everyone".into()
    });
    for gid in [807713145006850048u64, 1528495239843680461, 1] {
        let g = cfg.guild(gid);
        println!("guild {gid}: blocked_domains={:?} allowed_invites={:?} weights.blocked={} weights.invite={} weights.everyone={}",
            g.blocked_domains, g.allowed_invites, g.rules.weights.blocked_domain, g.rules.weights.invite_link, g.rules.weights.everyone_attempt);
        let f = MessageFacts {
            text: &text,
            channel_id: 10,
            now: chrono::Utc::now(),
            distinct_mentions: 0,
            mentions_everyone_text: text.contains("@everyone"),
            can_mention_everyone: false,
            is_first_message: false,
        };
        let r = evaluate(&f, &[], &g.rules, g);
        println!("  score={:.4} signals={:?}", r.score, r.signals.iter().map(|s| format!("{}:{:.2}", s.name, s.weight)).collect::<Vec<_>>());
    }
    Ok(())
}
