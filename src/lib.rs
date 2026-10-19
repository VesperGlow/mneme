pub mod api;
pub mod assets;
pub mod chat;
pub mod config;
pub mod console;
pub mod console_store;
pub mod continuity;
pub mod diagnostics;
pub mod domain;
pub mod graph;
pub mod llm;
pub mod memory;
pub mod models;
pub mod qq;
pub mod qq_store;
pub mod settings;
pub mod stickers;
pub mod store;
pub mod telemetry;
pub mod tokens;
pub mod tools;
pub mod usage;

use config::Config;
use sha2::{Digest, Sha256};
use std::sync::Arc;

#[derive(Clone)]
pub struct App {
    pub config: Arc<Config>,
    pub store: store::Store,
    pub llm: llm::Llm,
    pub models: models::Models,
    pub tokens: tokens::Counter,
    pub graph: graph::Graph,
    pub generation_slots: Arc<tokio::sync::Semaphore>,
    pub shutdown: tokio::sync::watch::Receiver<bool>,
    pub started_at: std::time::Instant,
    pub status_cache: Arc<tokio::sync::Mutex<Option<(std::time::Instant, serde_json::Value)>>>,
    pub qq: Option<qq::Client>,
    pub stickers: Arc<std::sync::RwLock<stickers::Catalog>>,
    pub persona: Arc<std::sync::RwLock<config::Persona>>,
    pub console_security: Arc<tokio::sync::Mutex<settings::ConsoleSecurity>>,
    pub maintenance: Arc<tokio::sync::RwLock<()>>,
    pub settings_lock: Arc<tokio::sync::Mutex<()>>,
    pub tools: Option<tools::Client>,
}

impl App {
    pub async fn new(
        mut config: Config,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<Self> {
        let mut store = store::Store::open(&config.database).await?;
        store.extract_every_tokens = config.memory.extract_every_tokens;
        store.extract_max_delay_seconds = config.memory.extract_max_delay_seconds;
        store.recover().await?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions)")
            .fetch_one(&store.pool)
            .await?;
        if exists {
            store.conversation().await?;
        }
        let (persona, security) = settings::load(&store, &config).await?;
        if config.llm.model == "auto" {
            let key = format!(
                "llm.model.{:x}",
                Sha256::digest(config.llm.base_url.as_bytes())
            );
            let cached: Option<String> =
                sqlx::query_scalar("SELECT value FROM metadata WHERE key=?")
                    .bind(&key)
                    .fetch_optional(&store.pool)
                    .await?;
            if let Some(model) = cached {
                config.llm.model = model;
            } else {
                crate::llm::resolve_model(&mut config).await?;
                sqlx::query("INSERT INTO metadata(key,value) VALUES(?,?)")
                    .bind(key)
                    .bind(&config.llm.model)
                    .execute(&store.pool)
                    .await?;
            }
        }
        let tokens = tokens::Counter::new(&config.llm.model, config.llm.tokenizer_path.as_deref())?;
        store.tokens = tokens.clone();
        Ok(Self {
            tools: if config.tools.enabled {
                Some(tools::Client::new(&config.tools)?)
            } else {
                None
            },
            llm: llm::Llm::new(&config)?.tracked(store.clone()),
            models: models::Models::new(&config)?.tracked(store.clone()),
            tokens,
            graph: graph::Graph::new(&config)?,
            qq: if config.qq.enabled() {
                Some(qq::Client::new(&config.qq)?)
            } else {
                None
            },
            generation_slots: Arc::new(tokio::sync::Semaphore::new(4)),
            stickers: Arc::new(std::sync::RwLock::new(stickers::load(&store).await?)),
            persona: Arc::new(std::sync::RwLock::new(persona)),
            console_security: Arc::new(tokio::sync::Mutex::new(security)),
            maintenance: Default::default(),
            settings_lock: Default::default(),
            config: Arc::new(config),
            store,
            shutdown,
            started_at: std::time::Instant::now(),
            status_cache: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }
}
