use serenity::all::*;
use std::sync::Arc;
use std::time::Duration;
use sudobot_chat_guard::budget::{Budget, Provider};
use sudobot_chat_guard::config::{Config, Env};
use sudobot_chat_guard::discord::actions::DiscordSink;
use sudobot_chat_guard::discord::dispatch::Dispatcher;
use sudobot_chat_guard::discord::handler::{Handler, State};
use sudobot_chat_guard::discord::modlog::DiscordModLog;
use sudobot_chat_guard::pipeline::{Budgets, Deps, Pipeline};
use sudobot_chat_guard::ports::HttpImageFetcher;
use sudobot_chat_guard::providers::{jev::JevClient, ocr::OcrClient, paxmod::PaxmodClient};
use sudobot_chat_guard::store::Store;

fn warn_if_placeholder(name: &str, v: &sudobot_chat_guard::secret::Secret) {
    let s = v.expose();
    if s.is_empty() || s.contains("dummy") || s.contains("replace") {
        tracing::warn!("{name} looks like a placeholder: that provider will reject requests and the bot will run on local rules only");
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let env = Env::from_env()?;
    let cfg = Arc::new(Config::load(&env.config_path)?);
    warn_if_placeholder("PAXMOD_API_KEY", &env.paxmod_key);
    warn_if_placeholder("TYPESAFE_API_KEY", &env.typesafe_key);
    warn_if_placeholder("OCR_API_KEY", &env.ocr_key);

    if let Some(path) = env.database_url.strip_prefix("sqlite://") {
        if let Some(dir) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let store = Store::connect(&env.database_url).await?;

    let timeout = Duration::from_millis(env.provider_timeout_ms);
    let budgets = Budgets {
        paxmod: Arc::new(Budget::new(env.paxmod_cap, env.reserved_share)),
        jev: Arc::new(Budget::new(env.jev_cap, env.reserved_share)),
        ocr: Arc::new(Budget::new(env.ocr_cap, env.reserved_share)),
    };
    let now = chrono::Utc::now();
    for (p, b) in [(Provider::Paxmod, &budgets.paxmod), (Provider::Jev, &budgets.jev), (Provider::Ocr, &budgets.ocr)] {
        b.preload(store.usage_today(p.name(), now).await.0);
    }

    let handler = Arc::new(Handler::default());
    let intents = GatewayIntents::GUILDS
        | GatewayIntents::GUILD_MEMBERS
        | GatewayIntents::GUILD_MESSAGES
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILD_MODERATION;
    let mut client = Client::builder(env.discord_token.expose(), intents).event_handler_arc(handler.clone()).await?;

    let pipeline = Arc::new(Pipeline::new(Deps {
        cfg: cfg.clone(),
        store: store.clone(),
        paxmod: Arc::new(PaxmodClient::new(&env.paxmod_base, env.paxmod_key.clone(), timeout)),
        jev: Arc::new(JevClient::new(&env.typesafe_base, env.typesafe_key.clone(), &env.jev_model, timeout)),
        ocr: Arc::new(OcrClient::new(&env.ocr_base, env.ocr_key.clone(), env.ocr_engine, Duration::from_millis(env.ocr_timeout_ms))),
        fetcher: Arc::new(HttpImageFetcher::new()),
        sink: Arc::new(DiscordSink { http: client.http.clone(), cache: client.cache.clone() }),
        modlog: Arc::new(DiscordModLog { http: client.http.clone(), cfg: cfg.clone(), store: store.clone() }),
        budgets,
        ocr_max_upload: env.ocr_max_upload_bytes,
        roll: Arc::new(rand::random::<f64>),
    }));
    let dispatcher = Dispatcher::start(pipeline.clone(), cfg.runtime.workers, cfg.runtime.queue_size);
    if handler.state.set(State { pipeline: pipeline.clone(), dispatcher, cfg: cfg.clone(), manager_guild: env.manager_guild_id }).is_err() {
        anyhow::bail!("state already set");
    }

    // Maintenance: memory trimming and retention pruning.
    {
        let (p, s, days) = (pipeline.clone(), store.clone(), cfg.runtime.retention_days);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(600));
            loop {
                tick.tick().await;
                let now = chrono::Utc::now();
                p.gc(now);
                if let Err(e) = s.prune(days, now).await {
                    tracing::warn!("prune failed: {e}");
                }
            }
        });
    }

    tracing::info!("starting; new guilds default to Observe mode");
    client.start().await?;
    Ok(())
}
