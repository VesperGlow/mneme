//! Deterministic provider replay: validates pipeline/policy contracts, not live LLM quality.
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
    domain::*,
    memory,
    store::{BeginTurn, Store},
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tower::ServiceExt;

const GAME: &str = "终末地的剧情也太 ML 了";
const MAYBE: &str = "这游戏好像还挺对我胃口，不过再看看";
const PREF: &str = "我一直喜欢探索类游戏";
const AGAIN: &str = "探索类游戏仍然是我的长期爱好";
const CHANGE: &str = "我现在不喜欢探索类游戏了";
const TOKEN: &str = "memory-layer-fixture-01234567890123456789";

fn candidate(text: &str) -> Value {
    let (key, content, kind, evidence, importance, confidence) = match text {
        // Deliberately overstate the interpretation: the server must preserve only the quote.
        GAME => (
            "user.preference.ml",
            "用户长期喜欢 ML 剧情",
            "event",
            "user_quote",
            0.01,
            0.3,
        ),
        MAYBE => (
            "user.preference.game",
            "用户喜欢这款游戏",
            "preference",
            "explicit_fact",
            0.01,
            0.99,
        ),
        PREF | AGAIN => (
            "user.preference.exploration",
            "用户长期喜欢探索类游戏。",
            "preference",
            "explicit_fact",
            0.8,
            0.98,
        ),
        CHANGE => (
            "user.preference.exploration",
            "用户现在不喜欢探索类游戏。",
            "preference",
            "explicit_fact",
            0.8,
            0.98,
        ),
        "昨晚又玩到两点" => (
            "user.preference.gaming",
            "用户可能喜欢夜间玩游戏。",
            "preference",
            "inference",
            0.2,
            0.95,
        ),
        "今天下班后看了日落，心情好了很多" => (
            "user.emotion.sunset",
            "用户今天看日落后心情变好。",
            "emotion",
            "explicit_fact",
            0.01,
            0.9,
        ),
        "嗯" => (
            "user.event.ack",
            "用户答应了",
            "event",
            "user_quote",
            0.9,
            0.99,
        ),
        _ => return Value::Null,
    };
    json!({"key":key,"content":content,"kind":kind,"tier":"long_term","evidence_type":evidence,
        "importance":importance,"confidence":confidence,"entities":["游戏"],"source_quote":text,
        "valid_until":null,"sensitive":false,"equivalent_to":null,"attribute_of":null,"relation":"replace","supersedes":[]})
}

async fn provider(fail_once: Arc<AtomicBool>) -> (String, tokio::task::JoinHandle<()>) {
    let router = Router::new().route("/chat/completions", post(move |Json(body): Json<Value>| {
        let fail_once = fail_once.clone();
        async move {
            if body["stream"] == true {
                let wire = format!("data: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"delta":{"content":"嗯，具体说说？"},"finish_reason":"stop"}]}));
                return ([("content-type", "text/event-stream")], wire).into_response();
            }
            if fail_once.swap(false, Ordering::SeqCst) {
                return (StatusCode::SERVICE_UNAVAILABLE, "temporary fixture failure").into_response();
            }
            let prompt = body["messages"][0]["content"].as_str().unwrap();
            assert!(prompt.contains(GAME) && prompt.contains("不按内容是否重要"));
            if let Some(schema) = body.get("response_format") {
                let fields = &schema["json_schema"]["schema"]["properties"]["memories"]["items"]["properties"];
                assert!(fields.get("tier").is_some() && fields.get("evidence_type").is_some());
            }
            let input: Value = serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            let batch = input["targets"].as_array();
            let targets = batch.cloned().unwrap_or_else(|| vec![input["target"].clone()]);
            let mut memories = Vec::new();
            for target in &targets {
                let mut item = candidate(target["content"].as_str().unwrap());
                if item.is_null() { continue; }
                if batch.is_some() { item["source_id"] = target["id"].clone(); }
                if target["content"] == GAME { memories.push(item.clone()); }
                memories.push(item);
            }
            let mut result = json!({"memories":memories});
            if batch.is_some() { result["processed_source_ids"] = json!(targets.iter().map(|t| &t["id"]).collect::<Vec<_>>()); }
            Json(json!({"choices":[{"message":{"content":result.to_string()},"finish_reason":"stop"}]})).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (base, task)
}

async fn selective_models() -> (String, tokio::task::JoinHandle<()>) {
    let router = Router::new()
        .route(
            "/embed",
            post(|Json(body): Json<Value>| async move {
                let vectors: Vec<_> = body["texts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|text| {
                        let mut vector = vec![0.0; 1024];
                        vector[usize::from(!text.as_str().unwrap().contains("终末地"))] = 1.0;
                        vector
                    })
                    .collect();
                Json(json!({"embeddings":vectors}))
            }),
        )
        .route(
            "/rerank",
            post(|Json(body): Json<Value>| async move {
                let scores: Vec<_> = body["documents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|d| {
                        if d.as_str().unwrap().contains("终末地") {
                            0.95
                        } else {
                            0.01
                        }
                    })
                    .collect();
                Json(json!({"scores":scores}))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (base, task)
}

fn config(temp: &tempfile::TempDir, llm: &str, models: &str) -> Config {
    let mut c: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    c.database = temp.path().join("db").to_string_lossy().into();
    c.llm.base_url = llm.into();
    c.llm.model = "memory-layers".into();
    c.llm_key = "fixture".into();
    c.llm.json_schema = true;
    c.models.base_url = models.into();
    c.models.timeout_seconds = 1;
    c.neo4j.base_url = models.into();
    c.neo4j_password = "fixture".into();
    c.api_token = TOKEN.into();
    c.memory.reconcile_updates = false;
    c.memory.extract_every_tokens = 100_000;
    c.memory.extract_max_delay_seconds = 120;
    c.memory.poll_seconds = 1;
    c.memory.max_attempts = 2;
    c
}
async fn source(store: &Store, text: &str) -> Message {
    let session = store.conversation().await.unwrap();
    let BeginTurn::Started(source) = store.begin_turn(&session, &id(), text).await.unwrap() else {
        panic!()
    };
    store.complete_turn(&source, "我听到了。").await.unwrap();
    source
}
async fn enqueue(app: &App, source: &Message) {
    sqlx::query(
        "INSERT INTO jobs(id,kind,payload,available_at,created_at) VALUES(?,'extract',?,?,?)",
    )
    .bind(format!("fixture:{}", source.id))
    .bind(&source.id)
    .bind(now())
    .bind(now())
    .execute(&app.store.pool)
    .await
    .unwrap();
    app.store.retry_dead().await.unwrap();
}
async fn idle(app: &App) {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let stats = app.store.job_stats().await.unwrap();
            assert_eq!(
                stats["dead"],
                0,
                "{}",
                app.store.extraction_diagnostics().await.unwrap()
            );
            let running: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM extraction_runs WHERE status='running'")
                    .fetch_one(&app.store.pool)
                    .await
                    .unwrap();
            if stats["pending"] == 0 && stats["running"] == 0 && running == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}
async fn extract(app: &App, text: &str) -> Message {
    let s = source(&app.store, text).await;
    enqueue(app, &s).await;
    idle(app).await;
    s
}
async fn get(router: &Router, path: &str) -> Value {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("authorization", format!("Bearer {TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&to_bytes(response.into_body(), 2_097_152).await.unwrap()).unwrap()
}

#[tokio::test]
async fn daily_chat_uncertainty_repetition_conflict_and_low_value_are_traceable() {
    let temp = tempfile::tempdir().unwrap();
    let (models, fake) = common::fake_services().await;
    let (llm, provider) = provider(Arc::new(AtomicBool::new(false))).await;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let (selective, reranker) = selective_models().await;
    let mut c = config(&temp, &llm, &models);
    c.models.base_url = selective;
    let app = App::new(c, rx).await.unwrap();
    let worker = tokio::spawn(memory::worker(app.clone()));
    let original = extract(&app, GAME).await;
    let memories = app.store.active_memories(20).await.unwrap();
    assert_eq!(memories.len(), 1);
    let game = &memories[0];
    assert_eq!(game.tier, "episodic");
    assert_eq!(game.evidence_type, "user_quote");
    assert_eq!(game.content, format!("用户表示：“{GAME}”"));
    assert_eq!(game.valid_until, Some(original.created_at + 30 * 86400));
    assert!(game.strength(now(), 180.0) > app.config.memory.forget_below);
    assert_eq!(game.indexed_revision, game.revision);
    assert_eq!(
        serde_json::from_str::<Vec<f32>>(&game.embedding)
            .unwrap()
            .len(),
        1024
    );
    let game_id = game.id.clone();
    extract(&app, GAME).await;
    assert_eq!(
        app.store.active_memories(20).await.unwrap().len(),
        1,
        "literal duplicates merge"
    );
    assert_eq!(app.store.sources(&game_id).await.unwrap().len(), 2);
    assert_eq!(
        app.store.memory(&game_id).await.unwrap().unwrap().tier,
        "episodic",
        "topic repetition is not a stable preference"
    );
    extract(&app, MAYBE).await;
    let m = app
        .store
        .active_memories(20)
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.content.contains(MAYBE))
        .unwrap();
    assert_eq!(m.evidence_type, "user_quote");
    assert_eq!(m.tier, "episodic");
    extract(&app, "今天下班后看了日落，心情好了很多").await;
    assert!(
        app.store
            .active_memories(20)
            .await
            .unwrap()
            .iter()
            .any(|m| m.kind == "emotion" && m.importance == 0.01)
    );
    extract(&app, "昨晚又玩到两点").await;
    let m = app
        .store
        .active_memories(20)
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.evidence_type == "inference")
        .unwrap();
    assert!(m.content.starts_with("模型推测（未验证）："));
    assert_eq!(m.confidence, 0.6);
    assert_eq!(m.tier, "episodic");
    let first = extract(&app, PREF).await;
    let preference = app
        .store
        .active_memories(20)
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.slot == "user.preference.exploration")
        .unwrap();
    assert_eq!(preference.tier, "episodic");
    // Replaying a single source cannot fabricate independent confirmation.
    enqueue(&app, &first).await;
    idle(&app).await;
    assert_eq!(
        app.store
            .memory(&preference.id)
            .await
            .unwrap()
            .unwrap()
            .tier,
        "episodic"
    );
    extract(&app, AGAIN).await;
    let confirmed = app.store.memory(&preference.id).await.unwrap().unwrap();
    assert_eq!(confirmed.tier, "long_term");
    assert!(confirmed.valid_until.is_none());
    extract(&app, PREF).await;
    assert!(
        app.store
            .memory(&preference.id)
            .await
            .unwrap()
            .unwrap()
            .valid_until
            .is_none(),
        "later confirmations do not restore episode TTL"
    );
    let sources = app.store.sources(&preference.id).await.unwrap();
    assert_eq!(sources.len(), 3);
    assert!(
        sources
            .iter()
            .all(|s| s["evidence_type"] == "explicit_fact" && s["confidence"] == 0.98)
    );
    let versions = app.store.memory_versions(&preference.id).await.unwrap();
    assert_eq!(versions[0]["snapshot"]["tier"], "long_term");
    extract(&app, CHANGE).await;
    assert_eq!(
        app.store
            .memory(&preference.id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "superseded"
    );
    let changed = app
        .store
        .active_memories(20)
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.slot == "user.preference.exploration")
        .unwrap();
    assert_eq!(changed.tier, "episodic");
    assert_eq!(app.store.sources(&changed.id).await.unwrap().len(), 1);
    let count = app.store.active_memories(20).await.unwrap().len();
    extract(&app, "嗯").await;
    extract(&app, "随便查一下天气").await;
    assert_eq!(app.store.active_memories(20).await.unwrap().len(), count);
    let router = api::router(app.clone());
    let diagnostics = get(&router, "/v1/console/extraction").await;
    assert!(
        diagnostics["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["reason"] == "low_value_acknowledgement")
    );
    assert!(
        diagnostics["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["reason"] == "preference_confirmed")
    );
    assert!(
        diagnostics["runs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["status"] == "complete")
    );
    // Existing hybrid retrieval + strict reranking remains the admission point to chat context.
    let recall = memory::debug_recall(&app, GAME).await.unwrap();
    assert!(recall.memories.iter().any(|r| r.memory.id == game_id));
    assert!(!recall.memories.iter().any(|r| r.memory.id == preference.id));
    assert_eq!(
        recall.memories.len(),
        1,
        "irrelevant stored episodes are excluded by reranking"
    );
    assert!(recall.trace.unwrap().candidates.iter().any(|c| {
        c["selected"] == false
            && c["reason"]
                .as_str()
                .is_some_and(|r| r.starts_with("below_"))
    }));
    reranker.abort();
    stop.send(true).unwrap();
    worker.await.unwrap();
    fake.abort();
    provider.abort();
}

#[tokio::test]
async fn short_chat_idle_flushes_without_another_message_and_dependency_failures_do_not_block_storage()
 {
    let temp = tempfile::tempdir().unwrap();
    let (models, fake) = common::fake_services().await;
    let (llm, provider) = provider(Arc::new(AtomicBool::new(false))).await;
    let mut c = config(&temp, &llm, &models);
    c.memory.extract_max_delay_seconds = 1;
    // CPU and graph are unavailable. A literal observation can still commit.
    c.models.base_url = "http://127.0.0.1:1".into();
    c.neo4j.base_url = "http://127.0.0.1:1".into();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    let router = api::router(app.clone());
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat")
                .header("authorization", format!("Bearer {TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"request_id":id(),"message":GAME}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let wire = String::from_utf8(
        to_bytes(response.into_body(), 2_097_152)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(wire.contains("event: done") && wire.contains("\"persisted\":true"));
    assert_eq!(app.store.job_stats().await.unwrap()["pending"], 0);
    let worker = tokio::spawn(memory::worker(app.clone()));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if app.store.active_memories(10).await.unwrap().len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap();
    // A second literal episode still succeeds with existing memory and no CPU context embedding.
    let second = source(&app.store, MAYBE).await;
    enqueue(&app, &second).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if app.store.active_memories(10).await.unwrap().len() >= 2
                && app.store.extraction_pending_tokens().await.unwrap() == 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(app.store.extraction_pending_tokens().await.unwrap(), 0);
    let m = app.store.active_memories(10).await.unwrap().remove(0);
    assert!(m.indexed_revision < m.revision);
    assert!(
        app.store
            .message(&second.id)
            .await
            .unwrap()
            .content
            .contains(MAYBE)
    );
    stop.send(true).unwrap();
    worker.await.unwrap();
    fake.abort();
    provider.abort();
}

#[tokio::test]
async fn token_trigger_provider_retry_restart_and_index_projection_are_durable() {
    let temp = tempfile::tempdir().unwrap();
    let (models, fake) = common::fake_services().await;
    let (llm, provider) = provider(Arc::new(AtomicBool::new(true))).await;
    let mut c = config(&temp, &llm, &models);
    c.memory.extract_every_tokens = 1;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c.clone(), rx.clone()).await.unwrap();
    let s = source(&app.store, GAME).await;
    assert_eq!(app.store.job_stats().await.unwrap()["pending"], 1);
    assert_eq!(app.store.extraction_pending_tokens().await.unwrap(), 0);
    let worker = tokio::spawn(memory::worker(app.clone()));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if app.store.extraction_diagnostics().await.unwrap()["runs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["status"] == "pending")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    worker.await.unwrap();
    drop(app);
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    let worker = tokio::spawn(memory::worker(app.clone()));
    idle(&app).await;
    let memories = app.store.active_memories(10).await.unwrap();
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].indexed_revision, memories[0].revision);
    assert_eq!(
        app.store.sources(&memories[0].id).await.unwrap()[0]["message_id"],
        s.id
    );
    let diagnostics = app.store.extraction_diagnostics().await.unwrap();
    assert_eq!(diagnostics["runs"][0]["attempt"], 2);
    assert_eq!(diagnostics["runs"][0]["stored_count"], 1);
    assert_eq!(diagnostics["runs"][0]["status"], "complete");
    assert_eq!(diagnostics["runs"][1]["status"], "pending");
    stop.send(true).unwrap();
    worker.await.unwrap();
    fake.abort();
    provider.abort();
}

#[tokio::test]
async fn consolidation_retention_and_diagnostic_history_are_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("db").to_str().unwrap())
        .await
        .unwrap();
    let s = source(&store, "今天看了日落。也玩了终末地。还去散步了。心情不错。").await;
    let observations: Vec<_> = ["今天看了日落", "也玩了终末地", "还去散步了", "心情不错"]
        .into_iter()
        .map(|quote| {
            let mut c: Candidate = serde_json::from_value(candidate(GAME)).unwrap();
            c.source_quote = quote.into();
            (c, vec![], None)
        })
        .collect();
    store
        .apply_candidates(&s, observations, "multiple-observations")
        .await
        .unwrap();
    assert_eq!(
        store.active_memories(10).await.unwrap().len(),
        4,
        "preserve each independent observation in one source"
    );
    for i in 0..260 {
        let text = format!("今天散步看见了第 {i} 种花");
        let mut c: Candidate = serde_json::from_value(candidate(GAME)).unwrap();
        c.content = text.clone();
        c.source_quote = text;
        c.evidence_type = "user_quote".into();
        // Direct store writes use real persisted evidence, like the worker does.
        let source = source(&store, &c.source_quote).await;
        store
            .apply_candidates(&source, vec![(c, vec![], None)], "retention")
            .await
            .unwrap();
    }
    assert_eq!(store.active_memories(300).await.unwrap().len(), 256);
    assert_eq!(
        store.list_memories(300, 0).await.unwrap().len(),
        264,
        "raw/audit history is retained"
    );
    let result = store.consolidate_memories().await.unwrap();
    assert!(result["at"].as_i64().unwrap() > 0);
    sqlx::query("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1010) INSERT INTO extraction_events(job_id,message_id,decision,reason) SELECT 'bound',?,'rejected','no_candidates' FROM n")
        .bind(s.id).execute(&store.pool).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM extraction_events")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(count, 1000);
    assert_eq!(
        store.extraction_diagnostics().await.unwrap()["events"]
            .as_array()
            .unwrap()
            .len(),
        100
    );
    // Expiry drives durable graph deletion, rather than leaving stale vector hits.
    let m = store.active_memories(1).await.unwrap().remove(0);
    sqlx::query("UPDATE memories SET valid_until=? WHERE id=?")
        .bind(now() - 1)
        .bind(&m.id)
        .execute(&store.pool)
        .await
        .unwrap();
    assert_eq!(store.expire(180.0, 0.12).await.unwrap(), 1);
    assert_eq!(
        store.memory(&m.id).await.unwrap().unwrap().status,
        "forgotten"
    );
}

#[tokio::test]
async fn periodic_consolidation_requires_distinct_explicit_sources_and_keeps_declared_deadlines() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path().join("db").to_str().unwrap())
        .await
        .unwrap();
    let deadline = now() + 7 * 86400;
    let mut c: Candidate = serde_json::from_value(candidate(PREF)).unwrap();
    c.valid_until = Some(
        chrono::DateTime::from_timestamp(deadline, 0)
            .unwrap()
            .to_rfc3339(),
    );
    let first = source(&store, PREF).await;
    store
        .apply_candidates(&first, vec![(c.clone(), vec![], None)], "first")
        .await
        .unwrap();
    assert_eq!(store.consolidate_memories().await.unwrap()["promoted"], 0);
    let second = source(&store, PREF).await;
    store
        .apply_candidates(&second, vec![(c, vec![], None)], "second")
        .await
        .unwrap();
    let memory = store.active_memories(10).await.unwrap().remove(0);
    // Simulate an older episodic record whose second confirmation predates deployment.
    sqlx::query("UPDATE memories SET tier='episodic' WHERE id=?")
        .bind(&memory.id)
        .execute(&store.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM extraction_events")
        .execute(&store.pool)
        .await
        .unwrap();
    let result = store.consolidate_memories().await.unwrap();
    assert_eq!(result["promoted"], 1);
    let promoted = store.memory(&memory.id).await.unwrap().unwrap();
    assert_eq!(promoted.tier, "long_term");
    assert_eq!(promoted.valid_until, Some(deadline));
    assert!(promoted.indexed_revision < promoted.revision);
    assert_eq!(
        store.consolidate_memories().await.unwrap()["promoted"],
        0,
        "maintenance is idempotent"
    );
}

#[tokio::test]
async fn dead_letter_retry_preserves_the_failed_run_and_counts_the_new_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let (models, fake) = common::fake_services().await;
    let (llm, provider) = provider(Arc::new(AtomicBool::new(true))).await;
    let mut c = config(&temp, &llm, &models);
    c.memory.max_attempts = 1;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    let s = source(&app.store, GAME).await;
    enqueue(&app, &s).await;
    let worker = tokio::spawn(memory::worker(app.clone()));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let data = app.store.extraction_diagnostics().await.unwrap();
            if data["runs"]
                .as_array()
                .is_some_and(|r| r.iter().any(|r| r["status"] == "dead"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(app.store.retry_dead().await.unwrap(), 1);
    idle(&app).await;
    let data = app.store.extraction_diagnostics().await.unwrap();
    let runs = data["runs"].as_array().unwrap();
    assert_eq!(
        runs.len(),
        2,
        "a dead-letter retry must append a new audit run"
    );
    assert_eq!(runs[0]["status"], "complete");
    assert_eq!(runs[0]["stored_count"], 1);
    assert_eq!(
        runs[1]["status"], "dead",
        "the earlier failure is immutable"
    );
    assert_eq!(app.store.active_memories(10).await.unwrap().len(), 1);
    stop.send(true).unwrap();
    worker.await.unwrap();
    fake.abort();
    provider.abort();
}
