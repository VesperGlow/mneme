mod common;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use mneme::{
    App, api,
    config::Config,
    console_store::{MemoryWrite, WriteResult},
    domain::*,
    store::{BeginTurn, Store},
};
use serde_json::{Value, json};
use tower::ServiceExt;

const TOKEN: &str = "console-test-token-01234567890123456789";
fn config(temp: &tempfile::TempDir, base: &str) -> Config {
    let mut config: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    config.memory.reconcile_updates = false; // This fixture does not implement semantic adjudication.
    config.memory.verify_recall = false;
    config.api_token = TOKEN.into();
    config.llm_key = "fixture".into();
    config.neo4j_password = "fixture".into();
    config.database = temp.path().join("db").to_string_lossy().into();
    config.models.base_url = base.into();
    config.neo4j.base_url = base.into();
    config.llm.model = "fixture-model".into();
    config.llm.base_url = base.into();
    config.memory.poll_seconds = 1;
    config.memory.extract_every_tokens = 1;
    config.llm.context_tokens = 16000;
    config.llm.history_turns = 16;
    config.validate().unwrap();
    config
}
async fn call(router: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
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
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
fn input(content: &str) -> Value {
    json!({"content":content,"kind":"fact","importance":0.9,"confidence":1.0,"entities":["成都"],"valid_until":null})
}
async fn app(
    temp: &tempfile::TempDir,
) -> (
    App,
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<()>,
) {
    let (base, fake) = common::fake_services().await;
    let (stop, rx) = tokio::sync::watch::channel(false);
    (App::new(config(temp, &base), rx).await.unwrap(), stop, fake)
}

#[tokio::test]
async fn crud_statistics_filters_history_and_auth() {
    let temp = tempfile::TempDir::new().unwrap();
    let (app, _stop, fake) = app(&temp).await;
    let router = api::router(app.clone());
    for path in [
        "/v1/console/auth",
        "/v1/console/stats",
        "/v1/console/messages",
        "/v1/console/system",
        "/v1/memories",
    ] {
        let response = router
            .clone()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    let page = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/console/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert_eq!(page.headers()["cache-control"], "no-store");
    assert!(
        page.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/console/auth")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("sec-fetch-site", "cross-site")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let (status, created) = call(&router, "POST", "/v1/memories", input("用户住在成都。")).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["memory"]["id"].as_str().unwrap();
    assert_eq!(created["memory"]["origin"], "manual");
    assert_eq!(created["index_update"], "queued");
    assert_eq!(
        call(&router, "POST", "/v1/memories", input("用户住在成都。"))
            .await
            .0,
        StatusCode::CONFLICT
    );
    let mut edited = input("用户现在住在杭州。");
    edited["expected_revision"] = json!(1);
    let (status, saved) = call(
        &router,
        "PUT",
        &format!("/v1/memories/{id}"),
        edited.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(saved["memory"]["revision"], 2);
    assert_eq!(
        call(&router, "PUT", &format!("/v1/memories/{id}"), edited)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let (_, detail) = call(&router, "GET", &format!("/v1/memories/{id}"), json!({})).await;
    assert_eq!(detail["versions"].as_array().unwrap().len(), 2);
    assert_eq!(
        detail["versions"][1]["snapshot"]["content"],
        "用户住在成都。"
    );
    assert_eq!(
        call(
            &router,
            "GET",
            "/v1/memories?q=杭州&status=active&kind=fact&sort=importance",
            json!({})
        )
        .await
        .1["total"],
        1
    );
    assert_eq!(
        call(&router, "GET", "/v1/memories?q=%27%20OR%201%3D1", json!({}))
            .await
            .1["total"],
        0
    );
    assert_eq!(
        call(
            &router,
            "GET",
            "/v1/memories?sort=content%3BDROP",
            json!({})
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    let mut expired = input("暂时有时效的安排");
    expired["valid_until"] = json!(now() - 10);
    expired["kind"] = json!("event");
    call(&router, "POST", "/v1/memories", expired).await;
    let (_, stats) = call(&router, "GET", "/v1/console/stats", json!({})).await;
    assert_eq!(stats["total"], 2);
    assert_eq!(stats["active"], 1);
    assert_eq!(stats["invalid"], 1);
    assert_eq!(stats["kinds"]["event"], 1);
    assert_eq!(stats["recent_7_days"], 2);
    assert_eq!(
        call(&router, "GET", "/v1/memories?status=invalid", json!({}))
            .await
            .1["total"],
        1
    );
    assert_eq!(
        call(
            &router,
            "DELETE",
            &format!("/v1/memories/{id}?expected_revision=1"),
            json!({})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        call(
            &router,
            "DELETE",
            &format!("/v1/memories/{id}?expected_revision=2"),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        call(&router, "GET", "/v1/memories?status=deleted", json!({}))
            .await
            .1["total"],
        1
    );
    let (_, system) = call(&router, "GET", "/v1/console/system", json!({})).await;
    assert_eq!(system["services"]["llm"]["state"], "healthy");
    assert_eq!(system["services"]["embedding"]["state"], "healthy");
    assert!(!system.to_string().contains(TOKEN));
    assert!(!system.to_string().contains("password"));
    assert_eq!(
        call(&router, "GET", "/v1/console/system", json!({}))
            .await
            .1["cached"],
        true
    );
    fake.abort();
}

#[tokio::test]
async fn manual_changes_defeat_stale_extractors_and_embedding_writes() {
    let temp = tempfile::TempDir::new().unwrap();
    let (app, _stop, fake) = app(&temp).await;
    let session = app.store.create_session().await.unwrap();
    let BeginTurn::Started(user) = app
        .store
        .begin_turn(&session, &id(), "我住在成都")
        .await
        .unwrap()
    else {
        panic!()
    };
    app.store.complete_turn(&user, "记住了").await.unwrap();
    let original = mneme::domain::Candidate {
        key: "user.residence".into(),
        content: "用户住在成都。".into(),
        kind: "fact".into(),
        importance: 0.9,
        confidence: 1.0,
        entities: vec!["成都".into()],
        source_quote: user.content.clone(),
        valid_until: None,
        sensitive: false,
        equivalent_to: None,
        attribute_of: None,
        relation: Default::default(),
        supersedes: Vec::new(),
    };
    app.store
        .apply_candidates(&user, vec![(original.clone(), vec![1.0], None)], "initial")
        .await
        .unwrap();
    let BeginTurn::Started(new_source) = app
        .store
        .begin_turn(&session, &id(), "我现在住在北京")
        .await
        .unwrap()
    else {
        panic!()
    };
    app.store
        .complete_turn(&new_source, "收到更新")
        .await
        .unwrap();
    let mut replacement = original.clone();
    replacement.content = "用户现在住在北京。".into();
    replacement.source_quote = new_source.content.clone();
    app.store
        .apply_candidates(
            &new_source,
            vec![(replacement, vec![1.0], None)],
            "replacement",
        )
        .await
        .unwrap();
    let memory = app.store.active_memories(10).await.unwrap().remove(0);
    let mut edit: MemoryWrite = serde_json::from_value(input("用户现在住在杭州。")).unwrap();
    edit.expected_revision = Some(memory.revision);
    assert!(matches!(
        app.store
            .manual_memory(Some(&memory.id), &edit)
            .await
            .unwrap(),
        WriteResult::Saved(_)
    ));
    // An old job may rename the slot and paraphrase the fact; its original source is suppressed.
    let mut drifted = original.clone();
    drifted.key = "user.home".into();
    drifted.content = "用户的住所是成都".into();
    app.store
        .apply_candidates(&user, vec![(drifted, vec![1.0], None)], "retry")
        .await
        .unwrap();
    let current = app.store.active_memories(10).await.unwrap();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].content, "用户现在住在杭州。");
    assert!(
        !app.store
            .save_embedding(&memory.id, 1, &vec![1.0; 1024])
            .await
            .unwrap()
    );
    app.store.delete_memory(&memory.id, Some(2)).await.unwrap();
    assert!(
        !app.store
            .save_embedding(&memory.id, 2, &vec![1.0; 1024])
            .await
            .unwrap()
    );
    app.store
        .apply_candidates(
            &user,
            vec![(original, vec![1.0], None)],
            "retry-after-delete",
        )
        .await
        .unwrap();
    assert!(app.store.active_memories(10).await.unwrap().is_empty());
    let path = app.config.database.clone();
    app.store.pool.close().await;
    drop(app);
    let store = Store::open(&path).await.unwrap();
    store.recover().await.unwrap();
    assert!(
        store
            .memory(&memory.id)
            .await
            .unwrap()
            .unwrap()
            .deleted_at
            .is_some()
    );
    assert_eq!(store.memory_versions(&memory.id).await.unwrap().len(), 5);
    fake.abort();
}

#[tokio::test]
async fn projection_debug_and_deletion_share_real_pipeline() {
    let temp = tempfile::TempDir::new().unwrap();
    let (app, stop, fake) = app(&temp).await;
    let router = api::router(app.clone());
    let (_, created) = call(&router, "POST", "/v1/memories", input("用户住在成都。")).await;
    let id = created["memory"]["id"].as_str().unwrap();
    let worker = tokio::spawn(mneme::memory::worker(app.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(8), async {
        loop {
            let memory = app.store.memory(id).await.unwrap().unwrap();
            if memory.indexed_revision == memory.revision {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let (_, debug) = call(
        &router,
        "POST",
        "/v1/console/retrieval",
        json!({"query":"我住在哪里"}),
    )
    .await;
    assert_eq!(debug["degraded"], false);
    assert_eq!(debug["results"][0]["memory"]["id"], id);
    assert_eq!(debug["trace"]["channels"][0]["hits"][0]["score"], 0.87);
    assert_eq!(debug["trace"]["candidates"][0]["rerank_score"], 0.95);
    assert_eq!(debug["trace"]["candidates"][0]["rerank_rank"], 1);
    assert_eq!(
        debug["trace"]["candidates"][0]["final_score"],
        debug["results"][0]["score"]
    );
    call(&router, "DELETE", &format!("/v1/memories/{id}"), json!({})).await;
    app.store.reindex().await.unwrap();
    app.store.retry_dead().await.unwrap();
    assert!(
        call(
            &router,
            "POST",
            "/v1/console/retrieval",
            json!({"query":"我住在哪里"})
        )
        .await
        .1["results"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    stop.send(true).unwrap();
    worker.await.unwrap();
    fake.abort();
}

#[tokio::test]
async fn migration_from_v1_preserves_existing_data() {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    let temp = tempfile::TempDir::new().unwrap();
    let migrations = temp.path().join("old-migrations");
    std::fs::create_dir(&migrations).unwrap();
    std::fs::write(
        migrations.join("0001_initial.sql"),
        include_str!("../migrations/0001_initial.sql"),
    )
    .unwrap();
    let path = temp.path().join("db");
    let pool = SqlitePoolOptions::new()
        .connect_with(
            SqliteConnectOptions::new()
                .filename(&path)
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::migrate::Migrator::new(migrations)
        .await
        .unwrap()
        .run(&pool)
        .await
        .unwrap();
    let id = id();
    sqlx::query("INSERT INTO memories(id,slot,content,content_hash,kind,importance,confidence,entities,embedding,status,created_at,updated_at,source_seq) VALUES(?,'user.residence','旧版本保存的住所','hash','fact',0.9,1.0,'[]','[]','active',1,1,1)").bind(&id).execute(&pool).await.unwrap();
    pool.close().await;
    let store = Store::open(path.to_str().unwrap()).await.unwrap();
    let old = store.memory(&id).await.unwrap().unwrap();
    assert_eq!(old.content, "旧版本保存的住所");
    assert_eq!(old.origin, "extracted");
    assert_eq!(store.memory_versions(&id).await.unwrap().len(), 1);
    assert_eq!(
        store.job_stats().await.unwrap()["pending"],
        1,
        "old projections must be queued during upgrade"
    );
}

#[tokio::test]
async fn deletion_during_reranking_also_redacts_rejected_debug_candidates() {
    use axum::{Json, routing::post};
    use std::sync::Arc;
    let temp = tempfile::TempDir::new().unwrap();
    let (base, fake) = common::fake_services().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let (e, r) = (entered.clone(), release.clone());
    let slow = Router::new()
        .route(
            "/embed",
            post(|| async {
                let mut v = vec![0.0; 1024];
                v[0] = 1.0;
                Json(json!({"embeddings":[v]}))
            }),
        )
        .route(
            "/rerank",
            post(move || {
                let (e, r) = (e.clone(), r.clone());
                async move {
                    e.notify_one();
                    r.notified().await;
                    Json(json!({"scores":[0.01]}))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = config(&temp, &base);
    config.models.base_url = format!("http://{}", listener.local_addr().unwrap());
    let model = tokio::spawn(async move { axum::serve(listener, slow).await.unwrap() });
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config, rx).await.unwrap();
    let router = api::router(app.clone());
    let (_, created) = call(&router, "POST", "/v1/memories", input("用户住在成都。")).await;
    let id = created["memory"]["id"].as_str().unwrap().to_owned();
    let search = tokio::spawn(async move {
        call(
            &router,
            "POST",
            "/v1/console/retrieval",
            json!({"query":"我住在成都吗"}),
        )
        .await
        .1
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();
    app.store.delete_memory(&id, None).await.unwrap();
    release.notify_one();
    let result = search.await.unwrap();
    assert!(result["results"].as_array().unwrap().is_empty());
    assert!(result["trace"]["candidates"][0]["content"].is_null());
    assert_eq!(
        result["trace"]["candidates"][0]["reason"],
        "changed_during_search"
    );
    fake.abort();
    model.abort();
}

#[tokio::test]
async fn console_transcript_includes_qq_and_web_in_the_shared_context() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, fake) = common::fake_services().await;
    let config = config(&temp, &base);
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config.clone(), rx.clone()).await.unwrap();
    let router = api::router(app.clone());
    let api_session = app.store.conversation().await.unwrap();
    // QQ and the API must now point at the same context, even when API was opened first.
    app.store
        .qq_accept(
            "fixture",
            mneme::qq_store::Incoming {
                message_id: id(),
                sender: "owner".into(),
                content: "来自 QQ 的旧消息".into(),
                expires_at: now() + 300,
            },
        )
        .await
        .unwrap();
    let old: String =
        sqlx::query_scalar("SELECT session_id FROM qq_sessions WHERE app_id='fixture'")
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
    let BeginTurn::Started(qq_user) = app
        .store
        .begin_turn(
            &old,
            &sqlx::query_scalar::<_, String>(
                "SELECT id FROM qq_inbox WHERE app_id='fixture' ORDER BY seq LIMIT 1",
            )
            .fetch_one(&app.store.pool)
            .await
            .unwrap(),
            "来自 QQ 的旧消息",
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    app.store
        .complete_turn(&qq_user, "这是 QQ 回复")
        .await
        .unwrap();
    assert_eq!(
        call(&router, "GET", "/v1/messages", json!({})).await.1["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let history = call(&router, "GET", "/v1/console/messages", json!({}))
        .await
        .1;
    assert_eq!(history["messages"].as_array().unwrap().len(), 2);
    assert_eq!(history["messages"][0]["content"], "来自 QQ 的旧消息");
    assert_eq!(history["messages"][1]["channel"], "qq");
    // The obsolete command cannot fork the global context.
    app.store
        .qq_accept(
            "fixture",
            mneme::qq_store::Incoming {
                message_id: id(),
                sender: "owner".into(),
                content: "/新会话".into(),
                expires_at: now() + 300,
            },
        )
        .await
        .unwrap();
    let current: String =
        sqlx::query_scalar("SELECT session_id FROM qq_sessions WHERE app_id='fixture'")
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
    assert_eq!(old, current);
    let BeginTurn::Started(failed) = app
        .store
        .begin_turn(
            &current,
            &sqlx::query_scalar::<_, String>(
                "SELECT id FROM qq_inbox WHERE app_id='fixture' ORDER BY seq DESC LIMIT 1",
            )
            .fetch_one(&app.store.pool)
            .await
            .unwrap(),
            "未完成的 QQ 消息",
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    app.store.fail_turn(&failed, "interrupted").await.unwrap();
    let BeginTurn::Started(web) = app
        .store
        .begin_turn(&api_session, &id(), "来自网页的消息")
        .await
        .unwrap()
    else {
        panic!()
    };
    app.store.complete_turn(&web, "这是网页回复").await.unwrap();
    let asset = id();
    sqlx::query("INSERT INTO assets(id,session_id,mime,data,width,height,kind,created_at) VALUES(?,?,'image/jpeg',X'FFD8FF',1,1,'upload',?)")
        .bind(&asset).bind(&old).bind(now()).execute(&app.store.pool).await.unwrap();
    sqlx::query("INSERT INTO message_assets(message_id,asset_id,position) VALUES(?,?,0)")
        .bind(&qq_user.id)
        .bind(&asset)
        .execute(&app.store.pool)
        .await
        .unwrap();
    let call_id = id();
    sqlx::query("INSERT INTO tool_calls(id,session_id,turn_id,call_id,name,action,arguments,status,result,created_at) VALUES(?,?,?,?,'browser','screenshot','{}','completed',?,?)")
        .bind(&call_id).bind(&old).bind(&qq_user.turn_id).bind(id()).bind(json!({"asset_id":asset}).to_string()).bind(now()).execute(&app.store.pool).await.unwrap();
    let history = call(&router, "GET", "/v1/console/messages", json!({}))
        .await
        .1;
    let messages = history["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 5);
    assert_eq!(messages[0]["channel"], "qq");
    assert_eq!(messages[2]["channel"], "qq");
    assert_eq!(messages[2]["status"], "failed");
    assert_eq!(messages[3]["channel"], "api");
    assert_eq!(history["assets"][0]["id"], asset);
    assert_eq!(history["tool_calls"][0]["id"], call_id);
    assert_eq!(app.store.history(&api_session, 100).await.unwrap().len(), 4);
    assert_eq!(app.store.history(&old, 100).await.unwrap().len(), 4);
    drop(router);
    app.store.pool.close().await;
    drop(app);
    let app = App::new(config, rx).await.unwrap();
    let router = api::router(app);
    assert_eq!(
        call(&router, "GET", "/v1/console/messages", json!({}))
            .await
            .1,
        history
    );
    fake.abort();
}

#[tokio::test]
async fn console_transcript_is_bounded_and_includes_in_progress_turns() {
    let temp = tempfile::TempDir::new().unwrap();
    let (app, _stop, fake) = app(&temp).await;
    let router = api::router(app.clone());
    let session = app.store.create_session().await.unwrap();
    for i in 0..102 {
        let BeginTurn::Started(user) = app
            .store
            .begin_turn(&session, &id(), &format!("消息 {i}"))
            .await
            .unwrap()
        else {
            panic!()
        };
        app.store.complete_turn(&user, "回复").await.unwrap();
    }
    let history = call(&router, "GET", "/v1/console/messages", json!({}))
        .await
        .1;
    assert_eq!(history["messages"].as_array().unwrap().len(), 200);
    assert_eq!(history["messages"][0]["content"], "消息 2");
    let BeginTurn::Started(user) = app
        .store
        .begin_turn(&session, &id(), "正在处理的消息")
        .await
        .unwrap()
    else {
        panic!()
    };
    let history = call(&router, "GET", "/v1/console/messages", json!({}))
        .await
        .1;
    let last = history["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(last["id"], user.id);
    assert_eq!(last["status"], "generating");
    fake.abort();
}
