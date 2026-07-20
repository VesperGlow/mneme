//! Mneme：单进程二进制 —— HTTP API + 进程内 embedding/rerank + SQLite 长期记忆 + QQ 桥接。

mod agent;
mod api;
mod cli;
mod config;
mod embedding;
mod fetch;
mod image;
mod llm;
mod mcp;
mod qq;
mod reranker;
mod shutdown;
mod store;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

// 异步任务都很轻（推理、SQLite 等重活全在 spawn_blocking 线程池里），
// 2 个 worker 足够，不必按核数起线程。
#[tokio::main(worker_threads = 2)]
async fn main() -> Result<()> {
    let cfg = Arc::new(config::Config::from_env()?);

    // 带参数即当作一次性子命令（如 `memory list`）：直接查库并退出，不启动服务、
    // 不初始化 embedding/QQ，也不打日志，保持输出干净。
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        return cli::run(&cfg, &args);
    }

    let level = cfg.log_level.to_lowercase();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(format!(
                "{level},hyper=warn,reqwest=warn,tungstenite=warn,ort=warn"
            ))
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // 优雅停机：SIGTERM/Ctrl-C 广播 + 在途写入追踪，退出前 flush 会话历史与记忆。
    let signal = shutdown::Signal::install();
    let pending = shutdown::Pending::default();

    let store = store::Store::open(cfg.clone())?;
    let embedder = Arc::new(embedding::Embedder::new(cfg.clone())?);
    let reranker = Arc::new(reranker::Reranker::new(cfg.clone()));
    let llm = Arc::new(llm::LlmClient::new(cfg.clone())?);
    let mut mcp = mcp::McpManager::new(cfg.clone())?;
    mcp.start().await?;
    let fetcher = Arc::new(fetch::Fetcher::new(
        cfg.fetch_timeout_seconds,
        cfg.fetch_max_bytes,
        cfg.fetch_result_max_chars,
    )?);
    let agent = agent::Agent::new(
        cfg.clone(),
        store.clone(),
        embedder.clone(),
        reranker.clone(),
        llm,
        Arc::new(mcp),
        fetcher,
        pending.clone(),
    );

    // 预热本地 embedding（首次启动含模型下载），不阻塞服务就绪。
    {
        let embedder = embedder.clone();
        tokio::spawn(async move {
            if let Err(error) = embedder.warmup().await {
                tracing::warn!("Embedding 预热失败：{error:#}");
            }
        });
    }

    // 预热重排模型（可选；下载/加载失败只告警，检索会回退到纯余弦）。
    if reranker.enabled() {
        let reranker = reranker.clone();
        tokio::spawn(async move {
            reranker.warmup().await;
        });
    }

    // HTTP API
    let state = api::AppState {
        cfg: cfg.clone(),
        agent: agent.clone(),
    };
    let api_addr = format!("0.0.0.0:{}", cfg.app_port);
    let listener = tokio::net::TcpListener::bind(&api_addr)
        .await
        .with_context(|| format!("监听 {api_addr} 失败"))?;
    tracing::info!("AI API 已启动: http://{api_addr}");
    let api_graceful = signal.subscribe();
    let api_server = tokio::spawn(async move {
        axum::serve(listener, api::router(state))
            .with_graceful_shutdown(api_graceful.wait())
            .await
    });

    // QQ 桥接（与 API 同进程；任一意外退出即整体退出，交给容器 restart 拉起）
    let bridge = qq::QqBridge::new(cfg.clone(), agent, signal.subscribe(), pending.clone())?;
    tokio::select! {
        // biased：停机信号优先于服务退出，避免优雅关闭被误判为“意外退出”。
        biased;
        _ = signal.subscribe().wait() => {}
        result = api_server => {
            result??;
            anyhow::bail!("AI API 服务意外退出");
        }
        result = bridge.run() => {
            result?;
            anyhow::bail!("QQ 桥接意外退出");
        }
    }

    // 停止接收新请求后，等在途消息处理与后台落库（历史/记忆/摘要/情绪）收尾。
    tracing::info!("正在优雅停机：等待在途消息与写入完成…");
    let grace = Duration::from_secs(cfg.shutdown_timeout_seconds);
    if tokio::time::timeout(grace, pending.wait_idle()).await.is_err() {
        tracing::warn!(
            "等待在途写入超过 {} 秒，放弃剩余任务退出",
            cfg.shutdown_timeout_seconds
        );
    }
    if let Err(error) = store.checkpoint().await {
        tracing::warn!("停机 WAL checkpoint 失败：{error:#}");
    }
    tracing::info!("已优雅退出");
    Ok(())
}
