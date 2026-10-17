pub mod api;
pub mod chat;
pub mod config;
pub mod console;
pub mod console_store;
pub mod diagnostics;
pub mod domain;
pub mod graph;
pub mod llm;
pub mod memory;
pub mod models;
pub mod qq;
pub mod qq_store;
pub mod settings;
pub mod store;
pub mod telemetry;

use config::Config;
use sha2::{Digest, Sha256};
use std::sync::Arc;

#[derive(Clone)]
pub struct App {
    pub config: Arc<Config>,
    pub store: store::Store,
    pub llm: llm::Llm,
    pub models: models::Models,
    pub graph: graph::Graph,
    pub generation_slots: Arc<tokio::sync::Semaphore>,
    pub shutdown: tokio::sync::watch::Receiver<bool>,
    pub started_at: std::time::Instant,
    pub status_cache: Arc<tokio::sync::Mutex<Option<(std::time::Instant, serde_json::Value)>>>,
    pub qq: Option<qq::Client>,
    pub persona: Arc<std::sync::RwLock<config::Persona>>,
    pub console_security: Arc<tokio::sync::Mutex<settings::ConsoleSecurity>>,
    pub maintenance: Arc<tokio::sync::RwLock<()>>,
    pub settings_lock: Arc<tokio::sync::Mutex<()>>,
}

impl App {
    pub async fn new(
        mut config: Config,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<Self> {
        let store = store::Store::open(&config.database).await?;
        store.recover().await?;
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
        Ok(Self {
            llm: llm::Llm::new(&config)?,
            models: models::Models::new(&config)?,
            graph: graph::Graph::new(&config)?,
            qq: if config.qq.enabled() {
                Some(qq::Client::new(&config.qq)?)
            } else {
                None
            },
            generation_slots: Arc::new(tokio::sync::Semaphore::new(4)),
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
