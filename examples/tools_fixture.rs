//! Temporary end-to-end QA API. No production configuration, database or credentials.
#[path = "../tests/common/mod.rs"]
mod common;
use mneme::{App, config::Config};
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let temp = tempfile::TempDir::new()?;
    let (base, upstream) = common::fake_services().await;
    let mut config: Config = toml::from_str(include_str!("../config/default.toml"))?;
    config.database = temp.path().join("db").to_string_lossy().into();
    config.api_token = "console-test-token-01234567890123456789".into();
    config.llm_key = "tools-qa-llm-fixture-only".into();
    config.neo4j_password = "fixture".into();
    config.llm.model = "tools-fixture".into();
    config.models.base_url = base.clone();
    config.neo4j.base_url = base;
    config.llm.base_url = std::env::var("MNEME_QA_LLM_URL")?;
    config.tools.base_url = std::env::var("MNEME_QA_TOOLS_URL")?;
    config.tools.enabled = true;
    config.tools.token = "tools-qa-only-token-01234567890123456789".into();
    config.memory.poll_seconds = 1;
    config.validate()?;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config, rx).await?;
    let worker = tokio::spawn(mneme::memory::worker(app.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:18080").await?;
    println!("Tools QA fixture: http://127.0.0.1:18080/console/");
    axum::serve(listener, mneme::api::router(app))
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = stop.send(true);
        })
        .await?;
    worker.await?;
    upstream.abort();
    Ok(())
}
