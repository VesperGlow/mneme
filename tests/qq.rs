mod common;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::Path,
    http::{HeaderMap, Request, StatusCode},
    routing::post,
};
use mneme::{App, api, config::Config, domain::now, memory, qq};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex, watch};
use tower::ServiceExt;

const APP_ID: &str = "12345678";
const SECRET: &str = "qq-fixture-secret-only";
const TOKEN: &str = "qq-test-console-token-01234567890123456789";

#[tokio::test]
async fn qq_forgetting_previews_before_confirmation_and_replays_without_regeneration() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, upstream) = common::fake_services().await;
    let (platform, qq_base, server) = platform().await;
    let (stop, rx) = watch::channel(false);
    let app = App::new(config(&temp, &base, &qq_base), rx).await.unwrap();
    let mneme::console_store::WriteResult::Saved(saved) = app
        .store
        .manual_memory(
            None,
            &mneme::console_store::MemoryWrite {
                content: "用户住在成都。".into(),
                kind: "fact".into(),
                importance: 0.8,
                confidence: 1.0,
                entities: vec![],
                valid_until: None,
                expected_revision: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    let callbacks = qq::router(app.clone());
    let worker = tokio::spawn(qq::worker(app.clone()));
    message(&callbacks, "forget-preview", "owner", "请忘记成都").await;
    wait_sent(&app, 1).await;
    assert!(
        app.store
            .memory(&saved.id)
            .await
            .unwrap()
            .unwrap()
            .deleted_at
            .is_none()
    );
    let content = platform.sent.lock().await[0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(content.contains("尚未删除"));
    let token = content
        .split("/确认忘记 ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap();
    let command = format!("/确认忘记 {token} 1");
    message(&callbacks, "forget-confirm", "owner", &command).await;
    wait_sent(&app, 2).await;
    let deleted = app.store.memory(&saved.id).await.unwrap().unwrap();
    assert!(deleted.deleted_at.is_some());
    message(&callbacks, "forget-confirm-replay", "owner", &command).await;
    wait_sent(&app, 3).await;
    assert_eq!(
        app.store.memory(&saved.id).await.unwrap().unwrap().revision,
        deleted.revision
    );
    let calls: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM model_requests WHERE purpose IN ('chat','memory')",
    )
    .fetch_one(&app.store.pool)
    .await
    .unwrap();
    assert_eq!(
        calls, 0,
        "control commands must not enter generated history or extraction"
    );
    assert!(app.store.console_history().await.unwrap().is_empty());
    stop.send(true).unwrap();
    worker.await.unwrap();
    upstream.abort();
    server.abort();
}

#[tokio::test]
async fn owner_stop_bypasses_the_delivery_queue_and_other_accounts_cannot_cancel() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, upstream) = common::fake_services().await;
    let (platform, qq_base, server) = platform().await;
    let started = Arc::new(tokio::sync::Notify::new());
    let signal = started.clone();
    let slow = Router::new().route(
        "/chat/completions",
        post(move || {
            let signal = signal.clone();
            async move {
                signal.notify_one();
                std::future::pending::<()>().await;
                Json(json!({}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = config(&temp, &base, &qq_base);
    cfg.llm.model = "fixture-model".into();
    cfg.llm.base_url = format!("http://{}", listener.local_addr().unwrap());
    let slow = tokio::spawn(async move { axum::serve(listener, slow).await.unwrap() });
    let (stop, rx) = watch::channel(false);
    let app = App::new(cfg, rx).await.unwrap();
    let callbacks = qq::router(app.clone());
    let worker = tokio::spawn(qq::worker(app.clone()));
    message(&callbacks, "blocked-turn", "owner", "先慢慢想").await;
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .unwrap();
    let turn = mneme::qq_store::delivery_id(APP_ID, "blocked-turn");
    message(&callbacks, "intruder-stop", "someone-else", "/停止").await;
    assert!(!app.store.turn_cancelled(&turn).await.unwrap());
    message(&callbacks, "owner-stop", "owner", "/停止").await;
    wait_sent(&app, 1).await;
    let sent = platform.sent.lock().await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["content"], "已停止当前回复，临时内容未保存。");
    drop(sent);
    let state: (String, String) =
        sqlx::query_as("SELECT status,last_error FROM qq_inbox WHERE id=?")
            .bind(&turn)
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
    assert_eq!(state, ("dead".into(), "cancelled".into()));
    let assistants: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM messages WHERE turn_id=? AND role='assistant'")
            .bind(&turn)
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
    assert_eq!(assistants, 0);
    // Platform redelivery cannot execute a second stop command.
    message(&callbacks, "owner-stop", "owner", "/停止").await;
    assert_eq!(
        app.store.qq_status(APP_ID).await.unwrap()["queue"]["sent"],
        1
    );
    stop.send(true).unwrap();
    worker.await.unwrap();
    for task in [slow, upstream, server] {
        task.abort();
    }
}

#[derive(Clone, Default)]
struct Platform {
    sent: Arc<Mutex<Vec<Value>>>,
    tokens: Arc<AtomicUsize>,
    fail: Arc<AtomicUsize>,
}
async fn platform() -> (Platform, String, tokio::task::JoinHandle<()>) {
    let state = Platform::default();
    let token_state = state.clone();
    let send_state = state.clone();
    let router = Router::new()
        .route(
            "/app/getAppAccessToken",
            post(move |Json(value): Json<Value>| {
                let state = token_state.clone();
                async move {
                    assert_eq!(value, json!({"appId":APP_ID,"clientSecret":SECRET}));
                    let n = state.tokens.fetch_add(1, Ordering::SeqCst) + 1;
                    Json(json!({"access_token":format!("fake-token-{n}"),"expires_in":"7200"}))
                }
            }),
        )
        .route(
            "/v2/users/{openid}/messages",
            post(
                move |Path(openid): Path<String>,
                      headers: HeaderMap,
                      Json(mut value): Json<Value>| {
                    let state = send_state.clone();
                    async move {
                        assert!(
                            headers["authorization"]
                                .to_str()
                                .unwrap()
                                .starts_with("QQBot fake-token-")
                        );
                        assert_eq!(headers["x-union-appid"], APP_ID);
                        let failure = state.fail.swap(0, Ordering::SeqCst);
                        if failure > 0 {
                            return (
                                StatusCode::from_u16(failure as u16).unwrap(),
                                Json(json!({"code":failure})),
                            );
                        }
                        value["recipient"] = json!(openid);
                        state.sent.lock().await.push(value);
                        (StatusCode::OK, Json(json!({"id":"accepted-reply"})))
                    }
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (state, base, task)
}
fn config(temp: &tempfile::TempDir, base: &str, qq_base: &str) -> Config {
    let mut c: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    c.memory.reconcile_updates = false; // This fixture does not implement semantic adjudication.
    c.memory.verify_recall = false;
    c.api_token = TOKEN.into();
    c.llm_key = "fixture".into();
    c.neo4j_password = "fixture-only".into();
    c.database = temp.path().join("db").to_string_lossy().into();
    c.llm.base_url = base.into();
    c.models.base_url = base.into();
    c.neo4j.base_url = base.into();
    c.qq.app_id = APP_ID.into();
    c.qq.app_secret = SECRET.into();
    c.qq.api_base = qq_base.into();
    c.qq.token_url = format!("{qq_base}/app/getAppAccessToken");
    c.memory.poll_seconds = 1;
    c.memory.extract_every_tokens = 1;
    c.validate().unwrap();
    c
}
async fn response(router: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = router.clone().oneshot(request).await.unwrap();
    let code = response.status();
    let bytes = to_bytes(response.into_body(), 1_048_576).await.unwrap();
    (code, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}
async fn admin(router: &Router, path: &str, method: &str) -> (StatusCode, Value) {
    response(
        router,
        Request::builder()
            .uri(path)
            .method(method)
            .header("authorization", format!("Bearer {TOKEN}"))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await
}
async fn hook(router: &Router, payload: Value) -> (StatusCode, Value) {
    let body = payload.to_string();
    let ts = now().to_string();
    response(
        router,
        Request::builder()
            .method("POST")
            .uri("/qqbot")
            .header("X-Bot-Appid", APP_ID)
            .header("X-Signature-Timestamp", &ts)
            .header(
                "X-Signature-Ed25519",
                qq::sign(SECRET, &ts, body.as_bytes()),
            )
            .body(Body::from(body))
            .unwrap(),
    )
    .await
}
async fn message(router: &Router, id: &str, sender: &str, text: &str) {
    let (status,ack)=hook(router,json!({"op":0,"t":"C2C_MESSAGE_CREATE","id":format!("event-{id}"),
        "d":{"id":id,"content":text,"author":{"user_openid":sender},"timestamp":chrono::Utc::now().to_rfc3339()}})).await;
    assert_eq!(status, StatusCode::OK, "{ack}");
    assert_eq!(ack, json!({"op":12,"d":0}));
}
async fn wait_sent(app: &App, count: i64) {
    for _ in 0..120 {
        if app.store.qq_status(APP_ID).await.unwrap()["queue"]["sent"] == count {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "delivery timeout: {}",
        app.store.qq_status(APP_ID).await.unwrap()
    );
}

#[test]
fn official_botgo_signature_vectors() {
    let body=br#"{"id":"ROBOT1.0_veoihSEXDc8Q.g-6eLpNIa11bH8MisOjn-m-LKxCPntMk6exUXgcWCGpVO7L2QKTNZzjZzFFDSbiOFcqAPWyVA!!","content":"\u54e6\u4e00\u4e0b","timestamp":"2024-10-15T16:33:15+08:00","author":{"id":"675860273","user_openid":"675860273"}}"#;
    let body = String::from_utf8(body.to_vec())
        .unwrap()
        .replace("\\u54e6\\u4e00\\u4e0b", "哦一下");
    assert!(qq::verify(
        "123456abcdef",
        "1728981195",
        body.as_bytes(),
        "e949b5b94ef4103df903fb031d1d16e358db3db83e79e117edd404c8508be3ce8a76d7bad1bed353194c126a1a5915b4ad8b5288c1191cc53a12acffccd82004"
    ));
    assert_eq!(
        qq::sign("DG5g3B4j9X2KOErG", "1725442341", b"Arq0D5A61EgUu4OxUvOp"),
        "87befc99c42c651b3aac0278e71ada338433ae26fcb24307bdc5ad38c1adc2d01bcfcadc0842edac85e85205028a1132afe09280305f13aa6909ffc2d652c706"
    );
    assert!(!qq::verify(SECRET, "1", b"{}", &"a".repeat(128)));
}

#[tokio::test]
async fn signed_challenge_auth_freshness_and_route_isolation() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, upstream) = common::fake_services().await;
    let (_, qq_base, server) = platform().await;
    let (_, rx) = watch::channel(false);
    let app = App::new(config(&temp, &base, &qq_base), rx).await.unwrap();
    assert_eq!(app.config.llm.model, "fixture-model");
    let callbacks = qq::router(app.clone());
    let console = api::router(app);
    let ts = now().to_string();
    let (status, value) = hook(
        &callbacks,
        json!({"op":13,"d":{"plain_token":"challenge-token","event_ts":ts}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(qq::verify(
        SECRET,
        &ts,
        b"challenge-token",
        value["signature"].as_str().unwrap()
    ));
    let body = json!({"op":0,"t":"IGNORED","d":{}}).to_string();
    for (timestamp, secret, appid) in [
        (now() - 301, SECRET, APP_ID),
        (now(), "wrong-secret", APP_ID),
        (now(), SECRET, "87654321"),
    ] {
        let ts = timestamp.to_string();
        let (status, _) = response(
            &callbacks,
            Request::builder()
                .method("POST")
                .uri("/qqbot")
                .header("X-Bot-Appid", appid)
                .header("X-Signature-Timestamp", &ts)
                .header(
                    "X-Signature-Ed25519",
                    qq::sign(secret, &ts, body.as_bytes()),
                )
                .body(Body::from(body.clone()))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    for path in ["/console/", "/v1/memories"] {
        assert_eq!(
            response(
                &callbacks,
                Request::builder().uri(path).body(Body::empty()).unwrap()
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        response(
            &console,
            Request::builder()
                .uri("/v1/qq/status")
                .body(Body::empty())
                .unwrap()
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    upstream.abort();
    server.abort();
}

#[tokio::test]
async fn private_reply_queue_uses_the_sixty_minute_c2c_window() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, upstream) = common::fake_services().await;
    let (_, qq_base, server) = platform().await;
    let (_, rx) = watch::channel(false);
    let app = App::new(config(&temp, &base, &qq_base), rx).await.unwrap();
    let callbacks = qq::router(app.clone());
    // A fresh, valid webhook may carry a delayed C2C event. Group rules do not apply.
    let sent = now() - 600;
    let stamp = chrono::DateTime::from_timestamp(sent, 0)
        .unwrap()
        .to_rfc3339();
    let (status,_)=hook(&callbacks,json!({"op":0,"t":"C2C_MESSAGE_CREATE","d":{"id":"long-browser-task","content":"浏览网页","author":{"user_openid":"owner"},"timestamp":stamp}})).await;
    assert_eq!(status, StatusCode::OK);
    let expires: i64 =
        sqlx::query_scalar("SELECT expires_at FROM qq_inbox WHERE message_id='long-browser-task'")
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
    assert_eq!(expires, sent + 3590);
    assert!(expires > now() + 1800);
    upstream.abort();
    server.abort();
}

#[tokio::test]
async fn runtime_qq_check_uses_real_client_without_binding_ports_or_opening_the_database() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, upstream) = common::fake_services().await;
    let (platform, qq_base, server) = platform().await;
    let cfg = config(&temp, &base, &qq_base);
    let (_, rx) = watch::channel(false);
    let app = App::new(cfg, rx).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config_path = temp.path().join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            "{}\n[qq]\napi_base=\"{qq_base}\"\ntoken_url=\"{qq_base}/app/getAppAccessToken\"\n",
            include_str!("../config/default.toml")
        ),
    )
    .unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_mneme"))
            .arg("--check-qq")
            .current_dir(temp.path())
            .kill_on_drop(true)
            .env("MNEME_CONFIG", config_path)
            .env("MNEME_DATABASE", &app.config.database)
            .env("MNEME_LISTEN", listener.local_addr().unwrap().to_string())
            .env("MNEME_API_TOKEN", TOKEN)
            .env("MNEME_LLM_API_KEY", "fixture")
            .env("MNEME_NEO4J_PASSWORD", "fixture-password-only")
            .env("MNEME_QQ_APP_ID", APP_ID)
            .env("MNEME_QQ_APP_SECRET", SECRET)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let result: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    assert_eq!(
        result,
        json!({"qq_token":true,"transport":"default","failure":null})
    );
    assert!(!stdout.contains(SECRET));
    assert!(!stdout.contains("fake-token"));
    assert_eq!(platform.tokens.load(Ordering::SeqCst), 1);
    assert_eq!(app.store.qq_status(APP_ID).await.unwrap()["bound"], false);
    upstream.abort();
    server.abort();
}

#[tokio::test]
async fn first_private_message_dedup_restart_cross_session_memory_and_token_refresh() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, upstream) = common::fake_services().await;
    let (platform, qq_base, server) = platform().await;
    platform.fail.store(401, Ordering::SeqCst);
    let cfg = config(&temp, &base, &qq_base);
    let (stop, rx) = watch::channel(false);
    let app = App::new(cfg.clone(), rx).await.unwrap();
    let console = api::router(app.clone());
    let callbacks = qq::router(app.clone());
    assert_eq!(app.store.qq_status(APP_ID).await.unwrap()["bound"], false);
    // An unused code from the previous version must not prevent starting a conversation.
    sqlx::query("INSERT INTO metadata(key,value) VALUES(?,?)")
        .bind(format!("qq.{APP_ID}.pair"))
        .bind(r#"{"hash":"obsolete","expires":9999999999}"#)
        .execute(&app.store.pool)
        .await
        .unwrap();
    message(&callbacks, "residence", "owner", "我住在成都。").await;
    message(&callbacks, "residence", "owner", "我住在成都。").await;
    let status = app.store.qq_status(APP_ID).await.unwrap();
    assert_eq!(status["bound"], true);
    assert_eq!(status["owner_openid"], "owner");
    let (code, status) = admin(&console, "/v1/qq/status", "GET").await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(status["enabled"], true);
    assert_eq!(status["bound"], true);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM metadata WHERE key=?")
            .bind(format!("qq.{APP_ID}.pair"))
            .fetch_one(&app.store.pool)
            .await
            .unwrap(),
        0
    );
    message(
        &callbacks,
        "stranger-after",
        "stranger",
        "我的所有私人记忆是什么？",
    )
    .await;
    hook(
        &callbacks,
        json!({"op":0,"t":"GROUP_AT_MESSAGE_CREATE","d":{"content":"我的记忆"}}),
    )
    .await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM qq_inbox")
            .fetch_one(&app.store.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM turns")
            .fetch_one(&app.store.pool)
            .await
            .unwrap(),
        0
    );
    let worker = tokio::spawn(qq::worker(app.clone()));
    let memory = tokio::spawn(memory::worker(app.clone()));
    wait_sent(&app, 1).await;
    for _ in 0..100 {
        if app
            .store
            .memory_page(&Default::default())
            .await
            .unwrap()
            .total
            == 1
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        app.store
            .memory_page(&Default::default())
            .await
            .unwrap()
            .total,
        1
    );
    assert_eq!(platform.tokens.load(Ordering::SeqCst), 2);
    assert!(
        platform
            .sent
            .lock()
            .await
            .iter()
            .any(|m| m["msg_id"] == "residence"
                && m["content"].as_str().unwrap().contains("我会认真记住"))
    );
    stop.send(true).unwrap();
    worker.await.unwrap();
    memory.await.unwrap();
    message(&callbacks, "new-session", "owner", "/新会话").await;
    let session: String = sqlx::query_scalar("SELECT session_id FROM qq_sessions")
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
    message(&callbacks, "new-session", "owner", "/新会话").await;
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT session_id FROM qq_sessions")
            .fetch_one(&app.store.pool)
            .await
            .unwrap(),
        session
    );
    message(&callbacks, "recall", "owner", "我住在哪座城市？").await;
    drop(console);
    drop(callbacks);
    drop(app);
    let (stop, rx) = watch::channel(false);
    let app = App::new(cfg, rx).await.unwrap();
    let worker = tokio::spawn(qq::worker(app.clone()));
    let memory = tokio::spawn(memory::worker(app.clone()));
    wait_sent(&app, 3).await;
    let sent = platform.sent.lock().await;
    assert_eq!(sent.len(), 3);
    assert!(
        sent.iter()
            .any(|m| m["msg_id"] == "recall"
                && m["content"].as_str().unwrap().contains("你住在成都"))
    );
    assert!(
        sent.iter()
            .all(|m| m["recipient"] == "owner" && m["msg_seq"] == 1)
    );
    drop(sent);
    assert_eq!(app.store.history(&session, 100).await.unwrap().len(), 4);
    stop.send(true).unwrap();
    worker.await.unwrap();
    memory.await.unwrap();
    upstream.abort();
    server.abort();
}

#[tokio::test]
async fn first_private_message_is_atomic_under_failure_and_concurrent_senders() {
    use mneme::qq_store::{Acceptance, Incoming};
    let temp = tempfile::TempDir::new().unwrap();
    let (base, upstream) = common::fake_services().await;
    let (_, rx) = watch::channel(false);
    let app = App::new(config(&temp, &base, &base), rx).await.unwrap();
    let incoming = |message_id: &str, sender: &str, text: &str| Incoming {
        message_id: message_id.into(),
        sender: sender.into(),
        content: text.into(),
        expires_at: now() + 290,
    };
    sqlx::query("CREATE TRIGGER qq_test_reject BEFORE INSERT ON qq_inbox BEGIN SELECT RAISE(ABORT, 'fixture inbox failure'); END")
        .execute(&app.store.pool).await.unwrap();
    assert!(
        app.store
            .qq_accept(APP_ID, incoming("failed", "failed-owner", "你好"))
            .await
            .is_err()
    );
    assert_eq!(app.store.qq_status(APP_ID).await.unwrap()["bound"], false);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sessions")
            .fetch_one(&app.store.pool)
            .await
            .unwrap(),
        0
    );
    sqlx::query("DROP TRIGGER qq_test_reject")
        .execute(&app.store.pool)
        .await
        .unwrap();

    let (first, second) = tokio::join!(
        app.store
            .qq_accept(APP_ID, incoming("first-a", "owner-a", "你好 A")),
        app.store
            .qq_accept(APP_ID, incoming("first-b", "owner-b", "你好 B")),
    );
    let (first, second) = (first.unwrap(), second.unwrap());
    assert!(matches!(
        (&first, &second),
        (Acceptance::Queued, Acceptance::OtherAccount)
            | (Acceptance::OtherAccount, Acceptance::Queued)
    ));
    let status = app.store.qq_status(APP_ID).await.unwrap();
    assert_eq!(status["queue"]["pending"], 1);
    let (message_id, owner) = if first == Acceptance::Queued {
        ("first-a", "owner-a")
    } else {
        ("first-b", "owner-b")
    };
    assert_eq!(status["owner_openid"], owner);
    assert_eq!(
        app.store
            .qq_accept(APP_ID, incoming(message_id, owner, "重复事件"))
            .await
            .unwrap(),
        Acceptance::Duplicate
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM qq_sessions")
            .fetch_one(&app.store.pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        app.store
            .qq_accept(
                APP_ID,
                incoming("old-command", owner, "/绑定 obsolete-fixture-code")
            )
            .await
            .unwrap(),
        Acceptance::Queued
    );
    let (content, reply): (String, String) =
        sqlx::query_as("SELECT content,reply FROM qq_inbox WHERE message_id='old-command'")
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
    assert!(content.is_empty());
    assert!(reply.contains("无需绑定"));
    upstream.abort();
}

#[tokio::test]
async fn persisted_reply_retries_without_regeneration_and_expired_queue_is_dead() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, upstream) = common::fake_services().await;
    let (platform, qq_base, server) = platform().await;
    let cfg = config(&temp, &base, &qq_base);
    let (stop, rx) = watch::channel(false);
    let app = App::new(cfg.clone(), rx).await.unwrap();
    sqlx::query("INSERT INTO metadata(key,value) VALUES(?,?)")
        .bind(format!("qq.{APP_ID}.owner"))
        .bind("owner")
        .execute(&app.store.pool)
        .await
        .unwrap();
    let callbacks = qq::router(app.clone());
    message(&callbacks, "retry", "owner", "你好").await;
    platform.fail.store(500, Ordering::SeqCst);
    let worker = tokio::spawn(qq::worker(app.clone()));
    for _ in 0..100 {
        let ready: bool = sqlx::query_scalar(
            "SELECT status='pending' AND reply IS NOT NULL AND attempts=1 FROM qq_inbox",
        )
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
        if ready {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let reply: String = sqlx::query_scalar("SELECT reply FROM qq_inbox")
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
    stop.send(true).unwrap();
    worker.await.unwrap();
    drop(callbacks);
    drop(app);
    let (stop, rx) = watch::channel(false);
    let app = App::new(cfg, rx).await.unwrap();
    let callbacks = qq::router(app.clone());
    sqlx::query("UPDATE qq_inbox SET available_at=0")
        .execute(&app.store.pool)
        .await
        .unwrap();
    let worker = tokio::spawn(qq::worker(app.clone()));
    wait_sent(&app, 1).await;
    assert_eq!(platform.sent.lock().await[0]["content"], reply);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM turns")
            .fetch_one(&app.store.pool)
            .await
            .unwrap(),
        1
    );
    message(&callbacks, "expired", "owner", "这条不应该生成").await;
    sqlx::query("UPDATE qq_inbox SET expires_at=0 WHERE message_id='expired'")
        .execute(&app.store.pool)
        .await
        .unwrap();
    for _ in 0..100 {
        if app.store.qq_status(APP_ID).await.unwrap()["queue"]["dead"] == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        app.store.qq_status(APP_ID).await.unwrap()["queue"]["dead"],
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM turns")
            .fetch_one(&app.store.pool)
            .await
            .unwrap(),
        1
    );
    stop.send(true).unwrap();
    worker.await.unwrap();
    upstream.abort();
    server.abort();
}

#[tokio::test]
async fn queued_qq_input_cannot_resurrect_a_console_deleted_memory() {
    use mneme::console_store::{MemoryWrite, WriteResult};
    let temp = tempfile::TempDir::new().unwrap();
    let (base, upstream) = common::fake_services().await;
    let (_, qq_base, server) = platform().await;
    let (stop, rx) = watch::channel(false);
    let app = App::new(config(&temp, &base, &qq_base), rx).await.unwrap();
    sqlx::query("INSERT INTO metadata(key,value) VALUES(?,?)")
        .bind(format!("qq.{APP_ID}.owner"))
        .bind("owner")
        .execute(&app.store.pool)
        .await
        .unwrap();
    let callbacks = qq::router(app.clone());
    message(&callbacks, "old-queued-fact", "owner", "我住在成都。").await;
    let WriteResult::Saved(memory) = app
        .store
        .manual_memory(
            None,
            &MemoryWrite {
                content: "用户住在成都。".into(),
                kind: "fact".into(),
                importance: 0.8,
                confidence: 1.0,
                entities: vec!["成都".into()],
                valid_until: None,
                expected_revision: None,
            },
        )
        .await
        .unwrap()
    else {
        panic!()
    };
    app.store
        .delete_memory(&memory.id, Some(memory.revision))
        .await
        .unwrap();
    let worker = tokio::spawn(qq::worker(app.clone()));
    let memory_worker = tokio::spawn(memory::worker(app.clone()));
    wait_sent(&app, 1).await;
    for _ in 0..100 {
        if app.store.job_stats().await.unwrap() == json!({"pending":0,"running":0,"dead":0}) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        app.store.job_stats().await.unwrap(),
        json!({"pending":0,"running":0,"dead":0})
    );
    assert_eq!(
        app.store
            .memory_page(&Default::default())
            .await
            .unwrap()
            .total,
        0
    );
    message(&callbacks, "new-explicit-fact", "owner", "我住在成都。").await;
    wait_sent(&app, 2).await;
    for _ in 0..100 {
        if app
            .store
            .memory_page(&Default::default())
            .await
            .unwrap()
            .total
            == 1
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        app.store
            .memory_page(&Default::default())
            .await
            .unwrap()
            .total,
        1
    );
    stop.send(true).unwrap();
    worker.await.unwrap();
    memory_worker.await.unwrap();
    upstream.abort();
    server.abort();
}

#[tokio::test]
async fn automatic_model_selection_filters_and_persists_across_provider_outage() {
    use axum::routing::get;
    let temp = tempfile::TempDir::new().unwrap();
    let (base, upstream) = common::fake_services().await;
    let (_, qq_base, qq_server) = platform().await;
    let failed = Arc::new(AtomicUsize::new(0));
    let calls = Arc::new(AtomicUsize::new(0));
    let fail = failed.clone();
    let requests = calls.clone();
    let router=Router::new().route("/models",get(move ||{
        let fail=fail.clone();let requests=requests.clone();async move {
            requests.fetch_add(1,Ordering::SeqCst);
            if fail.load(Ordering::SeqCst)>0 {return (StatusCode::NOT_FOUND,Json(json!({"error":"provider directory unavailable"})));}
            (StatusCode::OK,Json(json!({"data":[{"id":"embedding-model"},{"id":"reranker"},{"id":"chat-a"},{"id":"chat-b"}]})))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = config(&temp, &base, &qq_base);
    cfg.llm.base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (_, rx) = watch::channel(false);
    let app = App::new(cfg.clone(), rx).await.unwrap();
    assert_eq!(app.config.llm.model, "chat-a");
    drop(app);
    failed.store(1, Ordering::SeqCst);
    let (_, rx) = watch::channel(false);
    let app = App::new(cfg.clone(), rx).await.unwrap();
    assert_eq!(app.config.llm.model, "chat-a");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(app);
    let fresh = tempfile::TempDir::new().unwrap();
    cfg.database = fresh.path().join("db").to_string_lossy().into();
    let (_, rx) = watch::channel(false);
    let error = App::new(cfg.clone(), rx).await.err().unwrap().to_string();
    assert!(error.contains("MNEME_LLM_MODEL"));
    cfg.llm.model = "explicit-model".into();
    let (_, rx) = watch::channel(false);
    let app = App::new(cfg, rx).await.unwrap();
    assert_eq!(app.config.llm.model, "explicit-model");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    upstream.abort();
    qq_server.abort();
    server.abort();
}
