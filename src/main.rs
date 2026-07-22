//! Mneme：单进程二进制 —— HTTP API + SQLite 长期记忆 + QQ 桥接。
//! 模型只接 DeepSeek 官方 API，且全局只用一个模型（对话开思考、记忆侧关思考）。
//! 记忆检索不用向量、通常也不用额外的模型调用：候选池装得下就整池挂进主模型的
//! system 段（见 `agent::Agent::recall_for_context`），装不下才降级到精选。
//! 进程内不再有任何本地模型推理。

mod agent;
mod api;
mod cli;
mod config;
mod fetch;
mod llm;
mod mcp;
mod qq;
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
    // 不初始化 QQ，也不打日志，保持输出干净。
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        return cli::run(&cfg, &args);
    }

    let level = cfg.log_level.to_lowercase();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_new(format!(
                "{level},hyper=warn,reqwest=warn,tungstenite=warn"
            ))
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // 优雅停机：SIGTERM/Ctrl-C 广播 + 在途写入追踪，退出前 flush 会话历史与记忆。
    let signal = shutdown::Signal::install();
    let pending = shutdown::Pending::default();

    let store = store::Store::open(cfg.clone())?;
    let llm = Arc::new(llm::LlmClient::new(cfg.clone())?);
    let mut mcp = mcp::McpManager::new(cfg.clone())?;
    mcp.start().await?;
    let fetcher = Arc::new(fetch::Fetcher::new(
        config::FETCH_TIMEOUT_SECONDS,
        config::FETCH_MAX_BYTES,
        config::FETCH_RESULT_MAX_CHARS,
    )?);
    let agent = agent::Agent::new(
        cfg.clone(),
        store.clone(),
        llm,
        Arc::new(mcp),
        fetcher,
        pending.clone(),
    );

    // HTTP API
    let state = api::AppState {
        cfg: cfg.clone(),
        agent: agent.clone(),
    };
    let api_addr = format!("0.0.0.0:{}", config::API_PORT);
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

    // 记忆尾巴 flush：定时把空闲会话里仍未巩固的尾巴也巩固掉（补 eviction 触发的盲区）。
    {
        let agent = agent.clone();
        let shutdown = signal.subscribe();
        tokio::spawn(async move {
            agent.run_memory_flush_loop(shutdown).await;
        });
    }

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
    let grace = Duration::from_secs(config::SHUTDOWN_TIMEOUT_SECONDS);
    if tokio::time::timeout(grace, pending.wait_idle()).await.is_err() {
        tracing::warn!(
            "等待在途写入超过 {} 秒，放弃剩余任务退出",
            config::SHUTDOWN_TIMEOUT_SECONDS
        );
    }
    if let Err(error) = store.checkpoint().await {
        tracing::warn!("停机 WAL checkpoint 失败：{error:#}");
    }
    tracing::info!("已优雅退出");
    Ok(())
}
