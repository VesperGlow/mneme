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
    console_store::{MemoryWrite, WriteResult},
    domain::*,
    store::BeginTurn,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;
use tower::ServiceExt;

const TOKEN: &str = "control-test-token-01234567890123456789";

struct Fixture {
    _temp: tempfile::TempDir,
    app: App,
    _stop: tokio::sync::watch::Sender<bool>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    arrived: Arc<Notify>,
    release: Arc<Notify>,
    seen: Arc<Mutex<Vec<Value>>>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn setup(tool: bool) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let (base, fake) = common::fake_services().await;
    let arrived = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (signal, gate, capture) = (arrived.clone(), release.clone(), seen.clone());
    let provider = Router::new().route("/chat/completions", post(move |Json(body): Json<Value>| {
        let (signal, gate, capture) = (signal.clone(), gate.clone(), capture.clone());
        async move {
            capture.lock().unwrap().push(body.clone());
            let blocked = body["messages"].as_array().unwrap().last().unwrap()["content"] == "blocked";
            if blocked && tool {
                let data = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call1","type":"function","function":{"name":"browser","arguments":"{\"action\":\"read\"}"}}]},"finish_reason":"tool_calls"}]});
                return ([("content-type","text/event-stream")],format!("data: {data}\n\ndata: [DONE]\n\n")).into_response();
            }
            let stream = async_stream::stream! {
                let first = json!({"choices":[{"delta":{"content":"临时内容"},"finish_reason":null}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}});
                yield Ok::<_,std::convert::Infallible>(format!("data: {first}\n\n"));
                if blocked { signal.notify_one(); gate.notified().await; }
                yield Ok("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_owned());
            };
            ([("content-type","text/event-stream")], Body::from_stream(stream)).into_response()
        }
    }));
    let signal = arrived.clone();
    let gate = release.clone();
    let provider = provider.route(
        "/v1/execute",
        post(move || {
            let signal = signal.clone();
            let gate = gate.clone();
            async move {
                signal.notify_one();
                gate.notified().await;
                Json(json!({"result":{"text":"已读取"},"elapsed_ms":1}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let llm = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    let mut c: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    c.database = temp.path().join("db").to_string_lossy().into();
    c.api_token = TOKEN.into();
    c.llm_key = "fixture".into();
    c.neo4j_password = "fixture".into();
    c.llm.base_url = llm.clone();
    c.llm.model = "fixture-model".into();
    c.models.base_url = base.clone();
    c.neo4j.base_url = base;
    c.memory.extract_every_tokens = 100000;
    c.memory.reconcile_updates = false;
    c.tools.enabled = tool;
    c.tools.base_url = llm;
    c.tools.token = "control-tools-fixture-token-0123456789".into();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    Fixture {
        _temp: temp,
        app,
        _stop: stop,
        tasks: vec![fake, task],
        arrived,
        release,
        seen,
    }
}

async fn response(
    router: &Router,
    method: &str,
    path: &str,
    body: Value,
) -> axum::response::Response {
    router
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
        .unwrap()
}
async fn call(router: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = response(router, method, path, body).await;
    let status = response.status();
    (
        status,
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap(),
    )
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("operation did not stop promptly")
}

#[tokio::test]
async fn cancellation_interrupts_stream_and_tools_preserves_usage_and_unlocks_conversation() {
    for tool in [false, true] {
        let f = setup(tool).await;
        let router = api::router(f.app.clone());
        let turn = id();
        let stream = response(
            &router,
            "POST",
            "/v1/chat",
            json!({"request_id":turn,"message":"blocked"}),
        )
        .await;
        assert_eq!(stream.status(), StatusCode::OK);
        bounded(f.arrived.notified()).await;
        if !tool {
            bounded(async {
                loop {
                    let recorded: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM model_requests WHERE turn_id=? AND json_extract(usage,'$.total_tokens')=12)")
                        .bind(&turn).fetch_one(&f.app.store.pool).await.unwrap();
                    if recorded { break; }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }).await;
        }
        let (status, result) = bounded(call(
            &router,
            "POST",
            &format!("/v1/turns/{turn}/cancel"),
            json!({}),
        ))
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result["status"], "cancelled");
        assert_eq!(result["persisted"], false);
        let wire = bounded(to_bytes(stream.into_body(), 1024 * 1024))
            .await
            .unwrap();
        let wire = String::from_utf8(wire.to_vec()).unwrap();
        assert!(wire.contains("cancelled"));
        assert!(!wire.contains("event: done"));
        let (_, state) = call(&router, "GET", &format!("/v1/turns/{turn}"), json!({})).await;
        assert_eq!(state["status"], "failed");
        assert_eq!(state["error"], "cancelled");
        assert_eq!(state["messages"].as_array().unwrap().len(), 1);
        if !tool {
            assert_eq!(state["usage"]["total_tokens"], 12);
        }
        let running: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM model_requests WHERE turn_id=? AND status='running'",
        )
        .bind(&turn)
        .fetch_one(&f.app.store.pool)
        .await
        .unwrap();
        assert_eq!(running, 0);
        if tool {
            let status: String =
                sqlx::query_scalar("SELECT status FROM tool_calls WHERE turn_id=?")
                    .bind(&turn)
                    .fetch_one(&f.app.store.pool)
                    .await
                    .unwrap();
            assert_eq!(status, "failed");
        }
        assert_eq!(
            call(
                &router,
                "POST",
                &format!("/v1/turns/{turn}/cancel"),
                json!({})
            )
            .await
            .1["status"],
            "cancelled"
        );
        assert_eq!(
            call(
                &router,
                "POST",
                "/v1/chat",
                json!({"request_id":turn,"message":"blocked"})
            )
            .await
            .0,
            StatusCode::CONFLICT
        );
        f.release.notify_one();
        let next = response(
            &router,
            "POST",
            "/v1/chat",
            json!({"request_id":id(),"message":"继续"}),
        )
        .await;
        let wire = bounded(to_bytes(next.into_body(), 1024 * 1024))
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&wire).contains("event: done"));
        assert_eq!(
            f.seen.lock().unwrap().len(),
            2,
            "cancelled tool round must not issue a followup LLM request"
        );
    }
}

#[tokio::test]
async fn cancellation_and_completion_have_one_durable_winner() {
    let f = setup(false).await;
    for _ in 0..5 {
        let session = f.app.store.conversation().await.unwrap();
        let BeginTurn::Started(user) = f
            .app
            .store
            .begin_turn(&session, &id(), "竞态")
            .await
            .unwrap()
        else {
            panic!()
        };
        let (complete, cancel) = tokio::join!(
            f.app.store.complete_turn(&user, "完成"),
            f.app.store.cancel_turn(&user.turn_id)
        );
        let (status, error) = cancel.unwrap().unwrap();
        assert_eq!(complete.is_ok(), status == "complete");
        assert_eq!(
            error.as_deref(),
            if complete.is_ok() {
                None
            } else {
                Some("cancelled")
            }
        );
        let assistants: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM messages WHERE turn_id=? AND role='assistant'",
        )
        .bind(&user.turn_id)
        .fetch_one(&f.app.store.pool)
        .await
        .unwrap();
        assert_eq!(assistants, i64::from(complete.is_ok()));
    }
}

async fn seed(app: &App) -> (String, String) {
    let session = app.store.conversation().await.unwrap();
    let BeginTurn::Started(user) = app
        .store
        .begin_turn(&session, &id(), "我住在成都，周六想去展览，目前没有决定。")
        .await
        .unwrap()
    else {
        panic!()
    };
    app.store
        .complete_turn(&user, "展览还是计划。")
        .await
        .unwrap();
    let mut candidates = Vec::new();
    for (key, content, quote) in [
        ("user.residence", "用户住在成都。", "我住在成都"),
        (
            "user.exhibition",
            "用户周六想去展览，目前没有决定。",
            "周六想去展览，目前没有决定",
        ),
    ] {
        let c:Candidate=serde_json::from_value(json!({"key":key,"content":content,"kind":"fact","importance":0.8,"confidence":0.98,"entities":[],"source_quote":quote,"valid_until":null,"equivalent_to":null})).unwrap();
        let mut vector = vec![0f32; 1024];
        vector[usize::from(key != "user.residence")] = 1.0;
        candidates.push((c, vector, None));
    }
    app.store
        .apply_candidates(&user, candidates, "none")
        .await
        .unwrap();
    sqlx::query("INSERT INTO conversation_context(session_id,summary,through_seq) VALUES(?,'用户住在成都，计划看展览。',0)").bind(&session).execute(&app.store.pool).await.unwrap();
    let residence: String =
        sqlx::query_scalar("SELECT id FROM memories WHERE slot='user.residence'")
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
    (residence, session)
}

#[tokio::test]
async fn confirmed_forgetting_is_atomic_replayable_and_separates_audit_from_context() {
    let f = setup(false).await;
    let (residence, session) = seed(&f.app).await;
    let router = api::router(f.app.clone());
    let (_, preview) = call(
        &router,
        "POST",
        "/v1/memories/forget/preview",
        json!({"query":"成都"}),
    )
    .await;
    assert_eq!(
        f.app
            .store
            .memory(&residence)
            .await
            .unwrap()
            .unwrap()
            .status,
        "active"
    );
    let input = json!({"token":preview["token"],"memory_ids":[residence],"scope":"context","confirmation":"FORGET"});
    let (status, result) = call(
        &router,
        "POST",
        "/v1/memories/forget/confirm",
        input.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["originals_retained"], true);
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/memories/forget/confirm",
            input.clone()
        )
        .await
        .1,
        result
    );
    let mut changed = input;
    changed["scope"] = json!("memory");
    assert_eq!(
        call(&router, "POST", "/v1/memories/forget/confirm", changed)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let context = mneme::continuity::status(&f.app, &session).await.unwrap();
    assert_eq!(context["summary"], "");
    assert_eq!(context["history_turns"], 0);
    assert_eq!(f.app.store.console_history().await.unwrap().len(), 2);
    assert_eq!(f.app.store.sources(&residence).await.unwrap().len(), 1);
    assert_eq!(f.app.store.active_memories(10).await.unwrap().len(), 1);
    let stream = response(
        &router,
        "POST",
        "/v1/chat",
        json!({"request_id":id(),"message":"展览还是计划吗"}),
    )
    .await;
    bounded(to_bytes(stream.into_body(), 1024 * 1024))
        .await
        .unwrap();
    let request = f.seen.lock().unwrap().last().unwrap().clone();
    let text = request["messages"].to_string();
    assert!(
        !text.contains("成都"),
        "shared source quote must not reintroduce the deleted residence: {text}"
    );
    assert!(
        text.contains("目前没有决定"),
        "unrelated long-term facts remain available"
    );
}

#[tokio::test]
async fn forgetting_refuses_changed_expired_unreviewed_or_busy_plans_without_partial_deletion() {
    let f = setup(false).await;
    let (residence, session) = seed(&f.app).await;
    let router = api::router(f.app.clone());
    let preview = call(
        &router,
        "POST",
        "/v1/memories/forget/preview",
        json!({"query":"用户"}),
    )
    .await
    .1;
    let candidates = preview["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 2);
    let ids: Vec<_> = candidates.iter().map(|m| m["id"].clone()).collect();
    let input =
        json!({"token":preview["token"],"memory_ids":ids,"scope":"memory","confirmation":"FORGET"});
    let mut invalid = input.clone();
    invalid["memory_ids"] = json!([id()]);
    assert_eq!(
        call(&router, "POST", "/v1/memories/forget/confirm", invalid)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let BeginTurn::Started(user) = f
        .app
        .store
        .begin_turn(&session, &id(), "新的对话")
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/memories/forget/confirm",
            input.clone()
        )
        .await
        .1["error"],
        "stop_generation_before_forgetting"
    );
    f.app.store.complete_turn(&user, "新的回复").await.unwrap();
    let mut changed_context = input.clone();
    changed_context["scope"] = json!("context");
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/memories/forget/confirm",
            changed_context
        )
        .await
        .1["error"],
        "forget_context_changed"
    );
    // Change the last candidate so confirmation has to roll back earlier deletions.
    let last = candidates.last().unwrap()["id"].as_str().unwrap();
    let result = f
        .app
        .store
        .manual_memory(
            Some(last),
            &MemoryWrite {
                content: "用户更新了该记录。".into(),
                kind: "fact".into(),
                importance: 0.8,
                confidence: 1.0,
                entities: vec![],
                valid_until: None,
                expected_revision: candidates.last().unwrap()["revision"].as_i64(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(result, WriteResult::Saved(_)));
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/memories/forget/confirm",
            input.clone()
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(
        f.app
            .store
            .memory(&residence)
            .await
            .unwrap()
            .unwrap()
            .deleted_at
            .is_none()
    );
    assert_eq!(f.app.store.active_memories(10).await.unwrap().len(), 2);
    sqlx::query("UPDATE forget_requests SET expires_at=0")
        .execute(&f.app.store.pool)
        .await
        .unwrap();
    assert_eq!(
        call(&router, "POST", "/v1/memories/forget/confirm", input)
            .await
            .1["error"],
        "forget_preview_expired"
    );
}
