mod common;
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::post,
};
use mneme::{App, api, config::Config, domain::id, persona::DIMENSIONS};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tower::ServiceExt;
const TOKEN: &str = "persona-fixture-token-01234567890123456789";

#[derive(Default)]
struct Seen {
    requests: Mutex<Vec<Value>>,
    bad: AtomicBool,
    long_quote: AtomicBool,
    slow: AtomicBool,
}
async fn provider() -> (String, Arc<Seen>, tokio::task::JoinHandle<()>) {
    let seen = Arc::new(Seen::default());
    let capture = seen.clone();
    let router=Router::new().route("/chat/completions",post(move|Json(body):Json<Value>| {let seen=capture.clone();async move {
        seen.requests.lock().unwrap().push(body.clone());
        if body["stream"]==true {
            let system=body["messages"][0]["content"].as_str().unwrap();
            let reply=if system.contains("<derived_persona_profile") {"画像已参与本轮回复。[[sticker:missing]]"}else{"仅原始人格。[[image:missing]]"};
            let wire=format!("data: {}\n\ndata: [DONE]\n\n",json!({"choices":[{"delta":{"content":reply},"finish_reason":"stop"}]}));
            return ([("content-type","text/event-stream")],wire).into_response();
        }
        if seen.slow.load(Ordering::Relaxed){tokio::time::sleep(std::time::Duration::from_millis(300)).await;}
        let input:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(input.as_object().unwrap().len(),1,"compiler receives only raw persona");
        let quote=if seen.long_quote.load(Ordering::Relaxed){format!("{}{}",input["persona"]["style"].as_str().unwrap(),if seen.bad.load(Ordering::Relaxed){"伪造的后缀"}else{""})}else if seen.bad.load(Ordering::Relaxed){"不存在于原始人格的引用".into()}else{"自然".into()};
        let dimensions:Vec<_>=DIMENSIONS.iter().map(|name|json!({"id":name,"description":format!("{name}：有依据的沉静表达"),"basis":if seen.long_quote.load(Ordering::Relaxed){"依据".repeat(90)}else{"根据交流方式解释，不增加角色经历。".into()},"evidence":[{"field":"style","quote":quote}],"signals":["沉静"]})).collect();
        Json(json!({"choices":[{"message":{"content":json!({"dimensions":dimensions}).to_string()},"finish_reason":"stop"}]})).into_response()
    }}));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, seen, task)
}
fn config(temp: &tempfile::TempDir, base: &str, llm: &str) -> Config {
    let mut c: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    c.database = temp.path().join("db").to_string_lossy().into();
    c.api_token = TOKEN.into();
    c.llm_key = "fixture".into();
    c.neo4j_password = "fixture".into();
    c.models.base_url = base.into();
    c.neo4j.base_url = base.into();
    c.llm.base_url = llm.into();
    c.llm.model = "generic-model-a".into();
    c.llm
        .persona_request_options
        .insert("thinking".into(), json!({"type":"disabled"}));
    c
}
async fn call(router: &Router, method: &str, path: &str, value: Value) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({"wire":String::from_utf8_lossy(&bytes)}));
    (status, body)
}
async fn view(router: &Router) -> Value {
    let (s, v) = call(router, "GET", "/v1/console/persona/derivation", json!({})).await;
    assert_eq!(s, StatusCode::OK);
    v
}
async fn generate(router: &Router, force: bool) -> Value {
    let v = view(router).await;
    let (s, result) = call(
        router,
        "POST",
        "/v1/console/persona/derivation/generate",
        json!({"expected_version":v["version"],"source_hash":v["source_hash"],"force":force}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    result
}
fn edit(v: &Value) -> Value {
    json!({"expected_version":v["version"],"source_hash":v["source_hash"],"enabled":true,"dimensions":v["profile"]["dimensions"]})
}

#[tokio::test]
async fn generated_cache_runtime_preview_restart_and_model_switch() {
    let temp = tempfile::tempdir().unwrap();
    let (base, fake) = common::fake_services().await;
    let (llm, seen, upstream) = provider().await;
    let mut c = config(&temp, &base, &llm);
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c.clone(), rx.clone()).await.unwrap();
    let router = api::router(app.clone());
    assert_eq!(view(&router).await["status"], "empty");
    assert!(seen.requests.lock().unwrap().is_empty());
    let v = generate(&router, false).await;
    assert_eq!(v["status"], "ready");
    assert_eq!(v["version"], 1);
    let again = generate(&router, false).await;
    assert_eq!(again["version"], 1);
    assert_eq!(seen.requests.lock().unwrap().len(), 1);
    assert_eq!(
        seen.requests.lock().unwrap()[0]["thinking"]["type"],
        "disabled"
    );
    for message in ["今天好困。", "这段程序为什么报错？", "我今天心情很差。"]
    {
        let (s, reply) = call(
            &router,
            "POST",
            "/v1/console/persona/preview",
            json!({"message":message,"expected_version":1,"source_hash":v["source_hash"]}),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(reply["reply"], "画像已参与本轮回复。");
        assert_eq!(reply["derived_applied"], true);
    }
    for table in ["messages", "memories", "jobs"] {
        let count: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
        assert_eq!(count, 0, "preview must not write {table}");
    }
    let purposes: Vec<(String, i64)> =
        sqlx::query_as("SELECT purpose,COUNT(*) FROM model_requests GROUP BY purpose")
            .fetch_all(&app.store.pool)
            .await
            .unwrap();
    assert!(purposes.contains(&("persona_compile".into(), 1)));
    assert!(purposes.contains(&("persona_preview".into(), 3)));
    let (s, wire) = call(
        &router,
        "POST",
        "/v1/chat",
        json!({"request_id":id(),"message":"换个话题。"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        wire["wire"]
            .as_str()
            .unwrap()
            .contains("画像已参与本轮回复")
    );
    {
        let requests = seen.requests.lock().unwrap();
        let system = requests.last().unwrap()["messages"][0]["content"]
            .as_str()
            .unwrap();
        assert!(system.contains("原始人格为准"));
        assert!(system.contains("事实准确"));
        assert!(!system.contains("我会参考其中"));
    }
    drop(router);
    app.store.pool.close().await;
    drop(app);
    c.llm.model = "different-model-family".into();
    let restarted = App::new(c, rx).await.unwrap();
    let reopened = api::router(restarted.clone());
    let v = view(&reopened).await;
    assert_eq!(v["version"], 1);
    assert_eq!(v["status"], "ready");
    assert_eq!(v["effective"]["model"], "different-model-family");
    assert_eq!(
        seen.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["stream"] != true)
            .count(),
        1
    );
    fake.abort();
    upstream.abort();
}

#[tokio::test]
async fn manual_edits_locks_stale_review_rollback_disable_and_conflicts() {
    let temp = tempfile::tempdir().unwrap();
    let (base, fake) = common::fake_services().await;
    let (llm, _seen, upstream) = provider().await;
    let c = config(&temp, &base, &llm);
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    let router = api::router(app.clone());
    let v = generate(&router, false).await;
    let mut input = edit(&v);
    input["dimensions"][0]["description"] = json!("人工保留的反应倾向 <script>测试</script>");
    input["dimensions"][0]["locked"] = json!(true);
    input["dimensions"][1]["description"] = json!("未锁定的人工编辑也应保留");
    let (s, saved) = call(
        &router,
        "PUT",
        "/v1/console/persona/derivation",
        input.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        call(&router, "PUT", "/v1/console/persona/derivation", input)
            .await
            .0,
        StatusCode::CONFLICT
    );
    let regenerated = generate(&router, true).await;
    assert_eq!(
        regenerated["profile"]["dimensions"][0]["description"],
        saved["profile"]["dimensions"][0]["description"]
    );
    assert_eq!(regenerated["profile"]["dimensions"][0]["edited"], true);
    assert_eq!(
        regenerated["profile"]["dimensions"][1]["description"],
        "未锁定的人工编辑也应保留"
    );
    let mut raw = serde_json::to_value(app.persona.read().unwrap().clone()).unwrap();
    raw["style"] = json!("自然，表达直接，尊重自主。新的原始设定。");
    assert_eq!(
        call(&router, "PUT", "/v1/console/persona", raw.clone())
            .await
            .0,
        StatusCode::OK
    );
    let stale = view(&router).await;
    assert_eq!(stale["status"], "stale");
    assert_eq!(stale["effective"]["derived_applied"], false);
    assert_eq!(stale["profile"], regenerated["profile"]);
    let reviewed = generate(&router, true).await;
    assert_eq!(reviewed["status"], "needs_review");
    assert_eq!(reviewed["effective"]["derived_applied"], false);
    let (s, active) = call(
        &router,
        "PUT",
        "/v1/console/persona/derivation",
        edit(&reviewed),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(active["status"], "ready");
    let mut disabled = edit(&active);
    disabled["enabled"] = json!(false);
    let (_, disabled) = call(&router, "PUT", "/v1/console/persona/derivation", disabled).await;
    assert_eq!(disabled["status"], "disabled");
    assert_eq!(disabled["effective"]["derived_applied"], false);
    let (s,rolled)=call(&router,"POST","/v1/console/persona/derivation/rollback",json!({"expected_version":disabled["version"],"source_hash":disabled["source_hash"],"target_version":active["version"]})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(rolled["status"], "ready");
    assert!(rolled["version"].as_u64() > disabled["version"].as_u64());
    assert_eq!(
        serde_json::to_value(app.persona.read().unwrap().clone()).unwrap(),
        raw
    );
    fake.abort();
    upstream.abort();
}

#[tokio::test]
async fn failed_or_concurrent_generation_never_overwrites_owner_changes() {
    let temp = tempfile::tempdir().unwrap();
    let (base, fake) = common::fake_services().await;
    let (llm, seen, upstream) = provider().await;
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config(&temp, &base, &llm), rx).await.unwrap();
    let router = api::router(app.clone());
    let v = generate(&router, false).await;
    seen.bad.store(true, Ordering::Relaxed);
    let bad = generate(&router, true).await;
    assert_eq!(bad["profile"], v["profile"]);
    assert_eq!(bad["generation"]["running"], false);
    assert_eq!(bad["generation"]["error"], "persona_generation_failed");
    seen.bad.store(false, Ordering::Relaxed);
    seen.slow.store(true, Ordering::Relaxed);
    let concurrent = router.clone();
    let gen_request =
        json!({"expected_version":v["version"],"source_hash":v["source_hash"],"force":true});
    let task = tokio::spawn(async move {
        call(
            &concurrent,
            "POST",
            "/v1/console/persona/derivation/generate",
            gen_request,
        )
        .await
    });
    loop {
        if view(&router).await["generation"]["running"] == true {
            break;
        }
        tokio::task::yield_now().await;
    }
    let mut input = edit(&v);
    input["dimensions"][1]["description"] = json!("生成期间的新人工编辑");
    let (s, manual) = call(&router, "PUT", "/v1/console/persona/derivation", input).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(task.await.unwrap().0, StatusCode::OK);
    let result = view(&router).await;
    assert_eq!(result["profile"], manual["profile"]);
    assert_eq!(result["generation"]["error"], "persona_derivation_changed");
    fake.abort();
    upstream.abort();
}

#[tokio::test]
async fn interrupted_generation_recovery_authentication_and_reset() {
    let temp = tempfile::tempdir().unwrap();
    let (base, fake) = common::fake_services().await;
    let (llm, _seen, upstream) = provider().await;
    let c = config(&temp, &base, &llm);
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c.clone(), rx.clone()).await.unwrap();
    let router = api::router(app.clone());
    generate(&router, false).await;
    for (method, path) in [
        ("GET", "/v1/console/persona/derivation"),
        ("POST", "/v1/console/persona/preview"),
        ("PUT", "/v1/console/persona/derivation"),
        ("POST", "/v1/console/persona/derivation/generate"),
        ("POST", "/v1/console/persona/derivation/rollback"),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    sqlx::query("UPDATE metadata SET value=json_set(value,'$.generation.running',json('true')) WHERE key='console.persona_derivation.v1'").execute(&app.store.pool).await.unwrap();
    drop(router);
    app.store.pool.close().await;
    drop(app);
    let reopened = App::new(c, rx).await.unwrap();
    let router = api::router(reopened.clone());
    let v = view(&router).await;
    assert_eq!(v["generation"]["running"], false);
    assert_eq!(v["generation"]["error"], "generation_interrupted");
    assert_eq!(v["version"], 1);
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/admin/reset",
            json!({"confirmation":"RESET","reset_persona":false})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(view(&router).await["version"], 1);
    assert_eq!(
        call(
            &router,
            "POST",
            "/v1/admin/reset",
            json!({"confirmation":"RESET","reset_persona":true})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(view(&router).await["status"], "empty");
    fake.abort();
    upstream.abort();
}

#[tokio::test]
async fn corrupt_derived_cache_falls_back_to_raw_persona_without_generation() {
    let temp = tempfile::tempdir().unwrap();
    let (base, fake) = common::fake_services().await;
    let (llm, seen, upstream) = provider().await;
    let c = config(&temp, &base, &llm);
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c.clone(), rx.clone()).await.unwrap();
    sqlx::query(
        "INSERT INTO metadata(key,value) VALUES('console.persona_derivation.v1','broken-json')",
    )
    .execute(&app.store.pool)
    .await
    .unwrap();
    app.store.pool.close().await;
    drop(app);
    let restarted = App::new(c, rx).await.unwrap();
    let state = view(&api::router(restarted)).await;
    assert_eq!(state["status"], "empty");
    assert_eq!(state["generation"]["error"], "persona_cache_invalid");
    assert_eq!(state["effective"]["derived_applied"], false);
    assert!(seen.requests.lock().unwrap().is_empty());
    fake.abort();
    upstream.abort();
}

#[tokio::test]
async fn generated_quotes_are_bounded_only_after_full_source_verification() {
    let temp = tempfile::tempdir().unwrap();
    let (base, fake) = common::fake_services().await;
    let (llm, seen, upstream) = provider().await;
    let mut c = config(&temp, &base, &llm);
    c.persona.style = "I tend to hide my affection behind playful teasing, little complaints, and excuses to stay close to you".into();
    seen.long_quote.store(true, Ordering::Relaxed);
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    let router = api::router(app);
    let generated = generate(&router, true).await;
    assert_eq!(generated["status"], "ready");
    assert_eq!(
        generated["profile"]["dimensions"][0]["basis"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        150
    );
    assert!(
        generated["profile"]["dimensions"][0]["basis"]
            .as_str()
            .unwrap()
            .ends_with('…')
    );
    assert_eq!(
        generated["profile"]["dimensions"][0]["evidence"][0]["quote"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        100
    );
    seen.bad.store(true, Ordering::Relaxed);
    let rejected = generate(&router, true).await;
    assert_eq!(rejected["version"], generated["version"]);
    assert_eq!(rejected["generation"]["error"], "persona_generation_failed");
    fake.abort();
    upstream.abort();
}
