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
const TOKEN: &str = "persona-fixture-token-01234567890123456789";
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
#[tokio::test]
async fn saved_background_is_stable_preview_is_read_only_and_stale_preview_rejects() {
    let (_temp, app, seen, tasks, _stop) = setup().await;
    let router = api::router(app.clone());
    let mut persona = app.persona.read().unwrap().clone();
    persona.description = "沉静、独立，珍视坦诚；与用户是朋友。".into();
    persona.style = "".into();
    assert_eq!(
        call(&router, "PUT", "/v1/console/persona", json!(persona))
            .await
            .0,
        StatusCode::OK
    );
    let (_, before) = call(&router, "GET", "/v1/console/persona/context", json!({})).await;
    assert!(
        before["system_prompt"]
            .as_str()
            .unwrap()
            .contains(&persona.description)
    );
    assert!(
        !before["system_prompt"]
            .as_str()
            .unwrap()
            .contains("人格决定")
    );
    assert!(seen.lock().unwrap().is_empty());
    for _ in 0..2 {
        let (status, preview) = call(
            &router,
            "POST",
            "/v1/console/persona/preview",
            json!({"message":"今天看到一只猫。","source_hash":before["source_hash"]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(preview["reply"], "自然回复。");
    }
    for table in ["messages", "memories", "interaction_state"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
    for message in ["今天有点累。", "以后不要总给建议。", "不用回复"] {
        let (_, wire) = call(
            &router,
            "POST",
            "/v1/chat",
            json!({"request_id":id(),"message":message}),
        )
        .await;
        assert!(
            wire["wire"].as_str().unwrap().contains("event: done"),
            "{message}: {wire}"
        );
    }
    assert_eq!(
        seen.lock().unwrap().len(),
        5,
        "only explicit previews and chat calls"
    );
    assert_eq!(json!(*app.persona.read().unwrap()), json!(persona));
    let (_, after) = call(&router, "GET", "/v1/console/persona/context", json!({})).await;
    assert_eq!(before, after);
    let original = before["source_hash"].clone();
    persona.description.push_str("偏爱留白。");
    call(&router, "PUT", "/v1/console/persona", json!(persona)).await;
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/console/persona/preview",
            json!({"message":"看看","source_hash":original})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    for task in tasks {
        task.abort();
    }
}
#[tokio::test]
async fn old_layers_are_read_only_archives_and_never_enter_runtime_or_tools() {
    let (_temp, app, seen, tasks, _stop) = setup().await;
    for key in ["console.persona_derivation.v1", "agent.evolution.v1"] {
        sqlx::query("INSERT INTO metadata(key,value) VALUES(?,?)")
            .bind(key)
            .bind(json!({"text":"旧层必须保留但不注入","generation":{"running":true}}).to_string())
            .execute(&app.store.pool)
            .await
            .unwrap();
    }
    let router = api::router(app.clone());
    let (status, archive) = call(&router, "GET", "/v1/console/persona/archive", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(archive["metadata"].as_array().unwrap().len(), 2);
    for (method, path) in [
        ("PUT", "/v1/console/persona/derivation"),
        ("POST", "/v1/console/persona/derivation/generate"),
        ("POST", "/v1/console/persona/evolution/reflect"),
        ("PUT", "/v1/console/persona/evolution"),
    ] {
        assert_eq!(
            call(&router, method, path, json!({})).await.0,
            StatusCode::GONE
        );
    }
    let (_, context) = call(&router, "GET", "/v1/console/persona/context", json!({})).await;
    assert!(
        !context["system_prompt"]
            .as_str()
            .unwrap()
            .contains("旧层必须保留")
    );
    assert!(
        !mneme::tools::definitions()
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "self_prompt")
    );
    assert!(seen.lock().unwrap().is_empty());
    for task in tasks {
        task.abort();
    }
}
#[tokio::test]
async fn extraction_keeps_facts_without_persona_compilation_or_feedback_classification() {
    let (_temp, app, seen, tasks, _stop) = setup().await;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let mut worker_app = app.clone();
    worker_app.shutdown = rx;
    let session = app.store.conversation().await.unwrap();
    let mneme::store::BeginTurn::Started(user) = app
        .store
        .begin_turn(&session, &id(), "以后聊天不要每次都给建议，先听我说。")
        .await
        .unwrap()
    else {
        panic!()
    };
    app.store.complete_turn(&user, "明白。").await.unwrap();
    sqlx::query(
        "INSERT INTO jobs(id,kind,payload,available_at,created_at) VALUES(?,'extract',?,?,?)",
    )
    .bind(id())
    .bind(&user.id)
    .bind(now())
    .bind(now())
    .execute(&app.store.pool)
    .await
    .unwrap();
    let worker = tokio::spawn(mneme::memory::worker(worker_app));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let done: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM extracted_messages WHERE message_id=?)",
            )
            .bind(&user.id)
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
            if done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    {
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let prompt = requests[0]["messages"][0]["content"].as_str().unwrap();
        assert!(!prompt.contains("feedback_source_ids"));
        assert!(!prompt.contains("只读人格上下文"));
    }
    let pending: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM evolution_feedback")
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
    assert_eq!(pending, 0);
    stop.send(true).unwrap();
    worker.await.unwrap();
    for task in tasks {
        task.abort();
    }
}
