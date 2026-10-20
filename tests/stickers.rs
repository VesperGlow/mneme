mod common;
use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    routing::post,
};
use mneme::{
    App, api,
    config::Config,
    domain::{id, now},
    qq,
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tokio::sync::Mutex;
use tower::ServiceExt;
const TOKEN: &str = "sticker-test-token-01234567890123456789";
const TOOL_TOKEN: &str = "sticker-tools-token-01234567890123456789";
const GIF: &str = "R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";
#[derive(Clone, Default)]
struct Fixture {
    reply: Arc<Mutex<String>>,
    requests: Arc<Mutex<Vec<Value>>>,
    sent: Arc<Mutex<Vec<Value>>>,
    downloads: Arc<AtomicUsize>,
    fail_media: Arc<AtomicBool>,
    fail_vision: Arc<AtomicBool>,
    vision_calls: Arc<AtomicUsize>,
    vision_delay: Arc<AtomicUsize>,
    vision_descriptions: Arc<Mutex<std::collections::VecDeque<String>>>,
}
async fn fixture() -> (Fixture, String, tokio::task::JoinHandle<()>) {
    let fixture = Fixture::default();
    let routes = Router::new()
        .route(
            "/chat/completions",
            post(
                |State(s): State<Fixture>, Json(body): Json<Value>| async move {
                    if body["stream"] == false {
                        assert!(body["max_tokens"].as_u64().unwrap() >= 4096, "reasoning needs headroom before the JSON answer");
                        assert!(body.get("thinking").is_none(), "provider extensions must not leak to compatible endpoints");
                        assert!(body["messages"][0]["content"].as_str().unwrap().contains("名称和描述都用简体中文"));
                        assert!(body["messages"][1]["content"][0]["text"].as_str().unwrap().contains("名称和描述都用简体中文"));
                        s.vision_calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(std::time::Duration::from_millis(s.vision_delay.load(Ordering::SeqCst) as u64)).await;
                        if s.fail_vision.load(Ordering::SeqCst) { return ([("content-type", "text/event-stream")], Body::from("{}")); }
                        assert!(body["messages"][1]["content"].to_string().contains("data:image/jpeg;base64,"));
                        let description = s.vision_descriptions.lock().await.pop_front().unwrap_or_else(|| "小动物张开双臂求抱抱，适合安慰或表达亲近。".into());
                        return ([("content-type", "text/event-stream")], Body::from(json!({"choices":[{"message":{"content":json!({"name":"自动识别的抱抱","description":description}).to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":1000,"completion_tokens":40,"total_tokens":1040}}).to_string()));
                    }
                    s.requests.lock().await.push(body.clone());
                    let mut reply = s.reply.lock().await.clone();
                    if reply == "__echo_image__" {
                        let asset = body["messages"].as_array().unwrap().last().unwrap()["content"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter_map(|part| part["text"].as_str())
                            .find(|text| text.contains("标识 "))
                            .unwrap()
                            .split_whitespace()
                            .last()
                            .unwrap();
                        reply = format!("这是你发来的图片。[[image:{asset}]]");
                    }
                    let wire = format!(
                        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                        json!({"choices":[{"delta":{"content":reply},"finish_reason":null}]}),
                        json!({"choices":[{"delta":{},"finish_reason":"stop"}]})
                    );
                    let chunks: Vec<_> = wire
                        .into_bytes()
                        .chunks(5)
                        .map(|b| Ok::<_, std::convert::Infallible>(b.to_vec()))
                        .collect();
                    (
                        [("content-type", "text/event-stream")],
                        Body::from_stream(futures_util::stream::iter(chunks)),
                    )
                },
            ),
        )
        .route(
            "/v1/stickers/normalize",
            post(|headers: HeaderMap, bytes: Bytes| async move {
                assert_eq!(headers["authorization"], format!("Bearer {TOOL_TOKEN}"));
                if bytes == "bad" {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error":"invalid_image"})),
                    );
                }
                (
                    StatusCode::OK,
                    Json(json!({"mime":"image/gif","data":GIF,"width":1,"height":1})),
                )
            }),
        )
        .route("/v1/stickers/preview", post(|| async { Json(json!({"mime":"image/jpeg","data":"/9j/AA==","width":256,"height":256})) }))
        .route(
            "/v1/images/fetch",
            post(
                |State(s): State<Fixture>, Json(body): Json<Value>| async move {
                    assert_eq!(body["url"], "https://images.example/picture.png");
                    s.downloads.fetch_add(1, Ordering::SeqCst);
                    Json(json!({"mime":"image/jpeg","data":"/9j/AA==","width":1,"height":1}))
                },
            ),
        )
        .route(
            "/token",
            post(|| async { Json(json!({"access_token":"fixture-qq-token","expires_in":7200})) }),
        )
        .route(
            "/v2/users/owner/files",
            post(|headers: HeaderMap, Json(body): Json<Value>| async move {
                assert_eq!(headers["authorization"], "QQBot fixture-qq-token");
                assert_eq!(body["file_type"], 1);
                assert_eq!(body["srv_send_msg"], false);
                assert!(body["file_data"].as_str().unwrap().len() > 3);
                Json(json!({"file_info":"fixture-image-info"}))
            }),
        )
        .route(
            "/v2/users/owner/messages",
            post(
                |State(s): State<Fixture>, Json(body): Json<Value>| async move {
                    if body["msg_type"] == 7 && s.fail_media.swap(false, Ordering::SeqCst) {
                        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"code":500})));
                    }
                    s.sent.lock().await.push(body);
                    (StatusCode::OK, Json(json!({"id":"fixture-sent"})))
                },
            ),
        )
        .with_state(fixture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    (fixture, base, server)
}
fn config(temp: &tempfile::TempDir, base: &str, services: &str) -> Config {
    let mut c: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    c.memory.reconcile_updates = false; // This fixture does not implement semantic adjudication.
    c.memory.verify_recall = false;
    c.api_token = TOKEN.into();
    c.llm_key = "fixture".into();
    c.neo4j_password = "fixture".into();
    c.database = temp.path().join("db").to_string_lossy().into();
    c.llm.model = "fixture".into();
    c.llm.base_url = base.into();
    c.models.base_url = services.into();
    c.neo4j.base_url = services.into();
    c.tools.enabled = true;
    c.tools.base_url = base.into();
    c.tools.token = TOOL_TOKEN.into();
    c.qq.app_id = "12345678".into();
    c.qq.app_secret = "fixture-secret".into();
    c.qq.api_base = base.into();
    c.qq.token_url = format!("{base}/token");
    c
}
async fn request(router: &Router, method: &str, path: &str, body: String) -> (StatusCode, String) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {TOKEN}"))
                .header(
                    "content-type",
                    if path.starts_with("/v1/stickers?") {
                        "application/octet-stream"
                    } else {
                        "application/json"
                    },
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    (
        response.status(),
        String::from_utf8(
            to_bytes(response.into_body(), 4 * 1024 * 1024)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap(),
    )
}
async fn call(router: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let (status, wire) = request(router, method, path, body.to_string()).await;
    (status, serde_json::from_str(&wire).unwrap())
}
async fn upload(router: &Router) -> String {
    let (status, wire) = request(
        router,
        "POST",
        "/v1/stickers?name=hug&description=comfort",
        "gif".into(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{wire}");
    serde_json::from_str::<Value>(&wire).unwrap()["id"]
        .as_str()
        .unwrap()
        .into()
}
async fn chat(router: &Router, turn: &str) -> String {
    let (status, wire) = request(
        router,
        "POST",
        "/v1/chat",
        json!({"request_id":turn,"message":"你好"}).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(wire.contains("event: done"), "{wire}");
    wire
}
#[tokio::test]
async fn global_library_persistence_replay_disable_delete_and_reset() {
    let temp = tempfile::TempDir::new().unwrap();
    let (fake, base, server) = fixture().await;
    let (services, upstream) = common::fake_services().await;
    let config = config(&temp, &base, &services);
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config.clone(), rx.clone()).await.unwrap();
    let router = api::router(app.clone());
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/stickers")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        request(
            &router,
            "POST",
            "/v1/stickers?name=abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ",
            "gif".into()
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        request(&router, "POST", "/v1/stickers?name=bad", "bad".into())
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let sticker = upload(&router).await;
    let (_, catalog) = call(&router, "GET", "/v1/stickers", json!({})).await;
    let image = catalog["stickers"][0]["image_id"]
        .as_str()
        .unwrap()
        .to_owned();
    *fake.reply.lock().await = format!("抱抱你。[[sticker:{sticker}]]");
    let turn = id();
    let wire = chat(&router, &turn).await;
    assert!(!wire.contains("[[sticker:"));
    assert!(wire.contains("event: media"));
    assert!(wire.contains(&image));
    let (_, saved) = call(&router, "GET", &format!("/v1/turns/{turn}"), json!({})).await;
    assert_eq!(saved["messages"][1]["content"], "抱抱你。");
    assert_eq!(saved["media"]["stickers"][0]["image_id"], image);
    let calls = fake.requests.lock().await.len();
    assert!(chat(&router, &turn).await.contains(&image));
    assert_eq!(calls, fake.requests.lock().await.len());
    assert!(
        fake.requests.lock().await[0]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains(&sticker)
    );
    call(
        &router,
        "PUT",
        "/v1/stickers/settings",
        json!({"enabled":false}),
    )
    .await;
    let disabled = id();
    chat(&router, &disabled).await;
    let (_, saved) = call(&router, "GET", &format!("/v1/turns/{disabled}"), json!({})).await;
    assert_eq!(saved["media"]["stickers"], json!([]));
    call(
        &router,
        "PUT",
        "/v1/stickers/settings",
        json!({"enabled":true}),
    )
    .await;
    assert_eq!(
        call(
            &router,
            "PUT",
            &format!("/v1/stickers/{sticker}"),
            json!({"name":"拥抱","description":"安慰、鼓励","enabled":false})
        )
        .await
        .0,
        StatusCode::OK
    );
    chat(&router, &id()).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM message_stickers")
            .fetch_one(&app.store.pool)
            .await
            .unwrap(),
        1
    );
    drop(router);
    app.store.pool.close().await;
    drop(app);
    let app = App::new(config, rx).await.unwrap();
    assert_eq!(app.stickers.read().unwrap().stickers[0].name, "拥抱");
    assert!(!app.stickers.read().unwrap().stickers[0].enabled);
    let router = api::router(app.clone());
    assert_eq!(
        call(
            &router,
            "DELETE",
            &format!("/v1/stickers/{sticker}"),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/v1/sticker-images/{image}"))
                .header("authorization", format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "image/gif");
    let replacement = upload(&router).await;
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/admin/reset",
            json!({"confirmation":"RESET"})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(app.stickers.read().unwrap().stickers[0].id, replacement);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM message_stickers")
            .fetch_one(&app.store.pool)
            .await
            .unwrap(),
        0
    );
    server.abort();
    upstream.abort();
}
async fn wait_for(app: &App, condition: &str) {
    for _ in 0..150 {
        if sqlx::query_scalar::<_, bool>(condition)
            .fetch_one(&app.store.pool)
            .await
            .unwrap()
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
    panic!("QQ condition timed out: {condition}");
}
#[tokio::test]
async fn qq_image_only_message_and_media_retry_survive_restart() {
    let temp = tempfile::TempDir::new().unwrap();
    let (fake, base, server) = fixture().await;
    let (services, upstream) = common::fake_services().await;
    let config = config(&temp, &base, &services);
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config.clone(), rx).await.unwrap();
    let router = api::router(app.clone());
    let sticker = upload(&router).await;
    *fake.reply.lock().await = format!("我看到了你的图片。[[sticker:{sticker}]]");
    fake.fail_media.store(true, Ordering::SeqCst);
    let callbacks = qq::router(app.clone());
    let payload = json!({"op":0,"t":"C2C_MESSAGE_CREATE","d":{"id":"image-message","content":"","author":{"user_openid":"owner"},"timestamp":chrono::Utc::now().to_rfc3339(),"attachments":[{"content_type":"image/png","url":"https://images.example/picture.png"}]}}).to_string();
    let stamp = now().to_string();
    for _ in 0..2 {
        let response = callbacks
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/qqbot")
                    .header("X-Bot-Appid", "12345678")
                    .header("X-Signature-Timestamp", &stamp)
                    .header(
                        "X-Signature-Ed25519",
                        qq::sign("fixture-secret", &stamp, payload.as_bytes()),
                    )
                    .body(Body::from(payload.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    assert_eq!(
        fake.downloads.load(Ordering::SeqCst),
        0,
        "acknowledge before downloads"
    );
    let worker = tokio::spawn(qq::worker(app.clone()));
    wait_for(
        &app,
        "SELECT EXISTS(SELECT 1 FROM qq_inbox WHERE status='pending' AND next_chunk=1)",
    )
    .await;
    assert_eq!(fake.sent.lock().await.len(), 1);
    assert_eq!(fake.downloads.load(Ordering::SeqCst), 1);
    {
        let requests = fake.requests.lock().await;
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0]["messages"]
                .to_string()
                .contains("data:image/jpeg;base64,")
        );
    }
    stop.send(true).unwrap();
    worker.await.unwrap();
    drop(router);
    drop(callbacks);
    app.store.pool.close().await;
    drop(app);
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config, rx).await.unwrap();
    sqlx::query("UPDATE qq_inbox SET available_at=0")
        .execute(&app.store.pool)
        .await
        .unwrap();
    let worker = tokio::spawn(qq::worker(app.clone()));
    wait_for(
        &app,
        "SELECT EXISTS(SELECT 1 FROM qq_inbox WHERE status='sent')",
    )
    .await;
    let sent = fake.sent.lock().await;
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["msg_type"], 0);
    assert_eq!(sent[1]["msg_type"], 7);
    assert_eq!(sent[1]["msg_seq"], 2);
    assert_eq!(sent[1]["media"]["file_info"], "fixture-image-info");
    assert_eq!(fake.requests.lock().await.len(), 1);
    assert_eq!(fake.downloads.load(Ordering::SeqCst), 1);
    drop(sent);
    *fake.reply.lock().await = "__echo_image__".into();
    app.store
        .qq_accept_with_images(
            "12345678",
            mneme::qq_store::Incoming {
                message_id: "echo-image".into(),
                sender: "owner".into(),
                content: "把图片发回来".into(),
                expires_at: now() + 300,
            },
            &["https://images.example/picture.png".into()],
        )
        .await
        .unwrap();
    wait_for(&app, "SELECT COUNT(*)=2 FROM qq_inbox WHERE status='sent'").await;
    let sent = fake.sent.lock().await;
    assert_eq!(sent.len(), 4);
    assert_eq!(sent[3]["msg_type"], 7);
    assert_eq!(sent[3]["msg_seq"], 2);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM message_assets ma JOIN messages m ON m.id=ma.message_id WHERE m.role='assistant'").fetch_one(&app.store.pool).await.unwrap(), 1);
    assert_eq!(fake.requests.lock().await.len(), 2);
    stop.send(true).unwrap();
    worker.await.unwrap();
    server.abort();
    upstream.abort();
}

#[tokio::test]
async fn automatic_understanding_resumes_after_restart_and_can_retry_without_overwriting_edits() {
    let temp = tempfile::TempDir::new().unwrap();
    let (fake, base, server) = fixture().await;
    let (services, upstream) = common::fake_services().await;
    let config = config(&temp, &base, &services);
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config.clone(), rx).await.unwrap();
    let router = api::router(app.clone());
    let (status, wire) = request(&router, "POST", "/v1/stickers?enabled=true", "gif".into()).await;
    assert_eq!(status, StatusCode::CREATED);
    let sticker = serde_json::from_str::<Value>(&wire).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!app.stickers.read().unwrap().prompt().contains(&sticker));
    drop(router);
    app.store.pool.close().await;
    drop(app);
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config, rx).await.unwrap();
    let router = api::router(app.clone());
    let worker = tokio::spawn(mneme::stickers::worker(app.clone()));
    wait_for(
        &app,
        "SELECT EXISTS(SELECT 1 FROM stickers WHERE analysis_status='complete')",
    )
    .await;
    // SQLite commits before the worker refreshes its in-memory prompt catalog.
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if app.stickers.read().unwrap().stickers[0].name == "自动识别的抱抱" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        app.stickers
            .read()
            .unwrap()
            .prompt()
            .contains("安慰或表达亲近")
    );
    assert_eq!(fake.vision_calls.load(Ordering::SeqCst), 1);
    assert_eq!(sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM model_requests WHERE purpose='sticker' AND json_extract(usage,'$.total_tokens')=1040").fetch_one(&app.store.pool).await.unwrap(), 1);
    fake.fail_vision.store(true, Ordering::SeqCst);
    assert_eq!(
        call(
            &router,
            "POST",
            &format!("/v1/stickers/{sticker}/analyze"),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    wait_for(
        &app,
        "SELECT EXISTS(SELECT 1 FROM stickers WHERE analysis_status='failed')",
    )
    .await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sticker_images")
            .fetch_one(&app.store.pool)
            .await
            .unwrap(),
        1
    );
    fake.fail_vision.store(false, Ordering::SeqCst);
    call(
        &router,
        "POST",
        &format!("/v1/stickers/{sticker}/analyze"),
        json!({}),
    )
    .await;
    wait_for(
        &app,
        "SELECT EXISTS(SELECT 1 FROM stickers WHERE analysis_status='complete')",
    )
    .await;
    assert_eq!(fake.vision_calls.load(Ordering::SeqCst), 3);
    fake.vision_delay.store(600, Ordering::SeqCst);
    call(
        &router,
        "POST",
        &format!("/v1/stickers/{sticker}/analyze"),
        json!({}),
    )
    .await;
    for _ in 0..100 {
        if fake.vision_calls.load(Ordering::SeqCst) == 4 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    }
    assert_eq!(fake.vision_calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        call(
            &router,
            "PUT",
            &format!("/v1/stickers/{sticker}"),
            json!({"name":"我修正的名字","description":"我自己的理解","enabled":true})
        )
        .await
        .0,
        StatusCode::OK
    );
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    assert_eq!(
        app.stickers.read().unwrap().stickers[0].name,
        "我修正的名字"
    );
    assert_eq!(
        app.stickers.read().unwrap().stickers[0].analysis_status,
        "manual"
    );
    stop.send(true).unwrap();
    worker.await.unwrap();
    server.abort();
    upstream.abort();
}

#[tokio::test]
async fn oversized_descriptions_retry_once_without_truncation_and_count_each_physical_request() {
    let temp = tempfile::TempDir::new().unwrap();
    let (fake, base, server) = fixture().await;
    let (services, upstream) = common::fake_services().await;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config(&temp, &base, &services), rx).await.unwrap();
    let router = api::router(app.clone());
    let description = "A small animal waves hello; use for friendly greetings.";
    fake.vision_descriptions
        .lock()
        .await
        .extend(["Too long. ".repeat(30), description.into()]);
    let (status, wire) = request(&router, "POST", "/v1/stickers?enabled=true", "gif".into()).await;
    assert_eq!(status, StatusCode::CREATED);
    let sticker = serde_json::from_str::<Value>(&wire).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let worker = tokio::spawn(mneme::stickers::worker(app.clone()));
    wait_for(
        &app,
        "SELECT EXISTS(SELECT 1 FROM stickers WHERE analysis_status='complete')",
    )
    .await;
    assert_eq!(fake.vision_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT description FROM stickers")
            .fetch_one(&app.store.pool)
            .await
            .unwrap(),
        description
    );
    fake.vision_descriptions
        .lock()
        .await
        .extend(["Still too long. ".repeat(20), "Still too long. ".repeat(20)]);
    assert_eq!(
        call(
            &router,
            "POST",
            &format!("/v1/stickers/{sticker}/analyze"),
            json!({})
        )
        .await
        .0,
        StatusCode::OK
    );
    wait_for(
        &app,
        "SELECT EXISTS(SELECT 1 FROM stickers WHERE analysis_status='failed')",
    )
    .await;
    assert_eq!(
        fake.vision_calls.load(Ordering::SeqCst),
        4,
        "exactly one retry per analysis"
    );
    let (saved, error): (String, String) =
        sqlx::query_as("SELECT description,analysis_error FROM stickers")
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
    assert_eq!(
        saved, description,
        "failed reanalysis must preserve the old description"
    );
    assert_eq!(error, "invalid_image_description");
    let (requests, total): (i64, i64) = sqlx::query_as("SELECT COUNT(*),SUM(json_extract(usage,'$.total_tokens')) FROM model_requests WHERE purpose='sticker'").fetch_one(&app.store.pool).await.unwrap();
    assert_eq!((requests, total), (4, 4160));
    stop.send(true).unwrap();
    worker.await.unwrap();
    server.abort();
    upstream.abort();
}
