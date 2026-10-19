//! Local QA only: real Axum and SQLite with deterministic upstream HTTP fixtures.
#[path = "../tests/common/mod.rs"]
mod common;
use mneme::{App, config::Config, console_store::MemoryWrite};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let temp = tempfile::TempDir::new()?;
    let (base, upstream) = common::fake_services().await;
    let mut config: Config = toml::from_str(include_str!("../config/default.toml"))?;
    config.console.password = "console-fixture-password".into();
    config.llm.history_turns = 2;
    config.llm.model = "fixture-model".into();
    config.api_token = "console-test-token-01234567890123456789".into();
    config.llm_key = "fixture-only".into();
    config.neo4j_password = "fixture-only".into();
    config.database = temp.path().join("db").to_string_lossy().into();
    config.llm.base_url = base.clone();
    config.models.base_url = base.clone();
    config.neo4j.base_url = base;
    config.memory.poll_seconds = 1;
    if std::env::var("MNEME_QQ_UI_TEST").as_deref() == Ok("1") {
        config.qq.app_id = "12345678".into();
        config.qq.app_secret = "console-qq-fixture-only".into();
    }
    if let Ok(base) = std::env::var("MNEME_TOOLS_UI_URL") {
        config.tools.enabled = true;
        config.tools.base_url = base;
        config.tools.token = "console-tools-fixture-token-0123456789".into();
    }
    config.validate()?;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config, rx).await?;
    if std::env::var("MNEME_QQ_HISTORY_UI_TEST").as_deref() == Ok("1") {
        app.store
            .qq_accept(
                "history-fixture",
                mneme::qq_store::Incoming {
                    message_id: mneme::domain::id(),
                    sender: "fixture-owner".into(),
                    content: "来自 QQ 的既有聊天记录 <img src=x onerror=\"window.__qq_xss=1\">"
                        .into(),
                    expires_at: mneme::domain::now() + 300,
                },
            )
            .await?;
        let session: String =
            sqlx::query_scalar("SELECT session_id FROM qq_sessions WHERE app_id='history-fixture'")
                .fetch_one(&app.store.pool)
                .await?;
        let mneme::store::BeginTurn::Started(user) = app
            .store
            .begin_turn(
                &session,
                &sqlx::query_scalar::<_, String>(
                    "SELECT id FROM qq_inbox WHERE app_id='history-fixture'",
                )
                .fetch_one(&app.store.pool)
                .await?,
                "来自 QQ 的既有聊天记录 <img src=x onerror=\"window.__qq_xss=1\">",
            )
            .await?
        else {
            anyhow::bail!("fixture turn could not start")
        };
        app.store
            .complete_turn(&user, "这是 QQ 中已保存的完整回复。")
            .await?;
    }
    for (content, kind, importance, entities, expired) in [
        (
            "用户目前住在成都，喜欢这座城市慢下来的生活节奏。",
            "fact",
            0.9,
            vec!["成都"],
            false,
        ),
        (
            "用户喜欢不加糖的拿铁，通常在上午喝咖啡。",
            "preference",
            0.8,
            vec!["咖啡", "拿铁"],
            false,
        ),
        (
            "用户最近在开发一个 Rust 项目，重视代码的可维护性。",
            "event",
            0.8,
            vec!["Rust", "编程"],
            false,
        ),
        (
            "用户的朋友小林喜欢骑行，他们偶尔一起去河边散步。",
            "relationship",
            0.7,
            vec!["小林", "骑行"],
            false,
        ),
        (
            "用户觉得傍晚散步可以缓解工作中的紧张感。",
            "emotion",
            0.7,
            vec!["散步"],
            false,
        ),
        (
            "用户希望回答具体、自然，尽量少用不必要的术语。",
            "preference",
            0.9,
            vec!["交流方式"],
            false,
        ),
        (
            "用户通常在周六整理一周的读书笔记。",
            "fact",
            0.7,
            vec!["读书", "周六"],
            false,
        ),
        (
            "用户上周末参加过一次分享会。",
            "event",
            0.6,
            vec!["分享会"],
            true,
        ),
    ] {
        app.store
            .manual_memory(
                None,
                &MemoryWrite {
                    content: content.into(),
                    kind: kind.into(),
                    importance,
                    confidence: 0.95,
                    entities: entities.into_iter().map(str::to_owned).collect(),
                    valid_until: expired.then(|| mneme::domain::now() - 100),
                    expected_revision: None,
                },
            )
            .await?;
    }
    let worker = tokio::spawn(mneme::memory::worker(app.clone()));
    let sticker_worker = tokio::spawn(mneme::stickers::worker(app.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:18080").await?;
    println!("Console QA fixture: http://127.0.0.1:18080/console/");
    axum::serve(listener, mneme::api::router(app))
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = stop.send(true);
        })
        .await?;
    worker.await?;
    sticker_worker.await?;
    upstream.abort();
    Ok(())
}
