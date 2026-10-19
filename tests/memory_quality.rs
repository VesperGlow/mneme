mod common;
use axum::{Json, Router, routing::post};
use mneme::{App, config::Config, domain::*, memory, store::BeginTurn};
use serde_json::{Value, json};

fn config(temp: &tempfile::TempDir, base: &str, llm: &str) -> Config {
    let mut c: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    c.database = temp.path().join("quality.db").to_str().unwrap().into();
    c.llm.base_url = llm.into();
    c.llm.model = "fixture".into();
    c.models.base_url = base.into();
    c.neo4j.base_url = base.into();
    c.api_token = "quality-only-012345678901234567890123456789".into();
    c.llm_key = "fixture".into();
    c.neo4j_password = "fixture-password".into();
    c.memory.semantic_rewrite = false;
    c.memory.verify_recall = true;
    c
}
fn fact(key: &str, text: &str) -> Candidate {
    Candidate {
        key: key.into(),
        content: text.into(),
        kind: "fact".into(),
        importance: 0.8,
        confidence: 0.99,
        entities: vec![],
        source_quote: text.into(),
        valid_until: None,
        sensitive: false,
        equivalent_to: None,
        attribute_of: None,
        relation: FactRelation::Replace,
        supersedes: vec![],
    }
}
async fn source(app: &App, text: &str) -> Message {
    let session = app.store.conversation().await.unwrap();
    let BeginTurn::Started(m) = app.store.begin_turn(&session, &id(), text).await.unwrap() else {
        panic!()
    };
    app.store.complete_turn(&m, "收到").await.unwrap();
    m
}
async fn server(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (
        base,
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() }),
    )
}
fn completion(value: Value) -> Json<Value> {
    Json(
        json!({"choices":[{"message":{"content":value.to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":42,"completion_tokens":12,"total_tokens":54,"prompt_tokens_details":{"cached_tokens":20},"completion_tokens_details":{"reasoning_tokens":4}}}),
    )
}

#[tokio::test]
async fn collision_is_adjudicated_even_when_the_old_fact_is_outside_semantic_context() {
    let (base, fake) = common::fake_services().await;
    let router=Router::new().route("/chat/completions",post(|Json(body):Json<Value>|async move {
        let input:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        if input.get("previous").is_some() {
            assert!(input["previous"].as_array().unwrap().iter().any(|m|m["content"].as_str().unwrap().contains("Haskell")));
            completion(json!({"relation":"coexist","supersedes":[],"equivalent_to":null}))
        } else {
            assert_eq!(input["existing"],json!([]),"orthogonal old vector must miss the context window");
            completion(json!({"memories":[fact("user.main_language","用户不使用 OCaml 作为主力开发语言。")] }))
        }
    }));
    let (llm, provider) = server(router).await;
    let temp = tempfile::TempDir::new().unwrap();
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config(&temp, &base, &llm), rx).await.unwrap();
    let first = source(&app, "用户主要使用 Haskell 开发后端。").await;
    let mut orthogonal = vec![0.0; 1024];
    orthogonal[0] = 1.0;
    app.store
        .apply_candidates(
            &first,
            vec![(fact("user.main_language", &first.content), orthogonal, None)],
            "none",
        )
        .await
        .unwrap();
    let second = source(&app, "用户不使用 OCaml 作为主力开发语言。").await;
    sqlx::query("INSERT INTO jobs(id,kind,payload,available_at,created_at) VALUES('collision','extract',?,?,?)").bind(&second.id).bind(now()).bind(now()).execute(&app.store.pool).await.unwrap();
    let worker = tokio::spawn(memory::worker(app.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let s = app.store.job_stats().await.unwrap();
            if s["pending"] == 0 && s["running"] == 0 {
                assert_eq!(s["dead"], 0);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let active = app.store.active_memories(10).await.unwrap();
    assert_eq!(active.len(), 2);
    assert!(active.iter().any(|m| m.content == first.content));
    let requests = app
        .store
        .turn_model_requests(&second.turn_id)
        .await
        .unwrap();
    let memory: Vec<_> = requests.iter().filter(|r| r.purpose == "memory").collect();
    assert_eq!(
        memory.len(),
        2,
        "extraction and separate collision decision must both be metered"
    );
    assert!(memory.iter().all(|r| r.usage.total_tokens == Some(54)
        && r.usage.cached_input_tokens == Some(20)
        && r.usage.reasoning_tokens == Some(4)));
    stop.send(true).unwrap();
    worker.await.unwrap();
    fake.abort();
    provider.abort();
}

#[tokio::test]
async fn unsupported_distractor_cannot_suppress_supported_fact_and_unknown_attributes_stay_empty() {
    let (base, fake) = common::fake_services().await;
    let router=Router::new()
        .route("/embed",post(|Json(body):Json<Value>|async move {let mut unit=vec![0.0;1024];unit[0]=1.0;Json(json!({"embeddings":vec![unit;body["texts"].as_array().unwrap().len()],"usage":{"prompt_tokens":100,"completion_tokens":0,"total_tokens":100,"source":"model_tokenizer"}}))}))
        .route("/rerank",post(|Json(body):Json<Value>|async move {Json(json!({"scores":body["documents"].as_array().unwrap().iter().map(|d|if d.as_str().unwrap().contains("不使用"){0.18}else{0.29}).collect::<Vec<_>>(),"usage":{"prompt_tokens":80,"completion_tokens":0,"total_tokens":80,"source":"model_tokenizer"}}))}))
        .route("/chat/completions",post(|Json(body):Json<Value>|async move {
            let input:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            let question=input["question"].as_str().unwrap();
            let ids=if question.contains("版本号") {vec![]} else if question.contains("伪造") {vec![json!("unknown-id")]} else {input["facts"].as_array().unwrap().iter().filter(|m|m["content"].as_str().unwrap().contains("不使用")).map(|m|m["id"].clone()).collect()};
            completion(json!({"ids":ids}))
        }));
    let (models, provider) = server(router).await;
    let temp = tempfile::TempDir::new().unwrap();
    let mut c = config(&temp, &base, &models);
    c.models.base_url = models;
    let (_, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    for (key, text, cosine) in [
        (
            "user.primary_constraint",
            "用户不使用 TypeScript 作为职业主力。",
            0.55_f32,
        ),
        (
            "user.hobby",
            "用户喜欢用 TypeScript 写桌面小玩具。",
            0.35_f32,
        ),
    ] {
        let m = source(&app, text).await;
        let mut vector = vec![0.0; 1024];
        vector[0] = cosine;
        vector[1] = (1.0 - cosine * cosine).sqrt();
        app.store
            .apply_candidates(&m, vec![(fact(key, text), vector, None)], "none")
            .await
            .unwrap();
    }
    let found = memory::debug_recall(&app, "TypeScript 是我的饭碗吗？")
        .await
        .unwrap();
    assert!(!found.degraded);
    assert_eq!(found.memories.len(), 1);
    assert!(found.memories[0].memory.content.contains("不使用"));
    let unknown = memory::debug_recall(&app, "我使用 TypeScript 的版本号是多少？")
        .await
        .unwrap();
    assert!(unknown.memories.is_empty());
    assert!(!unknown.degraded);
    let invalid = memory::debug_recall(&app, "TypeScript 伪造证据测试？")
        .await
        .unwrap();
    assert!(invalid.memories.is_empty());
    assert!(invalid.degraded);
    fake.abort();
    provider.abort();
}

#[tokio::test]
async fn committed_extraction_wakes_an_idle_worker_before_its_poll_timer() {
    let (base, fake) = common::fake_services().await;
    let temp = tempfile::TempDir::new().unwrap();
    let mut c = config(&temp, &base, &base);
    c.memory.reconcile_updates = false;
    c.memory.extract_every_tokens = 1;
    c.memory.poll_seconds = 30;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    let worker = tokio::spawn(memory::worker(app.clone()));
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let m = source(&app, "我住在成都").await;
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let done: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM extracted_messages WHERE message_id=?)",
            )
            .bind(&m.id)
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
            if done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("committed work should wake immediately instead of waiting 30 seconds");
    stop.send(true).unwrap();
    worker.await.unwrap();
    fake.abort();
}
