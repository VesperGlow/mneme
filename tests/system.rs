mod common;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use common::fake_services;
use mneme::{
    App, api,
    config::Config,
    domain::*,
    memory,
    store::{BeginTurn, Store},
};
use serde_json::{Value, json};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::watch;
use tower::ServiceExt;

const TOKEN: &str = "test-token-012345678901234567890123456789";

fn config(temp: &TempDir, base: &str) -> Config {
    let mut config: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    config.memory.reconcile_updates = false; // This fixture does not implement semantic adjudication.
    config.memory.verify_recall = false;
    config.api_token = TOKEN.into();
    config.llm_key = "fake-key".into();
    config.neo4j_password = "fake-password".into();
    config.database = temp.path().join("mneme.sqlite3").to_str().unwrap().into();
    config.llm.model = "fixture-model".into();
    config.llm.base_url = base.into();
    config.models.base_url = base.into();
    config.neo4j.base_url = base.into();
    config.memory.poll_seconds = 1;
    config.memory.extract_every_tokens = 1;
    config.llm.context_tokens = 16000;
    config.llm.history_turns = 16;
    config.validate().unwrap();
    config
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
    let text = String::from_utf8(
        to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    (status, text)
}
async fn session(router: &Router) -> String {
    let (status, text) = request(router, "POST", "/v1/sessions", json!({})).await;
    assert_eq!(status, StatusCode::CREATED);
    serde_json::from_str::<Value>(&text).unwrap()["id"]
        .as_str()
        .unwrap()
        .into()
}
async fn drain(app: &App) {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let stats = app.store.job_stats().await.unwrap();
            assert_eq!(stats["dead"], 0);
            if stats["pending"] == 0 && stats["running"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("worker did not drain durable jobs");
}

#[tokio::test]
async fn cross_session_memory_survives_restart_with_pending_jobs() {
    let temp = TempDir::new().unwrap();
    let (base, fake) = fake_services().await;
    let cfg = config(&temp, &base);
    let (shutdown, rx) = watch::channel(false);
    let app = App::new(cfg.clone(), rx).await.unwrap();
    let router = api::router(app.clone());
    let first = session(&router).await;
    let turn = id();
    let input = json!({"request_id":turn,"message":"我住在成都。"});
    let (status, stream) = request(
        &router,
        "POST",
        &format!("/v1/sessions/{first}/chat"),
        input.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(stream.contains("event: done"));
    assert_eq!(app.store.job_stats().await.unwrap()["pending"], 1);
    // No worker has run: extraction is durably queued, independent of streaming latency.
    let (status, replay) = request(
        &router,
        "POST",
        &format!("/v1/sessions/{first}/chat"),
        input,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(replay.contains("\"replayed\":true"));
    assert_eq!(app.store.history(&first, 10).await.unwrap().len(), 2);
    shutdown.send(true).unwrap();
    app.store.pool.close().await;
    drop(router);
    drop(app);

    let (shutdown, rx) = watch::channel(false);
    let app = App::new(cfg, rx).await.unwrap();
    let worker = tokio::spawn(memory::worker(app.clone()));
    drain(&app).await;
    let stored = app.store.active_memories(10).await.unwrap();
    assert_eq!(stored.len(), 1);
    assert!(stored[0].content.contains("成都"));
    assert_eq!(
        app.store.sources(&stored[0].id).await.unwrap()[0]["quote"],
        "我住在成都"
    );
    let router = api::router(app.clone());
    let second = session(&router).await;
    let (_, stream) = request(
        &router,
        "POST",
        &format!("/v1/sessions/{second}/chat"),
        json!({"request_id":id(),"message":"我住在哪座城市？"}),
    )
    .await;
    assert!(stream.contains("你住在成都"), "{stream}");
    assert!(stream.contains(&stored[0].id));
    let (_, detail) = request(
        &router,
        "GET",
        &format!("/v1/memories/{}", stored[0].id),
        json!({}),
    )
    .await;
    assert!(detail.contains(&first));
    // Forgetting takes effect through SQLite immediately, before the graph worker catches up.
    request(
        &router,
        "DELETE",
        &format!("/v1/memories/{}", stored[0].id),
        json!({}),
    )
    .await;
    let (_, search) = request(
        &router,
        "POST",
        "/v1/memories/search",
        json!({"query":"我住在哪"}),
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Value>(&search).unwrap()["results"],
        json!([])
    );
    shutdown.send(true).unwrap();
    worker.await.unwrap();
    fake.abort();
}

#[tokio::test]
async fn auth_failed_stream_and_session_serialization() {
    let temp = TempDir::new().unwrap();
    let (base, fake) = fake_services().await;
    let (_shutdown, rx) = watch::channel(false);
    let app = App::new(config(&temp, &base), rx).await.unwrap();
    let router = api::router(app.clone());
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/sessions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let session = session(&router).await;
    let turn = id();
    let (_, stream) = request(
        &router,
        "POST",
        &format!("/v1/sessions/{session}/chat"),
        json!({"request_id":turn,"message":"请模拟断流"}),
    )
    .await;
    assert!(stream.contains("event: error"));
    assert!(!stream.contains("event: done"));
    assert!(app.store.history(&session, 10).await.unwrap().is_empty());
    assert_eq!(app.store.job_stats().await.unwrap()["pending"], 0);
    let (_, status) = request(&router, "GET", &format!("/v1/turns/{turn}"), json!({})).await;
    assert_eq!(
        serde_json::from_str::<Value>(&status).unwrap()["status"],
        "failed"
    );
    let request_id = id();
    assert!(matches!(
        app.store
            .begin_turn(&session, &request_id, "one")
            .await
            .unwrap(),
        BeginTurn::Started(_)
    ));
    assert!(matches!(
        app.store.begin_turn(&session, &id(), "two").await.unwrap(),
        BeginTurn::Busy
    ));
    assert!(matches!(
        app.store
            .begin_turn(&session, &request_id, "different")
            .await
            .unwrap(),
        BeginTurn::Conflict
    ));
    app.store.recover().await.unwrap();
    assert!(matches!(
        app.store
            .begin_turn(&session, &id(), "after restart")
            .await
            .unwrap(),
        BeginTurn::Started(_)
    ));
    fake.abort();
}

fn candidate(content: &str) -> Candidate {
    Candidate {
        key: "user.residence".into(),
        content: content.into(),
        kind: "fact".into(),
        importance: 0.8,
        confidence: 0.95,
        entities: vec!["城市".into()],
        source_quote: content.into(),
        valid_until: None,
        sensitive: false,
        equivalent_to: None,
        attribute_of: None,
        relation: Default::default(),
        supersedes: Vec::new(),
    }
}
async fn user(store: &Store, session: &str, content: &str) -> Message {
    let BeginTurn::Started(message) = store.begin_turn(session, &id(), content).await.unwrap()
    else {
        panic!()
    };
    store.complete_turn(&message, "收到").await.unwrap();
    message
}
#[tokio::test]
async fn dedup_updates_provenance_and_out_of_order_jobs_are_safe() {
    let temp = TempDir::new().unwrap();
    let store = Store::open(temp.path().join("db").to_str().unwrap())
        .await
        .unwrap();
    let session = store.create_session().await.unwrap();
    let a = user(&store, &session, "用户住在成都。").await;
    let apply = |message: Message, text: &str| {
        let store = store.clone();
        let c = candidate(text);
        async move {
            store
                .apply_candidates(
                    &message,
                    vec![(c, vec![1.0], None)],
                    &format!("extract:{}", message.turn_id),
                )
                .await
                .unwrap();
        }
    };
    apply(a.clone(), &a.content).await;
    let original = store.active_memories(10).await.unwrap().pop().unwrap();
    let b = user(&store, &session, "用户住在成都。").await;
    apply(b.clone(), &b.content).await;
    assert_eq!(store.active_memories(10).await.unwrap().len(), 1);
    assert_eq!(store.sources(&original.id).await.unwrap().len(), 2);
    let c = user(&store, &session, "用户住在杭州。").await;
    apply(c.clone(), &c.content).await;
    let current = store.active_memories(10).await.unwrap().pop().unwrap();
    assert_eq!(current.content, "用户住在杭州。");
    assert_eq!(
        store
            .memory(&original.id)
            .await
            .unwrap()
            .unwrap()
            .superseded_by,
        Some(current.id.clone())
    );
    // All turns can share a timestamp; SQLite's sequence still orders the observations.
    apply(a.clone(), &a.content).await;
    assert_eq!(store.active_memories(10).await.unwrap()[0].id, current.id);
    store.forget(&current.id).await.unwrap();
    apply(c.clone(), &c.content).await;
    assert!(
        store.active_memories(10).await.unwrap().is_empty(),
        "in-flight extraction must not undo forgetting"
    );
    let d = user(&store, &session, "用户住在杭州。").await;
    apply(d.clone(), &d.content).await;
    assert_eq!(
        store.active_memories(10).await.unwrap().len(),
        1,
        "a new explicit observation can be remembered"
    );
    let running = store.claim_job().await.unwrap().unwrap();
    store.pool.close().await;
    drop(store);
    let store = Store::open(temp.path().join("db").to_str().unwrap())
        .await
        .unwrap();
    store.recover().await.unwrap();
    let status: String = sqlx::query_scalar("SELECT status FROM jobs WHERE id=?")
        .bind(running.id)
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(status, "pending");
    sqlx::query("UPDATE memories SET updated_at=0")
        .execute(&store.pool)
        .await
        .unwrap();
    assert_eq!(store.expire(180.0, 0.12).await.unwrap(), 1);
    assert!(store.active_memories(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn single_writer_lock_and_dead_letter_retry() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("db");
    let store = Store::open(path.to_str().unwrap()).await.unwrap();
    assert!(Store::open(path.to_str().unwrap()).await.is_err());
    let session = store.create_session().await.unwrap();
    user(&store, &session, &"一个需要稍后提取的事实".repeat(1000)).await;
    let job = store.claim_job().await.unwrap().unwrap();
    store.retry_job(&job, 1).await.unwrap();
    assert_eq!(store.job_stats().await.unwrap()["dead"], 1);
    assert!(store.claim_job().await.unwrap().is_none());
    assert_eq!(store.retry_dead().await.unwrap(), 1);
    let retry = store.claim_job().await.unwrap().unwrap();
    assert_eq!(retry.attempts, 1);
    assert_eq!(retry.id, job.id);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn fabricated_sources_and_invalid_time_are_rejected() {
    let source = Message {
        id: id(),
        session_id: id(),
        turn_id: id(),
        role: "user".into(),
        content: "我住在成都".into(),
        created_at: now(),
    };
    let mut c = candidate("用户住在成都。");
    c.source_quote = "我住在杭州".into();
    assert!(memory::validate_candidate(&c, &source).is_err());
    c.source_quote = source.content.clone();
    c.valid_until = Some("昨天".into());
    assert!(memory::validate_candidate(&c, &source).is_err());
    c.valid_until = Some("2000-01-01T00:00:00+08:00".into());
    assert!(memory::validate_candidate(&c, &source).is_err());
    c.valid_until = None;
    assert!(memory::validate_candidate(&c, &source).is_ok());
}

#[test]
fn chinese_query_escapes_lucene_and_rrf_is_rank_based() {
    let query = mneme::graph::lucene_query("成都拿铁 OR *:* (密码) ");
    assert!(query.contains("\"成都\""));
    assert!(!query.contains('*'));
    assert!(!query.contains(':'));
    let ranked = memory::fuse(&[
        ("vector", vec!["a".into(), "b".into()]),
        ("keyword", vec!["b".into()]),
    ]);
    assert_eq!(ranked[0].0, "b");
    assert_eq!(ranked[0].2.len(), 2);
    assert_eq!(content_hash("我喜欢咖啡。"), content_hash("我 喜欢 咖啡!"));
    assert_ne!(
        content_hash("体重是60.5kg。"),
        content_hash("体重是605kg。")
    );
    assert_ne!(content_hash("喜欢A/B测试。"), content_hash("喜欢AB测试。"));
}
