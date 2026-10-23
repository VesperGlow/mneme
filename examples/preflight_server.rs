//! Opt-in, local-only acceptance server. Never reads .env or the production config.
use anyhow::{Result, ensure};
use mneme::{App, config::Config};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let root = PathBuf::from(std::env::var("MNEME_PREFLIGHT_ROOT")?).canonicalize()?;
    ensure!(
        root.join(".mneme-preflight-owned").is_file(),
        "missing isolation marker"
    );
    let path = PathBuf::from(std::env::var("MNEME_PREFLIGHT_CONFIG")?).canonicalize()?;
    ensure!(
        path.starts_with(&root),
        "config outside owned test directory"
    );
    let mut config: Config = toml::from_str(&std::fs::read_to_string(path)?)?;
    ensure!(
        config.listen.starts_with("127.0.0.1:"),
        "test server must use loopback"
    );
    ensure!(
        PathBuf::from(&config.database)
            .parent()
            .unwrap()
            .canonicalize()?
            .starts_with(&root),
        "database outside owned test directory"
    );
    ensure!(
        [
            &config.llm.base_url,
            &config.models.base_url,
            &config.neo4j.base_url
        ]
        .iter()
        .all(|url| url.starts_with("http://127.0.0.1:")),
        "upstreams must be local test relays"
    );
    config.api_token = "mneme-preflight-only-01234567890123456789".into();
    config.llm_key = "local-relay-only".into();
    config.neo4j_password = "mneme-preflight-only-20261023".into();
    config.validate()?;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config, rx).await?;
    let worker = tokio::spawn(mneme::memory::worker(app.clone()));
    let listener = tokio::net::TcpListener::bind(&app.config.listen).await?;
    axum::serve(listener, mneme::api::router(app))
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = stop.send(true);
        })
        .await?;
    worker.await?;
    Ok(())
}
