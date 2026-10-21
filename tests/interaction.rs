mod common;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::post,
};
use mneme::{
    App, api,
    config::Config,
    domain::{id, now},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
const TOKEN: &str = "source-context-token-01234567890123456789";
async fn setup() -> (
    tempfile::TempDir,
    App,
    Arc<Mutex<Vec<Value>>>,
    Vec<tokio::task::JoinHandle<()>>,
    tokio::sync::watch::Sender<bool>,
) {
    let temp = tempfile::tempdir().unwrap();
    let (base, fake) = common::fake_services().await;
    let seen = Arc::new(Mutex::new(vec![]));
    let capture = seen.clone();
    let provider=Router::new().route("/chat/completions",post(move|Json(body):Json<Value>|{let seen=capture.clone();async move {
  seen.lock().unwrap().push(body.clone());
  if body["stream"]==true {
   let current=body["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap();
   let content=if current=="不用回复" {""}else{"自然回复。"};
   return ([("content-type","text/event-stream")],format!("data: {}\n\ndata: [DONE]\n\n",json!({"choices":[{"delta":{"content":content},"finish_reason":"stop"}]}))).into_response();
  }
  Json(json!({"choices":[{"message":{"content":"{\"memories\":[]}"},"finish_reason":"stop"}]})).into_response()
 }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let llm = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    let mut c: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    c.database = temp.path().join("db").to_string_lossy().into();
    c.api_token = TOKEN.into();
    c.llm_key = "fixture".into();
    c.neo4j_password = "fixture".into();
    c.models.base_url = base.clone();
    c.neo4j.base_url = base;
    c.llm.base_url = llm;
    c.llm.model = "fixture-model".into();
    c.memory.poll_seconds = 1;
    c.memory.extract_every_tokens = 100000;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    (temp, app, seen, vec![fake, task], stop)
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
    let bytes = to_bytes(response.into_body(), 2000000).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({"wire":String::from_utf8_lossy(&bytes)})),
    )
}
async fn chat(router: &Router, message: &str) -> (String, String) {
    let turn = id();
    let (_, data) = call(
        router,
        "POST",
        "/v1/chat",
        json!({"request_id":turn,"message":message}),
    )
    .await;
    (turn, data["wire"].as_str().unwrap().to_owned())
}
#[tokio::test]
async fn recalled_fact_keeps_its_qualifying_source_and_record_time_separate() {
    let (_temp, app, seen, tasks, _stop) = setup().await;
    let session = app.store.conversation().await.unwrap();
    let quote = "假如我明年住在成都，那只是设想";
    let mneme::store::BeginTurn::Started(source) =
        app.store.begin_turn(&session, &id(), quote).await.unwrap()
    else {
        panic!()
    };
    app.store
        .complete_turn(&source, "这还是一个设想。")
        .await
        .unwrap();
    // A deliberately bad candidate checks that its qualification is still
    // provided to generation. This does not pretend to test entailment.
    let candidate: mneme::domain::Candidate = serde_json::from_value(json!({
        "key":"user.residence","content":"用户住在成都。","kind":"fact","importance":0.9,"confidence":0.99,
        "source_quote":quote,"entities":["成都"],"valid_until":null,"sensitive":false,"equivalent_to":null
    })).unwrap();
    let mut vector = vec![0.0; 1024];
    vector[0] = 1.0;
    app.store
        .apply_candidates(&source, vec![(candidate, vector, None)], "none")
        .await
        .unwrap();
    let router = api::router(app.clone());
    let (_, wire) = chat(&router, "我住在哪里？").await;
    assert!(wire.contains("event: done"), "{wire}");
    let requests = seen.lock().unwrap();
    let memory = requests.last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["content"].as_str())
        .find(|s| s.starts_with("<untrusted_long_term_memory>"))
        .unwrap();
    assert!(
        memory.contains(quote)
            && memory.contains("record_updated_at")
            && memory.contains("sources")
    );
    assert!(
        !memory.contains("\"observed_at\""),
        "record update is not event time"
    );
    for task in tasks {
        task.abort();
    }
}

#[tokio::test]
async fn immediate_followup_retains_read_fact_sources_but_not_expired_records() {
    let (_temp, app, seen, tasks, _stop) = setup().await;
    let router = api::router(app.clone());
    let (status, _) = call(&router,"POST","/v1/memories",json!({"content":"用户的项目正式名称是纸桥。","kind":"fact","importance":0.9,"confidence":1.0,"entities":["纸桥"],"valid_until":null,"expected_revision":null})).await;
    assert_eq!(status, StatusCode::CREATED);
    let (_, wire) = chat(&router, "之前保存的我的项目叫什么？").await;
    assert!(wire.contains("event: done"));
    let memory = app.store.active_memories(1).await.unwrap().remove(0);
    assert!(memory.last_accessed_at.is_some());
    let (_, wire) = chat(
        &router,
        "不是改名，我今天只是想给它想个副标题，名字还沿用之前那个。",
    )
    .await;
    assert!(wire.contains("event: done"));
    let has_fact = |request: &Value| {
        request["messages"].as_array().unwrap().iter().any(|m| {
            m["content"].as_str().is_some_and(|s| {
                s.starts_with("<untrusted_long_term_memory>") && s.contains("纸桥")
            })
        })
    };
    assert!(
        has_fact(seen.lock().unwrap().last().unwrap()),
        "retrieval omitted a noun; retain its already-read source as context"
    );
    sqlx::query("UPDATE memories SET valid_until=? WHERE id=?")
        .bind(now() - 1)
        .bind(&memory.id)
        .execute(&app.store.pool)
        .await
        .unwrap();
    chat(&router, "那个名字先不变").await;
    assert!(
        !has_fact(seen.lock().unwrap().last().unwrap()),
        "expiration still wins over recent context"
    );
    for task in tasks {
        task.abort();
    }
}
