mod common;
use axum::{Json, Router, http::StatusCode, routing::post};
use mneme::{
    App,
    config::Config,
    domain::*,
    memory,
    models::Models,
    store::{BeginTurn, Store},
};
use serde_json::{Value, json};

fn config(temp: &tempfile::TempDir, base: &str) -> Config {
    let mut c: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    c.database = temp.path().join("eval.db").to_str().unwrap().into();
    c.llm.model = "fixture".into();
    c.llm.base_url = base.into();
    c.models.base_url = base.into();
    c.neo4j.base_url = base.into();
    c.api_token = "memory-evaluation-only-01234567890123456789".into();
    c.llm_key = "fixture".into();
    c.neo4j_password = "fixture-password".into();
    c
}
fn fact(key: &str, text: &str) -> Candidate {
    Candidate {
        key: key.into(),
        content: text.into(),
        kind: "fact".into(),
        importance: 0.8,
        confidence: 0.95,
        entities: vec![],
        source_quote: text.into(),
        valid_until: None,
        sensitive: false,
        equivalent_to: None,
    }
}

#[tokio::test]
async fn relative_reranking_keeps_multiple_strong_matches_and_excludes_the_weak_tail() {
    let (base, graph) = common::fake_services().await;
    let server = Router::new()
        .route(
            "/embed",
            post(|Json(body): Json<Value>| async move {
                let mut unit = vec![0.0; 1024];
                unit[0] = 1.0;
                Json(json!({"embeddings":vec![unit;body["texts"].as_array().unwrap().len()]}))
            }),
        )
        .route(
            "/rerank",
            post(|Json(body): Json<Value>| async move {
                let scores: Vec<_> = body["documents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|d| match d.as_str().unwrap() {
                        "用户住在珠海。" => 0.9,
                        "用户住处附近工作。" => 0.85,
                        _ => 0.6,
                    })
                    .collect();
                Json(json!({"scores":scores}))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let models_url = format!("http://{}", listener.local_addr().unwrap());
    let models = tokio::spawn(async move { axum::serve(listener, server).await.unwrap() });
    let temp = tempfile::TempDir::new().unwrap();
    let mut c = config(&temp, &base);
    c.models.base_url = models_url;
    let (_, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c.clone(), rx).await.unwrap();
    let mut unit = vec![0.0; 1024];
    unit[0] = 1.0;
    for (slot, text) in [
        ("user.residence", "用户住在珠海。"),
        ("user.work", "用户住处附近工作。"),
        ("user.pet", "用户有一只猫。相似主题的弱候选。"),
    ] {
        let source = source(&app.store, text).await;
        let mut vector = unit.clone();
        if slot == "user.pet" {
            vector[0] = 0.5;
            vector[1] = 0.75_f32.sqrt();
        }
        app.store
            .apply_candidates(&source, vec![(fact(slot, text), vector, None)], "none")
            .await
            .unwrap();
    }
    let result = memory::debug_recall(&app, "我住在哪里，在哪里工作？")
        .await
        .unwrap();
    assert!(!result.degraded);
    assert_eq!(result.memories.len(), 2);
    assert!(result.memories.iter().all(|m| m.memory.slot != "user.pet"));
    assert!(
        result
            .trace
            .unwrap()
            .candidates
            .iter()
            .any(|v| v["reason"] == "below_relative_rerank_threshold")
    );
    c.memory.rerank_relative_min = 0.0;
    drop(app);
    let (_, rx) = tokio::sync::watch::channel(false);
    let legacy = App::new(c, rx).await.unwrap();
    assert_eq!(
        memory::recall(&legacy, "我住在哪里，在哪里工作？")
            .await
            .unwrap()
            .memories
            .len(),
        3
    );
    graph.abort();
    models.abort();
}
async fn source(store: &Store, text: &str) -> Message {
    let session = store.conversation().await.unwrap();
    let BeginTurn::Started(m) = store.begin_turn(&session, &id(), text).await.unwrap() else {
        panic!()
    };
    store.complete_turn(&m, "收到").await.unwrap();
    m
}
async fn apply(store: &Store, m: &Message, key: &str, text: &str) {
    store
        .apply_candidates(m, vec![(fact(key, text), vec![1.0], None)], &id())
        .await
        .unwrap();
}

#[tokio::test]
async fn updating_a_slot_to_an_existing_fact_retires_its_old_value() {
    let temp = tempfile::TempDir::new().unwrap();
    let store = Store::open(temp.path().join("db").to_str().unwrap())
        .await
        .unwrap();
    let a = source(&store, "用户喜欢茶。").await;
    apply(&store, &a, "user.preference.drink", &a.content).await;
    let b = source(&store, "用户喜欢咖啡。").await;
    apply(&store, &b, "user.preference.coffee", &b.content).await;
    let c = source(&store, "用户喜欢咖啡。").await;
    apply(&store, &c, "user.preference.drink", &c.content).await;
    let active = store.active_memories(100).await.unwrap();
    assert_eq!(
        active.len(),
        1,
        "the old tea preference must be superseded even when the new content is deduplicated"
    );
    assert_eq!(active[0].content, "用户喜欢咖啡。");
}

#[test]
fn assistant_output_cannot_be_user_evidence() {
    let m = Message {
        id: id(),
        session_id: id(),
        turn_id: id(),
        role: "assistant".into(),
        content: "用户住在成都。".into(),
        created_at: now(),
    };
    assert!(memory::validate_candidate(&fact("user.residence", &m.content), &m).is_err());
}

#[tokio::test]
async fn embedding_batches_respect_the_service_limit_and_deduplicate() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let router = Router::new().route(
        "/embed",
        post(move |Json(v): Json<Value>| {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            async move {
                let n = v["texts"].as_array().unwrap().len();
                if n > 8 {
                    return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({})));
                }
                let mut vector = vec![0.0; 1024];
                vector[0] = 1.0;
                (StatusCode::OK, Json(json!({"embeddings":vec![vector;n]})))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let temp = tempfile::TempDir::new().unwrap();
    let models = Models::new(&config(&temp, &base)).unwrap();
    let docs: Vec<_> = (0..32).map(|i| format!("用户事实{}", i % 17)).collect();
    let result = models.embed(&docs, false).await.unwrap();
    assert_eq!(result.len(), 32);
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 3);
    assert!(models.embed(&[], false).await.unwrap().is_empty());
    task.abort();
}

#[tokio::test]
async fn model_requests_do_not_reuse_a_blackholed_keepalive_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let mut unit = vec![0.0; 1024];
    unit[0] = 1.0;
    let response = json!({"embeddings":[unit]}).to_string();
    let server = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let response = response.clone();
            connections.spawn(async move {
                let mut bytes=Vec::new();let mut chunk=[0;4096];
                loop {
                    let n=socket.read(&mut chunk).await.unwrap();
                    if n==0 {return}
                    bytes.extend_from_slice(&chunk[..n]);
                    if let Some(end)=bytes.windows(4).position(|w|w==b"\r\n\r\n") {
                        let headers=String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                        let length:usize=headers.lines().find_map(|l|l.strip_prefix("content-length:")).unwrap().trim().parse().unwrap();
                        if bytes.len()>=end+4+length {break}
                    }
                }
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{}",response.len(),response).as_bytes()).await.unwrap();
                // Model/container proxy has lost the upstream after this reply:
                // TCP stays open, but subsequent requests cannot be answered.
                std::future::pending::<()>().await;
            });
        }
    });
    let temp = tempfile::TempDir::new().unwrap();
    let mut c = config(&temp, &base);
    c.models.timeout_seconds = 1;
    let models = Models::new(&c).unwrap();
    assert_eq!(
        models
            .embed(&["第一条独立内容".into()], false)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        models
            .embed(&["另一条全新内容".into()], false)
            .await
            .unwrap()
            .len(),
        1
    );
    server.abort();
}

#[tokio::test]
async fn partial_batch_retries_only_sources_with_no_valid_fact_instead_of_receipting_their_loss() {
    let (base, services) = common::fake_services().await;
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = calls.clone();
    let router=Router::new().route("/chat/completions",post(move|Json(body):Json<Value>|{
        count.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
        async move {
            let input:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            let item=|key:&str,text:&str,source:&Value|json!({"key":key,"content":text,"kind":"fact","importance":0.8,"confidence":0.99,"entities":[],"source_quote":source["content"],"source_id":source["id"],"valid_until":null,"sensitive":false,"equivalent_to":null});
            let facts=if let Some(targets)=input["targets"].as_array() {
                vec![item("user.residence","用户住在北海。",&targets[0]),item("user.爱好","用户长期喜欢陶艺。",&targets[1])]
            }else {
                assert!(input["target"]["content"].as_str().unwrap().contains("陶艺"),"valid source must not incur another model call");
                let mut value=item("user.hobby.ceramics","用户长期喜欢陶艺。",&input["target"]);
                value.as_object_mut().unwrap().remove("source_id");vec![value]
            };
            Json(json!({"choices":[{"finish_reason":"stop","message":{"content":json!({"memories":facts}).to_string()}}]}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let llm_url = format!("http://{}", listener.local_addr().unwrap());
    let llm = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let temp = tempfile::TempDir::new().unwrap();
    let mut c = config(&temp, &base);
    c.llm.base_url = llm_url;
    c.memory.poll_seconds = 1;
    c.memory.max_attempts = 1;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    let a = source(&app.store, "我住在北海。").await;
    let b = source(&app.store, "我长期喜欢陶艺。").await;
    sqlx::query("INSERT INTO jobs(id,kind,payload,available_at,created_at) VALUES('partial','extract',?,?,?)")
        .bind(json!({"message_ids":[a.id,b.id]}).to_string()).bind(now()).bind(now()).execute(&app.store.pool).await.unwrap();
    let worker = tokio::spawn(memory::worker(app.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let stats = app.store.job_stats().await.unwrap();
            assert_eq!(stats["dead"], 0);
            if stats["pending"] == 0 && stats["running"] == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
    assert_eq!(app.store.active_memories(10).await.unwrap().len(), 2);
    let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM extracted_messages")
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
    assert_eq!(receipts, 2);
    stop.send(true).unwrap();
    worker.await.unwrap();
    services.abort();
    llm.abort();
}

#[tokio::test]
async fn reranker_outage_does_not_inject_unrelated_memories() {
    let (base, task) = common::fake_services().await;
    let temp = tempfile::TempDir::new().unwrap();
    let mut c = config(&temp, &base);
    // /rerank returns an invalid model response; embeddings and graph still work.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    c.models.base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/rerank",
                post(|| async { StatusCode::SERVICE_UNAVAILABLE }),
            ),
        )
        .await
        .unwrap()
    });
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    let m = source(&app.store, "用户住在成都。").await;
    apply(&app.store, &m, "user.residence", &m.content).await;
    let stored = app.store.active_memories(10).await.unwrap().pop().unwrap();
    app.graph.project(&stored).await.unwrap();
    let result = memory::debug_recall(&app, "量子纠缠的原理是什么？")
        .await
        .unwrap();
    assert!(result.degraded);
    assert!(
        result.memories.is_empty(),
        "RRF ranks candidates but cannot establish relevance"
    );
    server.abort();
    task.abort();
}

#[tokio::test]
async fn deletion_blocks_preexisting_unprocessed_sources_with_changed_slots() {
    let temp = tempfile::TempDir::new().unwrap();
    let store = Store::open(temp.path().join("db").to_str().unwrap())
        .await
        .unwrap();
    let a = source(&store, "用户喜欢咖啡。").await;
    apply(&store, &a, "user.preference.drink", &a.content).await;
    // This old message has not been extracted yet when the user forgets the fact.
    let b = source(&store, "用户喜欢咖啡。").await;
    let m = store.active_memories(10).await.unwrap().pop().unwrap();
    store.forget(&m.id).await.unwrap();
    apply(&store, &b, "user.preference.coffee", &b.content).await;
    assert!(store.active_memories(10).await.unwrap().is_empty());
    let c = source(&store, "用户喜欢咖啡。").await;
    apply(&store, &c, "user.preference.coffee", &c.content).await;
    assert_eq!(store.active_memories(10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn ninety_days_of_updates_retries_and_concurrent_delivery_keep_latest_truth() {
    let temp = tempfile::TempDir::new().unwrap();
    let store = Store::open(temp.path().join("db").to_str().unwrap())
        .await
        .unwrap();
    let mut observations = Vec::new();
    for day in 0..90 {
        let mut m = source(&store, &format!("用户当前阅读计划为第{day}章。")).await;
        m.created_at = now() - (89 - day) * 86400;
        sqlx::query("UPDATE messages SET created_at=? WHERE id=?")
            .bind(m.created_at)
            .bind(&m.id)
            .execute(&store.pool)
            .await
            .unwrap();
        observations.push(m);
    }
    use futures_util::{StreamExt, stream};
    // Reverse delivery and duplicates exercise observation ordering, not wall-clock arrival.
    stream::iter(
        observations
            .iter()
            .rev()
            .chain(observations.iter())
            .cloned(),
    )
    .for_each_concurrent(50, |m| {
        let store = store.clone();
        async move { apply(&store, &m, "user.plan.reading", &m.content).await }
    })
    .await;
    let active = store.active_memories(100).await.unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].content, observations.last().unwrap().content);
    let seq: i64 = sqlx::query_scalar("SELECT MAX(seq) FROM messages WHERE role='user'")
        .fetch_one(&store.pool)
        .await
        .unwrap();
    assert_eq!(active[0].source_seq, seq);
    let sources = store.sources(&active[0].id).await.unwrap();
    assert_eq!(sources.len(), 1);
}

#[tokio::test]
async fn vector_cache_observes_embedding_replacement_without_a_revision_change() {
    let (base, task) = common::fake_services().await;
    let temp = tempfile::TempDir::new().unwrap();
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config(&temp, &base), rx).await.unwrap();
    let m = source(&app.store, "用户住在杭州。").await;
    let mut vector = vec![0.0; 1024];
    vector[0] = 1.0;
    app.store
        .apply_candidates(
            &m,
            vec![(fact("user.residence", &m.content), vector, None)],
            "seed",
        )
        .await
        .unwrap();
    let stored = app.store.active_memories(1).await.unwrap().pop().unwrap();
    app.graph.project(&stored).await.unwrap();
    assert_eq!(
        memory::recall(&app, "我住哪里？")
            .await
            .unwrap()
            .memories
            .len(),
        1
    );
    let mut replacement = vec![0.0; 1024];
    replacement[1] = 1.0;
    assert!(
        app.store
            .save_embedding(&stored.id, stored.revision, &replacement)
            .await
            .unwrap()
    );
    assert!(
        memory::recall(&app, "我住哪里？")
            .await
            .unwrap()
            .memories
            .is_empty()
    );
    task.abort();
}

#[tokio::test]
async fn retrieval_deadline_includes_waiting_for_sqlite_connections() {
    let (base, task) = common::fake_services().await;
    let temp = tempfile::TempDir::new().unwrap();
    let mut c = config(&temp, &base);
    c.memory.retrieval_timeout_seconds = 1;
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(app.store.pool.acquire().await.unwrap());
    }
    let at = std::time::Instant::now();
    let result = memory::debug_recall(&app, "我住哪里？").await.unwrap();
    assert!(result.degraded && result.memories.is_empty());
    assert_eq!(result.trace.unwrap().reranker_state, "deadline_exceeded");
    assert!(at.elapsed() < std::time::Duration::from_millis(1500));
    drop(held);
    task.abort();
}

#[tokio::test]
async fn separate_personal_instances_never_return_each_others_content() {
    let (base, task) = common::fake_services().await;
    let first = tempfile::TempDir::new().unwrap();
    let second = tempfile::TempDir::new().unwrap();
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let a = App::new(config(&first, &base), rx.clone()).await.unwrap();
    let b = App::new(config(&second, &base), rx).await.unwrap();
    for (app, text) in [(&a, "甲用户住在杭州。"), (&b, "乙用户住在上海。")] {
        let m = source(&app.store, text).await;
        let mut vector = vec![0.0; 1024];
        vector[0] = 1.0;
        app.store
            .apply_candidates(
                &m,
                vec![(fact("user.residence", text), vector, None)],
                "seed",
            )
            .await
            .unwrap();
        app.graph
            .project(&app.store.active_memories(1).await.unwrap()[0])
            .await
            .unwrap();
    }
    for (app, own, other) in [(&a, "甲用户", "乙用户"), (&b, "乙用户", "甲用户")] {
        let r = memory::debug_recall(app, "我住哪里？").await.unwrap();
        assert_eq!(r.memories.len(), 1);
        assert!(r.memories[0].memory.content.starts_with(own));
        assert!(!serde_json::to_string(&r.trace).unwrap().contains(other));
        let other_app = if own == "甲用户" { &b } else { &a };
        let foreign_id = &other_app.store.active_memories(1).await.unwrap()[0].id;
        assert!(
            !serde_json::to_string(&r.trace)
                .unwrap()
                .contains(foreign_id)
        );
    }
    task.abort();
}

#[tokio::test]
async fn identical_model_requests_share_inflight_work_and_content_changes_miss_cache() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let embedded = Arc::new(AtomicUsize::new(0));
    let reranked = Arc::new(AtomicUsize::new(0));
    let (emb, rank) = (embedded.clone(), reranked.clone());
    let router = Router::new()
        .route(
            "/embed",
            post(move |Json(body): Json<Value>| {
                emb.fetch_add(1, Ordering::Relaxed);
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                    let mut v = vec![0.0; 1024];
                    v[0] = 1.0;
                    Json(json!({"embeddings":vec![v;body["texts"].as_array().unwrap().len()]}))
                }
            }),
        )
        .route(
            "/rerank",
            post(move |Json(body): Json<Value>| {
                rank.fetch_add(1, Ordering::Relaxed);
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                    Json(json!({"scores":vec![0.8;body["documents"].as_array().unwrap().len()]}))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let temp = tempfile::TempDir::new().unwrap();
    let models = Models::new(&config(&temp, &base)).unwrap();
    use futures_util::{StreamExt, stream};
    stream::iter(0..50)
        .for_each_concurrent(50, |_| {
            let models = models.clone();
            async move {
                models.embed(&["我住哪里？".into()], true).await.unwrap();
                models
                    .rerank("我住哪里？", &["用户住在杭州。".into()])
                    .await
                    .unwrap();
            }
        })
        .await;
    assert_eq!(embedded.load(Ordering::Relaxed), 1);
    assert_eq!(reranked.load(Ordering::Relaxed), 1);
    models.embed(&["我住哪里？".into()], false).await.unwrap();
    models
        .rerank("我住哪里？", &["用户住在上海。".into()])
        .await
        .unwrap();
    assert_eq!(
        embedded.load(Ordering::Relaxed),
        2,
        "query and document instructions must have different cache keys"
    );
    assert_eq!(
        reranked.load(Ordering::Relaxed),
        2,
        "editing the memory must invalidate semantic scores"
    );
    task.abort();
}

#[tokio::test]
async fn failed_inference_does_not_poison_success_cache() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let router = Router::new().route(
        "/embed",
        post(move || {
            let failed = count.fetch_add(1, Ordering::Relaxed) == 0;
            async move {
                let mut v = vec![0.0; 1024];
                v[0] = 1.0;
                (
                    if failed {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::OK
                    },
                    Json(json!({"embeddings":[v]})),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let temp = tempfile::TempDir::new().unwrap();
    let models = Models::new(&config(&temp, &base)).unwrap();
    let text = vec!["用户住在杭州。".into()];
    assert!(models.embed(&text, false).await.is_err());
    assert!(models.embed(&text, false).await.is_ok());
    assert!(models.embed(&text, false).await.is_ok());
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    task.abort();
}

#[tokio::test]
async fn automatic_decay_allows_a_newer_queued_confirmation() {
    let temp = tempfile::TempDir::new().unwrap();
    let store = Store::open(temp.path().join("db").to_str().unwrap())
        .await
        .unwrap();
    let old = source(&store, "用户喜欢咖啡。").await;
    apply(&store, &old, "user.preference.coffee", &old.content).await;
    let fresh = source(&store, "用户喜欢咖啡。").await;
    sqlx::query("UPDATE memories SET updated_at=0")
        .execute(&store.pool)
        .await
        .unwrap();
    assert_eq!(store.expire(180.0, 0.12).await.unwrap(), 1);
    apply(&store, &fresh, "user.preference.coffee", &fresh.content).await;
    let active = store.active_memories(10).await.unwrap();
    assert_eq!(
        active.len(),
        1,
        "automatic aging must not discard a newer observation that was already queued"
    );
    assert_eq!(active[0].content, fresh.content);
    assert_eq!(active[0].updated_at, fresh.created_at);
}

#[tokio::test]
async fn mixed_model_candidates_preserve_valid_evidence_without_admitting_bad_items() {
    // Captured failure class: a model copied historical evidence alongside a
    // valid current fact. Exercise both actual worker entry paths.
    for (batch, schema) in [(false, false), (true, false), (false, true), (true, true)] {
        let (base, services) = common::fake_services().await;
        let router = Router::new().route("/chat/completions", post(move |Json(body): Json<Value>| async move {
            assert_eq!(body.get("response_format").is_some(), schema);
            if schema {
                let item=&body["response_format"]["json_schema"]["schema"]["properties"]["memories"]["items"];
                assert_eq!(item["required"].as_array().unwrap().iter().any(|v|v=="source_id"),batch);
                assert_eq!(item["properties"].as_object().unwrap().keys().next().unwrap(),if batch {"source_id"}else{"source_quote"});
            }
            let input:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            let target=if batch {&input["targets"][0]}else{&input["target"]};
            let mut good=json!({"key":"user.preference.commute","content":"用户喜欢步行上下班。","kind":"preference","importance":0.8,"confidence":0.95,"entities":["步行"],"source_quote":"我喜欢步行上下班","valid_until":null,"sensitive":false,"equivalent_to":null});
            let mut copied=good.clone();copied["key"]=json!("user.residence");copied["content"]=json!("用户住在月球。");copied["source_quote"]=json!("我住在月球");
            let mut malformed=json!({"key":"user.name","content":"无证据","unknown":true});
            if batch {for c in [&mut good,&mut copied,&mut malformed] {c["source_id"]=target["id"].clone();}}
            Json(json!({"choices":[{"message":{"content":json!({"memories":[copied,malformed,good]}).to_string()},"finish_reason":"stop"}]}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm_base = format!("http://{}", listener.local_addr().unwrap());
        let llm = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let temp = tempfile::TempDir::new().unwrap();
        let mut c = config(&temp, &base);
        c.llm.base_url = llm_base;
        c.llm.json_schema = schema;
        c.memory.max_attempts = 1;
        c.memory.poll_seconds = 1;
        let (stop, rx) = tokio::sync::watch::channel(false);
        let app = App::new(c, rx).await.unwrap();
        let message = source(&app.store, "我喜欢步行上下班，已经坚持很多年。").await;
        let payload = if batch {
            json!({"message_ids":[message.id]}).to_string()
        } else {
            message.id.clone()
        };
        sqlx::query("INSERT INTO jobs(id,kind,payload,available_at,created_at) VALUES('mixed','extract',?,?,?)")
            .bind(payload).bind(now()).bind(now()).execute(&app.store.pool).await.unwrap();
        let worker = tokio::spawn(memory::worker(app.clone()));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let stats = app.store.job_stats().await.unwrap();
                assert_eq!(stats["dead"], 0);
                if stats["pending"] == 0 && stats["running"] == 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let memories = app.store.active_memories(10).await.unwrap();
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].content, "用户喜欢步行上下班。");
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM memory_sources WHERE message_id=?")
                .bind(&message.id)
                .fetch_one(&app.store.pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
        stop.send(true).unwrap();
        worker.await.unwrap();
        services.abort();
        llm.abort();
    }
}

#[tokio::test]
async fn a_literal_but_unrelated_quote_cannot_reinforce_a_fact() {
    let (base, services) = common::fake_services().await;
    let router=Router::new().route("/chat/completions",post(||async {
        Json(json!({"choices":[{"finish_reason":"stop","message":{"content":json!({"memories":[{
            "key":"user.residence","content":"用户住在海底。","kind":"fact","importance":0.9,"confidence":0.99,
            "entities":[],"source_quote":"哈哈，好的。","valid_until":null,"sensitive":false,"equivalent_to":null
        }]}).to_string()}}]}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let llm_base = format!("http://{}", listener.local_addr().unwrap());
    let llm = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let temp = tempfile::TempDir::new().unwrap();
    let mut c = config(&temp, &base);
    c.llm.base_url = llm_base;
    c.memory.max_attempts = 1;
    c.memory.poll_seconds = 1;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    let m = source(&app.store, "哈哈，好的。").await;
    sqlx::query("INSERT INTO jobs(id,kind,payload,available_at,created_at) VALUES('unrelated','extract',?,?,?)").bind(&m.id).bind(now()).bind(now()).execute(&app.store.pool).await.unwrap();
    let worker = tokio::spawn(memory::worker(app.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if app.store.job_stats().await.unwrap()["dead"] == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(app.store.active_memories(10).await.unwrap().is_empty());
    let receipts: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM extracted_messages WHERE message_id=?")
            .bind(&m.id)
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
    assert_eq!(
        receipts, 0,
        "unsupported output stays retryable rather than acknowledging an extraction"
    );
    stop.send(true).unwrap();
    worker.await.unwrap();
    services.abort();
    llm.abort();
}

#[tokio::test]
async fn an_equivalence_hint_cannot_replace_a_new_value_with_the_old_fact() {
    let (base, services) = common::fake_services().await;
    let router=Router::new().route("/chat/completions",post(|Json(body):Json<Value>|async move {
        let input:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        let old=&input["existing"][0];assert!(old["id"].is_string());
        Json(json!({"choices":[{"finish_reason":"stop","message":{"content":json!({"memories":[{
            "key":"user.residence","content":"用户现在住在北海。","kind":"fact","importance":0.9,"confidence":0.99,
            "entities":["北海"],"source_quote":"我现在住在北海","valid_until":null,"sensitive":false,"equivalent_to":old["id"]
        }]}).to_string()}}]}))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let llm_base = format!("http://{}", listener.local_addr().unwrap());
    let llm = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let temp = tempfile::TempDir::new().unwrap();
    let mut c = config(&temp, &base);
    c.llm.base_url = llm_base;
    c.memory.max_attempts = 1;
    c.memory.poll_seconds = 1;
    let (stop, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    let prior = source(&app.store, "用户住在银川。").await;
    let vector = app
        .models
        .embed(std::slice::from_ref(&prior.content), false)
        .await
        .unwrap()
        .remove(0);
    app.store
        .apply_candidates(
            &prior,
            vec![(fact("user.residence", &prior.content), vector, None)],
            "none",
        )
        .await
        .unwrap();
    let old = app.store.active_memories(10).await.unwrap().remove(0);
    let current = source(&app.store, "我现在住在北海，刚刚搬家完成。").await;
    sqlx::query("INSERT INTO jobs(id,kind,payload,available_at,created_at) VALUES('equivalence','extract',?,?,?)").bind(&current.id).bind(now()).bind(now()).execute(&app.store.pool).await.unwrap();
    let worker = tokio::spawn(memory::worker(app.clone()));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let stats = app.store.job_stats().await.unwrap();
            assert_eq!(stats["dead"], 0);
            if stats["pending"] == 0 && stats["running"] == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let memories = app.store.active_memories(10).await.unwrap();
    assert_eq!(memories.len(), 1);
    assert_eq!(memories[0].content, "用户现在住在北海。");
    assert_ne!(memories[0].id, old.id);
    assert_eq!(
        app.store.memory(&old.id).await.unwrap().unwrap().status,
        "superseded"
    );
    stop.send(true).unwrap();
    worker.await.unwrap();
    services.abort();
    llm.abort();
}
