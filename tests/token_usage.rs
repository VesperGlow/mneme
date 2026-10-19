mod common;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::post,
};
use mneme::{App, api, config::Config, domain::id};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const TOKEN: &str = "usage-fixture-token-01234567890123456789";

async fn provider(legacy: bool) -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let requests = seen.clone();
    let router = Router::new().route("/chat/completions", post(move |Json(body): Json<Value>| {
        let seen = requests.clone();
        async move {
            seen.lock().unwrap().push(body.clone());
            if body["stream"] != true {
                return Json(json!({"choices":[{"message":{"content":"{\"memories\":[]}"},"finish_reason":"stop"}],
                    "usage":{"input_tokens":50,"output_tokens":10}})).into_response();
            }
            if legacy && body.get("stream_options").is_some() {
                return (StatusCode::BAD_REQUEST, Json(json!({"error":{"message":"unsupported stream_options.include_usage"}}))).into_response();
            }
            let user = body["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap();
            if user == "bad400" {
                return (StatusCode::BAD_REQUEST, Json(json!({"error":{"message":"invalid model"}}))).into_response();
            }
            let reasoning = if user == "reasoning" { "先核对信息。<img src=x onerror=\"window.__reasoning_xss=1\">" } else { "" };
            let mut wire = String::new();
            for text in [reasoning, if reasoning.is_empty() {""} else {"再组织回答。"}] {
                if !text.is_empty() { wire.push_str(&format!("data: {}\n\n", json!({"choices":[{"delta":{"reasoning":text}}]}))); }
            }
            wire.push_str(&format!("data: {}\n\n",json!({"choices":[{"delta":{"content":"答复"},"finish_reason":"stop"}]})));
            if !legacy && user != "missing" {
                let usage = json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":90010,"completion_tokens_details":{"reasoning_tokens":90000},"total_tokens":90110}});
                wire.push_str(&format!("data: {usage}\n\ndata: {usage}\n\n"));
            }
            if user != "cut" { wire.push_str("data: [DONE]\n\n"); }
            let chunks: Vec<_> = wire.into_bytes().chunks(3).map(|b|Ok::<_,std::convert::Infallible>(b.to_vec())).collect();
            ([("content-type","text/event-stream")],Body::from_stream(futures_util::stream::iter(chunks))).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (base, seen, server)
}
fn config(temp: &tempfile::TempDir, base: &str, models: &str) -> Config {
    let mut c: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    c.database = temp.path().join("db").to_string_lossy().into();
    c.api_token = TOKEN.into();
    c.llm_key = "fixture".into();
    c.neo4j_password = "fixture".into();
    c.llm.model = "usage-fixture".into();
    c.llm.base_url = base.into();
    c.models.base_url = models.into();
    c.neo4j.base_url = models.into();
    c.memory.extract_every_chars = 100_000;
    c
}
async fn request(router: &Router, method: &str, path: &str, body: Value) -> String {
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
    assert_eq!(response.status(), StatusCode::OK);
    String::from_utf8(
        to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}
async fn data(router: &Router, path: &str) -> Value {
    serde_json::from_str(&request(router, "GET", path, json!({})).await).unwrap()
}
async fn chat(router: &Router, turn: &str, message: &str) -> String {
    request(
        router,
        "POST",
        "/v1/chat",
        json!({"request_id":turn,"message":message}),
    )
    .await
}

#[tokio::test]
async fn actual_usage_reasoning_replay_restart_and_context_are_separate() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, seen, server) = provider(false).await;
    let (models, fake) = common::fake_services().await;
    let c = config(&temp, &base, &models);
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c.clone(), rx.clone()).await.unwrap();
    let router = api::router(app.clone());
    let turn = id();
    let wire = chat(&router, &turn, "reasoning").await;
    assert!(
        wire.contains("event: done") && wire.contains("event: reasoning"),
        "{wire}"
    );
    assert!(wire.find("event: reasoning").unwrap() < wire.find("event: delta").unwrap());
    assert!(wire.contains("先核对信息") && wire.contains("再组织回答"));
    assert_eq!(
        seen.lock().unwrap()[0]["stream_options"]["include_usage"],
        true
    );
    let before = data(&router, "/v1/conversation").await;
    let context = &before["context"];
    assert_eq!(context["session_usage"]["requests"], 1);
    assert_eq!(context["session_usage"]["input_tokens"], 100);
    assert_eq!(context["session_usage"]["output_tokens"], 90010);
    assert_eq!(context["session_usage"]["reasoning_tokens"], 90000);
    assert_eq!(context["session_usage"]["total_tokens"], 90110);
    assert_eq!(context["last_request"]["usage"]["input_tokens"], 100);
    assert!(
        context["context_used_tokens"].as_u64().unwrap() < 5000,
        "reasoning spend must not enter next context"
    );
    assert!(context["distribution"]["system"].as_u64().unwrap() > 0);
    let history = data(&router, "/v1/console/messages").await;
    assert_eq!(history["messages"][1]["content"], "答复");
    assert!(
        history["model_requests"][0]["reasoning"]
            .as_str()
            .unwrap()
            .contains("再组织回答")
    );
    let replay = chat(&router, &turn, "reasoning").await;
    assert!(replay.contains("replayed") && replay.contains("event: reasoning"));
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(
        data(&router, "/v1/conversation").await["context"]["session_usage"],
        context["session_usage"]
    );
    app.store.pool.close().await;
    drop(router);
    drop(app);
    let app = App::new(c, rx).await.unwrap();
    let router = api::router(app.clone());
    assert_eq!(
        data(&router, "/v1/conversation").await["context"]["session_usage"],
        context["session_usage"]
    );
    assert_eq!(
        data(&router, "/v1/console/messages").await["model_requests"][0]["reasoning"],
        history["model_requests"][0]["reasoning"]
    );
    let counts = chat(&router, &id(), "counts-only").await;
    assert!(counts.contains("event: usage") && !counts.contains("event: reasoning"));
    let missing = chat(&router, &id(), "missing").await;
    assert!(missing.contains("event: done") && !missing.contains("event: reasoning"));
    let context = data(&router, "/v1/conversation").await;
    assert_eq!(context["context"]["session_usage"]["requests"], 3);
    assert_eq!(
        context["context"]["session_usage"]["reported"]["total_tokens"],
        2
    );
    assert_eq!(context["context"]["session_usage"]["total_tokens"], 180220);
    assert!(context["context"]["last_request"]["usage"]["input_tokens"].is_null());
    app.llm
        .complete_json("extract", json!({"target":"fixture"}))
        .await
        .unwrap();
    let context = data(&router, "/v1/conversation").await;
    assert_eq!(
        context["context"]["session_usage"]["by_purpose"]["memory"]["total_tokens"],
        60
    );
    assert_eq!(context["context"]["session_usage"]["total_tokens"], 180280);
    assert_eq!(
        context["context"]["session_usage"]["reported"]["reasoning_tokens"],
        2
    );
    server.abort();
    fake.abort();
}

#[tokio::test]
async fn interrupted_stream_keeps_reported_usage_without_committing_partial_reply() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, seen, server) = provider(false).await;
    let (models, fake) = common::fake_services().await;
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config(&temp, &base, &models), rx).await.unwrap();
    let router = api::router(app.clone());
    let turn = id();
    let wire = chat(&router, &turn, "cut").await;
    assert!(wire.contains("event: error") && !wire.contains("event: done"));
    let detail = data(&router, &format!("/v1/turns/{turn}")).await;
    assert_eq!(detail["status"], "failed");
    assert_eq!(detail["messages"].as_array().unwrap().len(), 1);
    assert_eq!(detail["model_requests"][0]["usage"]["total_tokens"], 90110);
    assert_eq!(detail["model_requests"][0]["status"], "failed");
    assert_eq!(
        data(&router, "/v1/conversation").await["context"]["session_usage"]["requests"],
        1
    );
    let wire = chat(&router, &id(), "bad400").await;
    assert!(wire.contains("event: error"));
    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "unrelated validation errors must not retry"
    );
    server.abort();
    fake.abort();
}

#[tokio::test]
async fn unsupported_stream_usage_falls_back_once_and_is_not_double_counted() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, seen, server) = provider(true).await;
    let (models, fake) = common::fake_services().await;
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config(&temp, &base, &models), rx).await.unwrap();
    let router = api::router(app);
    for _ in 0..2 {
        assert!(chat(&router, &id(), "legacy").await.contains("event: done"));
    }
    assert_eq!(seen.lock().unwrap().len(), 3);
    assert!(seen.lock().unwrap()[2].get("stream_options").is_none());
    let context = data(&router, "/v1/conversation").await;
    assert_eq!(context["context"]["session_usage"]["requests"], 2);
    assert!(context["context"]["session_usage"]["total_tokens"].is_null());
    assert!(context["context"]["tokens_estimated"].as_bool().unwrap());
    server.abort();
    fake.abort();
}
