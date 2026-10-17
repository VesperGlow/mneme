mod common;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use mneme::{
    App, api,
    config::Config,
    domain::*,
    store::{BeginTurn, Store},
};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "continuity-test-token-01234567890123456789";
fn config(temp: &tempfile::TempDir, base: &str) -> Config {
    let mut c: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    c.database = temp.path().join("db").to_string_lossy().into();
    c.api_token = TOKEN.into();
    c.llm_key = "fixture".into();
    c.neo4j_password = "fixture".into();
    c.llm.base_url = base.into();
    c.models.base_url = base.into();
    c.neo4j.base_url = base.into();
    c.llm.model = "fixture-model".into();
    c.llm.history_turns = 2;
    c.llm.summary_chars = 500;
    c.memory.poll_seconds = 1;
    c.memory.extract_every_chars = 1;
    c
}
async fn request(router: &Router, method: &str, path: &str, body: Value) -> (StatusCode, String) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    (
        status,
        String::from_utf8(
            to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap(),
    )
}
async fn data(router: &Router, path: &str) -> Value {
    let (status, text) = request(router, "GET", path, json!({})).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    serde_json::from_str(&text).unwrap()
}
async fn chat(router: &Router, text: &str) -> String {
    let (status, wire) = request(
        router,
        "POST",
        "/v1/chat",
        json!({"request_id":id(),"message":text}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{wire}");
    wire
}
async fn seed(store: &Store, session: &str, text: &str) -> Message {
    let BeginTurn::Started(user) = store.begin_turn(session, &id(), text).await.unwrap() else {
        panic!()
    };
    store.complete_turn(&user, "已记下").await.unwrap();
    user
}

#[tokio::test]
async fn legacy_api_history_merges_once_and_all_clients_continue_after_restart() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, fake) = common::fake_services().await;
    let c = config(&temp, &base);
    let store = Store::open(&c.database).await.unwrap();
    let first = id();
    sqlx::query("INSERT INTO sessions(id,created_at) VALUES(?,?)")
        .bind(&first)
        .bind(now())
        .execute(&store.pool)
        .await
        .unwrap();
    let a = seed(&store, &first, "旧窗口的第一条消息").await;
    let second = id();
    sqlx::query("INSERT INTO sessions(id,created_at) VALUES(?,?)")
        .bind(&second)
        .bind(now())
        .execute(&store.pool)
        .await
        .unwrap();
    seed(&store, &second, "旧窗口的第二条消息").await;
    let qq = id();
    sqlx::query("INSERT INTO sessions(id,created_at) VALUES(?,?)")
        .bind(&qq)
        .bind(now())
        .execute(&store.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO qq_sessions(app_id,user_openid,session_id) VALUES('fixture','owner',?)",
    )
    .bind(&qq)
    .execute(&store.pool)
    .await
    .unwrap();
    seed(&store, &qq, "QQ 独立传输历史").await;
    let asset = id();
    sqlx::query("INSERT INTO assets(id,session_id,mime,data,width,height,kind,created_at) VALUES(?,?,'image/jpeg',X'FFD8FF',1,1,'upload',?)")
        .bind(&asset).bind(&second).bind(now()).execute(&store.pool).await.unwrap();
    store.pool.close().await;
    drop(store);
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c.clone(), rx.clone()).await.unwrap();
    let router = api::router(app.clone());
    let current = data(&router, "/v1/conversation").await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    for _ in 0..3 {
        let (status, text) = request(&router, "POST", "/v1/sessions", json!({})).await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["id"], current);
    }
    assert_eq!(
        data(&router, "/v1/sessions").await["sessions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let history = data(&router, "/v1/messages").await;
    assert_eq!(history["messages"].as_array().unwrap().len(), 6);
    assert!(history.to_string().contains("QQ 独立"));
    assert_eq!(
        data(&router, &format!("/v1/sessions/{second}/messages")).await["messages"],
        history["messages"]
    );
    let mapped: String = sqlx::query_scalar("SELECT session_id FROM assets WHERE id=?")
        .bind(asset)
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
    assert_eq!(mapped, current);
    let (_, replay) = request(
        &router,
        "POST",
        &format!("/v1/sessions/{first}/chat"),
        json!({"request_id":a.turn_id,"message":a.content}),
    )
    .await;
    assert!(replay.contains("\"replayed\":true"));
    drop(router);
    app.store.pool.close().await;
    drop(app);
    let app = App::new(c, rx).await.unwrap();
    let router = api::router(app.clone());
    assert_eq!(data(&router, "/v1/conversation").await["id"], current);
    assert_eq!(app.store.resolve_conversation(&qq).await.unwrap(), current);
    assert_eq!(app.store.history(&current, 100).await.unwrap().len(), 6);
    fake.abort();
}

#[tokio::test]
async fn compaction_preserves_summary_raw_evidence_and_durable_memory_jobs_across_restart() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, fake) = common::fake_services().await;
    let c = config(&temp, &base);
    c.validate().unwrap();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c.clone(), rx.clone()).await.unwrap();
    let router = api::router(app.clone());
    assert!(chat(&router, "我住在成都").await.contains("event: done"));
    assert!(
        chat(&router, "我们正在计划旅行，行程还没有完成")
            .await
            .contains("event: done")
    );
    let wire = chat(&router, "继续刚才的话题，我住在哪里？").await;
    assert!(
        wire.contains("event: compaction") && wire.contains("event: done"),
        "{wire}"
    );
    assert!(
        wire.contains("你住在成都"),
        "summary must be included before memory worker runs: {wire}"
    );
    assert!(app.store.active_memories(10).await.unwrap().is_empty());
    let before = data(&router, "/v1/conversation").await;
    assert_eq!(before["context"]["compactions"], 1);
    assert!(
        before["context"]["summary"]
            .as_str()
            .unwrap()
            .contains("成都")
    );
    assert_eq!(
        data(&router, "/v1/messages").await["messages"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    let jobs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE kind='extract'")
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
    assert_eq!(jobs, 3);
    drop(router);
    app.store.pool.close().await;
    drop(app);
    let app = App::new(c, rx).await.unwrap();
    let router = api::router(app.clone());
    assert_eq!(data(&router, "/v1/conversation").await, before);
    let worker = tokio::spawn(mneme::memory::worker(app.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if !app.store.active_memories(10).await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let memory = app.store.active_memories(10).await.unwrap().pop().unwrap();
    let sources = app.store.sources(&memory.id).await.unwrap();
    assert_eq!(sources[0]["quote"], "我住在成都");
    assert_eq!(sources[0]["content"], "我住在成都");
    stop.send(true).unwrap();
    worker.await.unwrap();
    fake.abort();
}

#[tokio::test]
async fn token_limit_compacts_before_turn_limit_and_failed_summary_keeps_watermark() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, fake) = common::fake_services().await;
    let mut c = config(&temp, &base);
    c.llm.history_turns = 100;
    c.llm.context_tokens = 12000;
    c.validate().unwrap();
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    let router = api::router(app.clone());
    let long = "字".repeat(4000);
    assert!(chat(&router, &long).await.contains("event: done"));
    assert!(chat(&router, &long).await.contains("event: done"));
    assert!(chat(&router, &long).await.contains("event: compaction"));
    assert!(
        data(&router, "/v1/conversation").await["context"]["compactions"]
            .as_i64()
            .unwrap()
            > 0
    );
    let id = app.store.conversation().await.unwrap();
    let mut tx = app.store.pool.begin().await.unwrap();
    sqlx::query("DELETE FROM conversation_context")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE messages SET content=? WHERE role='user'")
        .bind(format!("__summary_fail__{}", "字".repeat(3984)))
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let failed = chat(&router, &long).await;
    assert!(
        failed.contains("event: error") && !failed.contains("event: done"),
        "{failed}"
    );
    assert_eq!(
        data(&router, "/v1/conversation").await["context"]["compactions"],
        0
    );
    assert_eq!(app.store.history(&id, 100).await.unwrap().len(), 6);
    fake.abort();
}

#[tokio::test]
async fn global_context_batches_every_ten_thousand_characters_across_transports_and_restart() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, fake) = common::fake_services().await;
    let mut c = config(&temp, &base);
    c.llm.history_turns = 1000;
    c.memory.extract_every_chars = 10_000;
    assert_eq!(c.llm.context_tokens, 100_000);
    c.validate().unwrap();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c.clone(), rx.clone()).await.unwrap();
    let text = format!("我住在成都{}", "甲".repeat(3995));
    assert_eq!(text.chars().count(), 4000);
    let platform_id = id();
    app.store
        .qq_accept(
            "fixture",
            mneme::qq_store::Incoming {
                message_id: platform_id.clone(),
                sender: "owner".into(),
                content: text.clone(),
                expires_at: now() + 300,
            },
        )
        .await
        .unwrap();
    let session = app.store.conversation().await.unwrap();
    let qq_session: String = sqlx::query_scalar("SELECT session_id FROM qq_sessions")
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
    assert_eq!(qq_session, session);
    let request = mneme::qq_store::delivery_id("fixture", &platform_id);
    let BeginTurn::Started(qq_user) = app
        .store
        .begin_turn(&session, &request, &text)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(matches!(
        app.store
            .begin_turn(&session, &id(), "另一端的消息")
            .await
            .unwrap(),
        BeginTurn::Busy
    ));
    app.store.complete_turn(&qq_user, "已记住").await.unwrap();
    assert_eq!(app.store.extraction_pending_chars().await.unwrap(), 4003);
    assert_eq!(app.store.job_stats().await.unwrap()["pending"], 0);
    app.store.pool.close().await;
    drop(app);
    let app = App::new(c, rx).await.unwrap();
    assert_eq!(app.store.extraction_pending_chars().await.unwrap(), 4003);
    let BeginTurn::Started(web) = app
        .store
        .begin_turn(&session, &id(), &"乙".repeat(4000))
        .await
        .unwrap()
    else {
        panic!()
    };
    app.store
        .complete_turn(&web, &"答".repeat(1997))
        .await
        .unwrap();
    assert_eq!(app.store.extraction_pending_chars().await.unwrap(), 0);
    let jobs: Vec<(String, String)> =
        sqlx::query_as("SELECT id,payload FROM jobs WHERE kind='extract'")
            .fetch_all(&app.store.pool)
            .await
            .unwrap();
    assert_eq!(jobs.len(), 1);
    let payload: Value = serde_json::from_str(&jobs[0].1).unwrap();
    assert_eq!(payload["message_ids"], json!([qq_user.id, web.id]));
    assert!(app.store.active_memories(10).await.unwrap().is_empty());
    let router = api::router(app.clone());
    let wire = chat(&router, "我住在哪？").await;
    assert!(
        wire.contains("你住在成都"),
        "QQ context should reach API before extraction: {wire}"
    );
    let worker = tokio::spawn(mneme::memory::worker(app.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if !app.store.active_memories(10).await.unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let memory = app.store.active_memories(10).await.unwrap().pop().unwrap();
    let sources = app.store.sources(&memory.id).await.unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0]["message_id"], qq_user.id);
    assert_eq!(sources[0]["quote"], "我住在成都");
    assert_eq!(
        app.llm.metrics.snapshot().requests,
        2,
        "one generation and one batch extraction"
    );
    stop.send(true).unwrap();
    worker.await.unwrap();
    fake.abort();
}

#[tokio::test]
async fn token_budget_preserves_more_than_a_hundred_thousand_english_characters() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, fake) = common::fake_services().await;
    let mut c = config(&temp, &base);
    c.llm.history_turns = 1000;
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    let session = app.store.conversation().await.unwrap();
    let text = "word ".repeat(800);
    for _ in 0..30 {
        seed(&app.store, &session, &text).await;
    }
    let history = app.store.history(&session, 1000).await.unwrap();
    assert!(
        history
            .iter()
            .map(|m| m.content.chars().count())
            .sum::<usize>()
            > 100_000
    );
    let expected = history
        .iter()
        .map(|m| mneme::tokens::message(&m.content))
        .sum::<usize>();
    assert!(expected < 30_000);
    let router = api::router(app);
    let status = data(&router, "/v1/conversation").await;
    assert_eq!(status["context"]["context_used_tokens"], expected);
    assert_eq!(status["context"]["tokens_estimated"], true);
    let wire = chat(&router, "Continue our conversation.").await;
    assert!(wire.contains("event: done"), "{wire}");
    assert!(!wire.contains("event: compaction"));
    assert_eq!(
        data(&router, "/v1/conversation").await["context"]["compactions"],
        0
    );
    fake.abort();
}

#[tokio::test]
async fn hundred_thousand_context_does_not_compact_at_sixteen_turns() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, fake) = common::fake_services().await;
    let mut c = config(&temp, &base);
    c.llm.history_turns = 1000;
    c.memory.extract_every_chars = 10_000;
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    let router = api::router(app.clone());
    let message = "字".repeat(4000);
    for _ in 0..22 {
        assert!(chat(&router, &message).await.contains("event: done"));
    }
    assert_eq!(
        data(&router, "/v1/conversation").await["context"]["compactions"],
        0
    );
    let mut compacted = false;
    for _ in 0..5 {
        let wire = chat(&router, &message).await;
        assert!(wire.contains("event: done"), "{wire}");
        compacted |= wire.contains("event: compaction");
    }
    assert!(compacted);
    let state = data(&router, "/v1/conversation").await;
    assert_eq!(state["context"]["context_tokens"], 100_000);
    assert!(
        state["context"]["summary"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 2000
    );
    assert_eq!(
        data(&router, "/v1/messages").await["messages"]
            .as_array()
            .unwrap()
            .len(),
        54
    );
    fake.abort();
}

#[tokio::test]
async fn batch_with_fabricated_source_cannot_commit_facts_or_extraction_receipts() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, fake) = common::fake_services().await;
    let mut c = config(&temp, &base);
    c.memory.max_attempts = 1;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    let router = api::router(app.clone());
    assert!(
        chat(&router, "我住在成都 __bad_batch_source__")
            .await
            .contains("event: done")
    );
    let worker = tokio::spawn(mneme::memory::worker(app.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if app.store.job_stats().await.unwrap()["dead"] == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(app.store.active_memories(10).await.unwrap().is_empty());
    let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM extracted_messages")
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
    assert_eq!(receipts, 0);
    stop.send(true).unwrap();
    worker.await.unwrap();
    fake.abort();
}
