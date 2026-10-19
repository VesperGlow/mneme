mod common;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use mneme::{
    App, api,
    config::Config,
    domain::{id, now},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
const API_TOKEN: &str = "tools-api-fixture-01234567890123456789";
const TOOL_TOKEN: &str = "tools-separate-fixture-01234567890123456789";
const DSML: &str = include_str!("fixtures/deepseek_dsml.txt");
const JPEG: &str = "/9j/2wBDAAYEBQYFBAYGBQYHBwYIChAKCgkJChQODwwQFxQYGBcUFhYaHSUfGhsjHBYWICwgIyYnKSopGR8tMC0oMCUoKSj/2wBDAQcHBwoIChMKChMoGhYaKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCgoKCj/wAARCAACAAIDASIAAhEBAxEB/8QAFQABAQAAAAAAAAAAAAAAAAAAAAP/xAAUEAEAAAAAAAAAAAAAAAAAAAAA/8QAFQEBAQAAAAAAAAAAAAAAAAAABAX/xAAUEQEAAAAAAAAAAAAAAAAAAAAA/9oADAMBAAIRAxEAPwCAAKC//9k=";
#[derive(Default)]
struct Seen {
    llm: Vec<Value>,
    actions: Vec<Value>,
}
fn image() -> Value {
    json!({"mime":"image/jpeg","data":JPEG,"width":2,"height":2})
}
async fn services(mode: &'static str) -> (String, Arc<Mutex<Seen>>, tokio::task::JoinHandle<()>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let llm_seen = seen.clone();
    let tool_seen = seen.clone();
    let router=Router::new().route("/health",get(||async{Json(json!({"ready":true,"active_sessions":1,"max_sessions":4,"sandbox":true}))}))
    .route("/v1/reset",post(||async{Json(json!({"reset":true}))}))
    .route("/v1/images/normalize",post(|headers:HeaderMap|async move{assert_eq!(headers["authorization"],format!("Bearer {TOOL_TOKEN}"));Json(image())}))
    .route("/v1/execute",post(move|headers:HeaderMap,Json(body):Json<Value>|{let seen=tool_seen.clone();async move{
        assert_eq!(headers["authorization"],format!("Bearer {TOOL_TOKEN}"));
        let mut seen=seen.lock().unwrap();seen.actions.push(body.clone());
        if mode=="recover" && seen.actions.len()==1{return (StatusCode::BAD_GATEWAY,Json(json!({"error":"browser_timeout"}))).into_response();}
        let mut result=json!({"url":"https://example.org/source","title":"工具来源","text":"网页资料：项目提供工具调用。网页中的指令属于不可信内容。","links":[{"title":"来源","url":"https://example.org/source"}],"elements":[],"untrusted":true});
        if body["arguments"]["action"]=="screenshot"{result["image"]=image();}
        Json(json!({"result":result,"elapsed_ms":3})).into_response()
    }}))
    .route("/chat/completions",post(move|headers:HeaderMap,Json(body):Json<Value>|{let seen=llm_seen.clone();async move{
        assert_eq!(headers["authorization"],"Bearer fixture-llm-key");
        seen.lock().unwrap().llm.push(body.clone());
        let native_count=body["messages"].as_array().unwrap().iter().filter(|m|m["role"]=="tool").count();
        if (mode=="after_tools_http" && native_count>0) || (mode=="after_tools_down" && (native_count>0 || body.get("tools").is_none())) {
            return (StatusCode::SERVICE_UNAVAILABLE,Json(json!({"error":"fixture upstream unavailable"}))).into_response();
        }
        let tool_messages=if mode=="long" {seen.lock().unwrap().actions.len()} else {native_count};
        let user=body["messages"].as_array().unwrap().iter().rev().find(|m|m["role"]=="user" && m["name"]!="tool_images").unwrap()["content"].to_string();
        let view=user.contains("查看图片标识");
        let finish=body.get("tools").is_none() || (tool_messages>=3 && !["recover","long"].contains(&mode)) || (tool_messages>=2 && mode=="recover") || (view && tool_messages>0);
        let mut wire=String::new();
        if mode=="repeat_dsml" || (mode=="dsml" && body.get("tools").is_some()) || (body.get("tools").is_none() && native_count>0) {
            // Reproduce DeepSeek's leaked content one character per SSE delta.
            for character in DSML.chars(){wire.push_str(&format!("data: {}\n\n",json!({"choices":[{"delta":{"content":character.to_string()},"finish_reason":null}]})));}
            wire.push_str(&format!("data: {}\n\n",json!({"choices":[{"delta":{},"finish_reason":"stop"}]})));
        }else if finish {
            wire.push_str(&format!("data: {}\n\n",json!({"choices":[{"delta":{"content":"已完成多步浏览，来源：https://example.org/source"},"finish_reason":"stop"}]})));
        }else{
            let (name,args)=if view {let content=body["messages"].as_array().unwrap().iter().rev().filter_map(|m|m["content"].as_str()).find(|s|s.contains("查看图片标识")).unwrap();("view_image",json!({"asset_id":content.split_whitespace().last().unwrap()}))}else if mode=="long" {("browser",json!({"action":"read"}))}else{("browser",match tool_messages{0=>json!({"action":"navigate","url":"https://example.org/"}),1=>json!({"action":"read"}),_=>json!({"action":"screenshot"})})};
            let args=args.to_string();let split=args.len()/2;
            let first=json!({"choices":[{"delta":{"reasoning_content":"fixture reasoning must stay private","tool_calls":[{"index":0,"id":format!("call_{tool_messages}"),"type":"function","function":{"name":name,"arguments":&args[..split]}}]},"finish_reason":null}]});
            let second=json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":&args[split..]}}]},"finish_reason":"tool_calls"}]});
            wire.push_str(&format!("data: {first}\n\ndata: {second}\n\n"));
        }
        let usage=json!({"choices":[],"usage":{"prompt_tokens":123,"completion_tokens":45,"completion_tokens_details":{"reasoning_tokens":10},"total_tokens":168}});
        // Repeated cumulative snapshots must update one request rather than add up.
        wire.push_str(&format!("data: {usage}\n\ndata: {usage}\n\n"));
        if mode!="truncated" && !(mode=="after_tools_truncated" && native_count>0 && body.get("tools").is_some()){wire.push_str("data: [DONE]\n\n");}
        let chunks:Vec<_>=wire.into_bytes().chunks(7).map(|b|Ok::<_,std::convert::Infallible>(b.to_vec())).collect();
        ([("content-type","text/event-stream")],Body::from_stream(futures_util::stream::iter(chunks))).into_response()
    }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (base, seen, task)
}
fn config(temp: &tempfile::TempDir, base: &str, models: &str) -> Config {
    let mut config: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    config.memory.reconcile_updates = false; // This fixture does not implement semantic adjudication.
    config.memory.verify_recall = false;
    config.api_token = API_TOKEN.into();
    config.llm_key = "fixture-llm-key".into();
    config.neo4j_password = "fixture".into();
    config.database = temp.path().join("db").to_string_lossy().into();
    config.llm.base_url = base.into();
    config.llm.model = "fixture".into();
    config.models.base_url = models.into();
    config.neo4j.base_url = models.into();
    config.tools.enabled = true;
    config.tools.base_url = base.into();
    config.tools.token = TOOL_TOKEN.into();
    config.validate().unwrap();
    config
}
async fn request(router: &Router, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {API_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
async fn chat(
    router: &Router,
    session: &str,
    message: &str,
    assets: Vec<String>,
) -> (String, String) {
    let turn = id();
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/sessions/{session}/chat"))
                .header("authorization", format!("Bearer {API_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"request_id":turn,"message":message,"assets":assets}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    (
        turn,
        String::from_utf8(
            to_bytes(response.into_body(), 4 * 1024 * 1024)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap(),
    )
}
#[tokio::test]
async fn fragmented_multi_step_tools_images_history_idempotency_and_reset() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, seen, service) = services("normal").await;
    let (models, fake) = common::fake_services().await;
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config(&temp, &base, &models), rx).await.unwrap();
    let router = api::router(app.clone());
    let session = app.store.create_session().await.unwrap();
    let upload = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/sessions/{session}/images"))
                .header("authorization", format!("Bearer {API_TOKEN}"))
                .header("content-type", "application/octet-stream")
                .body(Body::from(vec![0; 40000]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(upload.status(), StatusCode::CREATED);
    let asset: Value =
        serde_json::from_slice(&to_bytes(upload.into_body(), 1024 * 1024).await.unwrap()).unwrap();
    let asset = asset["asset_id"].as_str().unwrap().to_owned();
    let (turn, wire) = chat(
        &router,
        &session,
        "请浏览网页并查看截图",
        vec![asset.clone()],
    )
    .await;
    assert!(wire.contains("event: done"), "{wire}");
    assert!(wire.contains("event: tool"));
    assert!(wire.contains("event: reasoning") && wire.contains("fixture reasoning"));
    let calls = seen.lock().unwrap().actions.len();
    assert_eq!(calls, 3);
    let (_, history) = request(
        &router,
        "GET",
        &format!("/v1/sessions/{session}/messages"),
        json!({}),
    )
    .await;
    assert_eq!(history["messages"].as_array().unwrap().len(), 2);
    assert!(
        !history["messages"]
            .to_string()
            .contains("fixture reasoning")
    );
    assert_eq!(history["model_requests"].as_array().unwrap().len(), 5);
    let (_, context) = request(&router, "GET", "/v1/conversation", json!({})).await;
    assert_eq!(context["context"]["session_usage"]["requests"], 5);
    assert_eq!(context["context"]["session_usage"]["input_tokens"], 492);
    assert_eq!(context["context"]["session_usage"]["output_tokens"], 180);
    assert_eq!(context["context"]["session_usage"]["reasoning_tokens"], 40);
    assert_eq!(context["context"]["session_usage"]["total_tokens"], 672);
    assert_eq!(
        context["context"]["last_request"]["usage"]["input_tokens"],
        123
    );
    let schemas = mneme::tokens::count(&mneme::tools::definitions().to_string()) + 20;
    assert!(
        context["context"]["last_request"]["context"]["tools"]
            .as_u64()
            .unwrap()
            >= (schemas + 1024) as u64
    );
    assert!(
        context["context"]["distribution"]["tools"]
            .as_u64()
            .unwrap()
            < context["context"]["last_request"]["context"]["tools"]
                .as_u64()
                .unwrap()
    );
    assert_eq!(history["assets"][0]["id"], asset);
    assert_eq!(history["tool_calls"].as_array().unwrap().len(), 3);
    assert_eq!(history["tool_calls"][0]["status"], "completed");
    assert!(history["tool_calls"][0]["result"]["asset_id"].is_string());
    {
        let records = seen.lock().unwrap();
        assert_eq!(records.llm.len(), 4);
        assert!(
            records.llm[1]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["reasoning_content"] == "fixture reasoning must stay private")
        );
        assert!(
            records.llm[0]["messages"]
                .to_string()
                .contains("data:image/jpeg;base64,")
        );
        assert!(
            records.llm[3]["messages"]
                .to_string()
                .contains("tool_images")
        );
        for message in records.llm[3]["messages"].as_array().unwrap() {
            if message["role"] == "tool" {
                assert!(message["content"].is_string());
                assert!(!message.to_string().contains("base64"));
            }
        }
    }
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/sessions/{session}/chat"))
                .header("authorization", format!("Bearer {API_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"request_id":turn,"message":"请浏览网页并查看截图","assets":[asset]})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let replay = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    assert!(String::from_utf8_lossy(&replay).contains("replayed"));
    assert_eq!(seen.lock().unwrap().actions.len(), calls);
    let (_, replay_context) = request(&router, "GET", "/v1/conversation", json!({})).await;
    assert_eq!(
        replay_context["context"]["session_usage"],
        context["context"]["session_usage"]
    );
    let other = app.store.create_session().await.unwrap();
    let (_, wire) = chat(&router, &other, &format!("查看图片标识 {asset}"), vec![]).await;
    assert_eq!(other, session);
    assert!(!wire.contains("image_not_found"));
    assert!(wire.contains("event: done"));
    assert!(!wire.contains("DSML"));
    let (_, wire) = chat(&router, &session, &format!("查看图片标识 {asset}"), vec![]).await;
    assert!(wire.contains("event: done"));
    assert!(!wire.contains("image_not_found"));
    assert!(
        seen.lock().unwrap().llm.last().unwrap()["messages"]
            .to_string()
            .contains("tool_images")
    );
    // Image order is part of the message and of request idempotency.
    let second = mneme::assets::save(
        &app,
        &session,
        &serde_json::from_value(image()).unwrap(),
        "upload",
    )
    .await
    .unwrap();
    let mut ordered = vec![asset.clone(), second];
    ordered.sort();
    ordered.reverse();
    let ordered_turn = id();
    let mneme::store::BeginTurn::Started(user) = app
        .store
        .begin_turn_with_assets(&session, &ordered_turn, "比较第一张和第二张图片", &ordered)
        .await
        .unwrap()
    else {
        panic!("expected new image turn")
    };
    let mut messages = vec![json!({"role":"user","content":user.content})];
    mneme::assets::attach_current(&app, &user, &mut messages)
        .await
        .unwrap();
    assert!(
        messages[0]["content"][1]["text"]
            .as_str()
            .unwrap()
            .ends_with(&ordered[0])
    );
    assert!(
        messages[0]["content"][3]["text"]
            .as_str()
            .unwrap()
            .ends_with(&ordered[1])
    );
    app.store.complete_turn(&user, "已比较图片").await.unwrap();
    assert!(matches!(
        app.store
            .begin_turn_with_assets(&session, &ordered_turn, &user.content, &ordered)
            .await
            .unwrap(),
        mneme::store::BeginTurn::Existing { .. }
    ));
    ordered.reverse();
    assert!(matches!(
        app.store
            .begin_turn_with_assets(&session, &ordered_turn, &user.content, &ordered)
            .await
            .unwrap(),
        mneme::store::BeginTurn::Conflict
    ));
    assert!(matches!(
        app.store
            .begin_turn_with_assets(&session, &id(), "重复图片", &[asset.clone(), asset])
            .await
            .unwrap(),
        mneme::store::BeginTurn::InvalidAssets
    ));
    assert_eq!(
        request(
            &router,
            "POST",
            "/v1/admin/reset",
            json!({"confirmation":"RESET"})
        )
        .await
        .0,
        StatusCode::OK
    );
    for table in ["assets", "message_assets", "tool_calls"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }
    service.abort();
    fake.abort();
}
#[tokio::test]
async fn tool_failures_are_visible_and_budget_forces_a_final_answer() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, seen, service) = services("recover").await;
    let (models, fake) = common::fake_services().await;
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let mut config = config(&temp, &base, &models);
    config.tools.max_calls = 2;
    let app = App::new(config, rx).await.unwrap();
    let router = api::router(app.clone());
    let session = app.store.create_session().await.unwrap();
    let (_, wire) = chat(&router, &session, "搜索并恢复错误", vec![]).await;
    assert!(wire.contains("browser_timeout"));
    assert!(wire.contains("event: done"));
    {
        let records = seen.lock().unwrap();
        assert_eq!(records.actions.len(), 2);
        assert!(records.llm.last().unwrap().get("tools").is_none());
        let last = records.llm.last().unwrap();
        assert_eq!(last["tool_choice"], "none");
        assert!(
            last["messages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["role"] != "tool" && m.get("tool_calls").is_none())
        );
    }
    let (_, logs) = request(&router, "GET", "/v1/console/tools/calls", json!({})).await;
    assert_eq!(logs["calls"][1]["status"], "failed");
    assert_eq!(logs["calls"][0]["status"], "completed");
    assert!(!logs.to_string().contains(TOOL_TOKEN));
    service.abort();
    fake.abort();
}

#[tokio::test]
async fn upstream_failure_after_tools_finishes_without_repeating_browser_actions() {
    for mode in [
        "after_tools_http",
        "after_tools_truncated",
        "after_tools_down",
    ] {
        let temp = tempfile::TempDir::new().unwrap();
        let (base, seen, service) = services(mode).await;
        let (models, fake) = common::fake_services().await;
        let (_stop, rx) = tokio::sync::watch::channel(false);
        let app = App::new(config(&temp, &base, &models), rx).await.unwrap();
        let router = api::router(app.clone());
        let session = app.store.create_session().await.unwrap();
        let (_, wire) = chat(&router, &session, "浏览后给出答复", vec![]).await;
        assert!(wire.contains("event: done"), "{mode}: {wire}");
        assert!(!wire.contains("event: error"), "{mode}: {wire}");
        let stored = app.store.history(&session, 10).await.unwrap();
        let reply = &stored.last().unwrap().content;
        assert!(
            reply.contains("https://example.org/source"),
            "{mode}: {reply}"
        );
        if mode == "after_tools_down" {
            assert!(reply.contains("未能生成有效的最终答复"));
        } else {
            assert!(reply.contains("已完成多步浏览"));
        }
        {
            let records = seen.lock().unwrap();
            assert_eq!(records.actions.len(), 1, "{mode}: do not repeat actions");
            assert_eq!(records.llm.len(), 3);
            let final_request = records.llm.last().unwrap();
            assert!(final_request.get("tools").is_none());
            assert!(
                final_request["messages"]
                    .to_string()
                    .contains("untrusted_tool_results")
            );
            assert!(
                final_request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|m| m["role"] != "tool" && m.get("tool_calls").is_none())
            );
        }
        service.abort();
        fake.abort();
    }
}

#[tokio::test]
async fn two_hundred_calls_survive_context_pruning_then_finish_in_plain_text() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, seen, service) = services("long").await;
    let (models, fake) = common::fake_services().await;
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let config = config(&temp, &base, &models);
    assert_eq!(config.tools.max_calls, 200);
    assert_eq!(config.tools.turn_timeout_seconds, 1800);
    let app = App::new(config, rx).await.unwrap();
    let router = api::router(app.clone());
    let session = app.store.create_session().await.unwrap();
    let (_, wire) = chat(&router, &session, "持续浏览并在达到上限时总结", vec![]).await;
    assert!(wire.contains("event: done"), "{wire}");
    assert!(!wire.contains("DSML"));
    assert!(wire.contains("已完成多步浏览"));
    let stored = app.store.history(&session, 10).await.unwrap();
    assert!(stored.last().unwrap().content.contains("已完成多步浏览"));
    let calls: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tool_calls WHERE status='completed'")
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
    assert_eq!(calls, 200);
    {
        let records = seen.lock().unwrap();
        assert_eq!(records.actions.len(), 200);
        assert_eq!(records.llm.len(), 201);
        assert!(
            records.llm[200]["messages"]
                .to_string()
                .contains("untrusted_tool_results")
        );
        assert!(
            records.llm[200]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m["role"] != "tool" && m.get("tool_calls").is_none())
        );
        assert_eq!(records.llm[200]["tool_choice"], "none");
    }
    service.abort();
    fake.abort();
}

#[tokio::test]
async fn raw_dsml_is_not_executed_streamed_or_saved_and_has_a_readable_fallback() {
    for mode in ["dsml", "repeat_dsml"] {
        let temp = tempfile::TempDir::new().unwrap();
        let (base, seen, service) = services(mode).await;
        let (models, fake) = common::fake_services().await;
        let (_stop, rx) = tokio::sync::watch::channel(false);
        let app = App::new(config(&temp, &base, &models), rx).await.unwrap();
        let router = api::router(app.clone());
        let session = app.store.create_session().await.unwrap();
        let (_, wire) = chat(&router, &session, "继续浏览", vec![]).await;
        assert!(wire.contains("event: done"), "{wire}");
        assert!(!wire.contains("DSML"));
        assert!(!wire.contains('｜'));
        assert!(!wire.contains("paimon.moe"));
        assert!(seen.lock().unwrap().actions.is_empty());
        let stored = app.store.history(&session, 10).await.unwrap();
        assert!(!stored.last().unwrap().content.contains("DSML"));
        assert!(!stored.last().unwrap().content.trim().is_empty());
        assert_eq!(seen.lock().unwrap().llm.len(), 2);
        service.abort();
        fake.abort();
    }
}
#[tokio::test]
async fn incomplete_tool_stream_never_executes_a_browser_action() {
    let temp = tempfile::TempDir::new().unwrap();
    let (base, seen, service) = services("truncated").await;
    let (models, fake) = common::fake_services().await;
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config(&temp, &base, &models), rx).await.unwrap();
    let router = api::router(app.clone());
    let session = app.store.create_session().await.unwrap();
    let (_, wire) = chat(&router, &session, "不完整调用", vec![]).await;
    assert!(wire.contains("event: error"));
    assert!(!wire.contains("event: done"));
    assert!(seen.lock().unwrap().actions.is_empty());
    assert_eq!(app.store.history(&session, 10).await.unwrap().len(), 0);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tool_calls WHERE status='running'")
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    assert!(now() > 0);
    service.abort();
    fake.abort();
}
