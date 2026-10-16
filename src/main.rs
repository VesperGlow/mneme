use anyhow::Result;
use mneme::{App, config::Config};
use std::{path::PathBuf, time::Duration};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mneme=info".into()),
        )
        .init();
    let path = std::env::var_os("MNEME_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|| "config/default.toml".into());
    let config = Config::load(&path)?;
    let listener = tokio::net::TcpListener::bind(&config.listen).await?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let app = App::new(config, shutdown_rx.clone()).await?;
    let worker = tokio::spawn(mneme::memory::worker(app.clone()));
    let slots = app.generation_slots.clone();
    let server =
        axum::serve(listener, mneme::api::router(app)).with_graceful_shutdown(async move {
            #[cfg(unix)]
            {
                let mut terminate =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("signal handler");
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            }
            #[cfg(not(unix))]
            tokio::signal::ctrl_c().await.expect("signal handler");
            let _ = shutdown_tx.send(true);
        });
    tracing::info!("mneme started");
    server.await?;
    let _ = tokio::time::timeout(Duration::from_secs(15), async {
        let _ = worker.await;
        let _all = slots.acquire_many(4).await;
    })
    .await;
    Ok(())
}
