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
    if std::env::args().nth(1).as_deref() == Some("--check-qq") {
        anyhow::ensure!(config.qq.enabled(), "QQ is not configured");
        let client = mneme::qq::Client::new(&config.qq)?;
        let result = client.check_token().await;
        let failure = result.as_ref().err().map(mneme::diagnostics::describe);
        println!(
            "{}",
            serde_json::json!({"qq_token":result.is_ok(),"transport":client.transport(),"failure":failure})
        );
        anyhow::ensure!(
            result.is_ok(),
            "QQ connectivity check failed; see diagnostic result"
        );
        return Ok(());
    }
    let listener = tokio::net::TcpListener::bind(&config.listen).await?;
    let qq_listener = if config.qq.enabled() {
        Some(tokio::net::TcpListener::bind(&config.qq.listen).await?)
    } else {
        None
    };
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let app = App::new(config, shutdown_rx.clone()).await?;
    if app.qq.is_some() {
        tracing::info!(listen=%app.config.qq.listen, "QQ webhook enabled; first private message starts the personal session");
    }
    let worker = tokio::spawn(mneme::memory::worker(app.clone()));
    let qq_worker = tokio::spawn(mneme::qq::worker(app.clone()));
    let sticker_worker = tokio::spawn(mneme::stickers::worker(app.clone()));
    let mut qq_shutdown = shutdown_rx.clone();
    let qq_server = qq_listener.map(|listener| {
        let router = mneme::qq::router(app.clone());
        tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    while !*qq_shutdown.borrow() && qq_shutdown.changed().await.is_ok() {}
                })
                .await
        })
    });
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
        let _ = qq_worker.await;
        let _ = sticker_worker.await;
        if let Some(server) = qq_server {
            let _ = server.await;
        }
        let _all = slots.acquire_many(4).await;
    })
    .await;
    Ok(())
}
