pub mod api;
pub mod config;
pub mod console;
pub mod console_store;
pub mod domain;
pub mod graph;
pub mod llm;
pub mod memory;
pub mod models;
pub mod store;
pub mod telemetry;

use config::Config;
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
}

impl App {
    pub async fn new(
        config: Config,
        shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<Self> {
        let store = store::Store::open(&config.database).await?;
        store.recover().await?;
        Ok(Self {
            llm: llm::Llm::new(&config)?,
            models: models::Models::new(&config)?,
            graph: graph::Graph::new(&config)?,
            generation_slots: Arc::new(tokio::sync::Semaphore::new(4)),
            config: Arc::new(config),
            store,
            shutdown,
            started_at: std::time::Instant::now(),
            status_cache: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }
}
