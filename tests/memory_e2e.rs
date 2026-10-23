//! Real local causal model -> durable worker -> real embedding/Neo4j -> recall -> real answer.
//! Only scripts/memory-e2e.py may start this ignored suite, with disposable graph marker.
use axum::{Json, Router, body::Body, response::IntoResponse, routing::post};
use futures_util::StreamExt;
use mneme::{App, chat, config::Config, domain::*, memory, store::BeginTurn};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

fn matches(text: &str, fact: &Value) -> bool {
    // A stored first-person paraphrase still refers to its user source.
    let text = text.replace('我', "用户");
    fact["all"].as_array().unwrap().iter().all(|group| {
        group
            .as_array()
            .unwrap()
            .iter()
            .any(|v| text.contains(v.as_str().unwrap()))
    })
}
fn labels<'a>(text: &str, facts: &'a [Value]) -> HashSet<&'a str> {
    facts
        .iter()
        .filter(|f| matches(text, f))
        .map(|f| f["label"].as_str().unwrap())
        .collect()
}
fn contains_forbidden(text: &str, value: &Value) -> bool {
    match value {
        Value::String(word) => text.contains(word),
        Value::Array(items) => items.iter().any(|v| contains_forbidden(text, v)),
        _ => false,
    }
}

#[test]
fn forbidden_gold_accepts_flat_or_nested_alternative_lists() {
    assert!(contains_forbidden("星期三", &json!([["周三", "星期三"]])));
    assert!(contains_forbidden("周三", &json!(["周三"])));
    assert!(!contains_forbidden("周四", &json!([["周三", "星期三"]])));
}
async fn drain(app: &App) {
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            let stats = app.store.job_stats().await.unwrap();
            if stats["pending"] == 0 && stats["running"] == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("isolated durable worker timeout");
}
async fn generate(
    app: &App,
    source: &Message,
    history: Vec<Message>,
    recalled: &[Recall],
) -> (String, f64, f64) {
    let at = Instant::now();
    let messages = chat::build_context(app, history, source, recalled);
    let context_ms = at.elapsed().as_secs_f64() * 1000.0;
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let reader = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let at = Instant::now();
    let answer = app.llm.stream(&messages, &tx).await;
    let ms = at.elapsed().as_secs_f64() * 1000.0;
    drop(tx);
    reader.await.unwrap();
    (
        answer.unwrap_or_else(|e| format!("EVALUATION_ERROR: {e}")),
        context_ms,
        ms,
    )
}
async fn setup(run: &str, dir: &tempfile::TempDir, base: &str) -> App {
    setup_threshold(run, dir, base, 100000).await
}
async fn setup_threshold(run: &str, dir: &tempfile::TempDir, base: &str, threshold: usize) -> App {
    let mut c: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    c.database = dir.path().join("db").to_str().unwrap().into();
    c.api_token = "memory-evaluation-only-01234567890123456789".into();
    c.llm_key = "local-no-cost".into();
    c.llm.model = "mneme-eval-local".into();
    c.llm.base_url = base.into();
    c.llm.context_tokens = 16384;
    c.llm.max_tokens = 240;
    c.llm.temperature = 0.0;
    c.llm.json_schema = std::env::var("MNEME_E2E_SCHEMA").as_deref() != Ok("off");
    c.models.base_url = base.into();
    c.neo4j.base_url = base.into();
    c.neo4j_password = "mneme-memory-eval-only".into();
    c.memory.extract_every_tokens = threshold;
    c.memory.poll_seconds = 1;
    c.memory.max_attempts = 1;
    let (_, rx) = tokio::sync::watch::channel(false);
    let app = App::new(c, rx).await.unwrap();
    assert_eq!(
        app.graph
            .query(
                "MATCH (e:MnemeEvaluation {id:$id}) RETURN e.id",
                json!({"id":run})
            )
            .await
            .unwrap()
            .len(),
        1,
        "disposable Neo4j marker required"
    );
    app.graph.initialize().await.unwrap();
    app
}
// Record usage before forwarding it: consumers can stop at [DONE] without
// polling the upstream to EOF, so an audit performed after the stream loses rows.
async fn audited_sse(
    response: reqwest::Response,
    log: Arc<Mutex<Vec<Value>>>,
    mut record: Value,
    at: Instant,
) -> Body {
    record["usage"] = Value::Null;
    record["usage_frames"] = json!([]);
    record["ms"] = json!(at.elapsed().as_secs_f64() * 1000.0);
    let index = {
        let mut rows = log.lock().await;
        let index = rows.len();
        rows.push(record);
        index
    };
    let stream = async_stream::stream! {
        let mut upstream = response.bytes_stream();
        let mut pending = Vec::new();
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => {
                    pending.extend_from_slice(&bytes);
                    while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
                        let line: Vec<_> = pending.drain(..=end).collect();
                        if let Some(data) = std::str::from_utf8(&line).ok()
                            .and_then(|line| line.strip_prefix("data:"))
                            .and_then(|data| serde_json::from_str::<Value>(data.trim()).ok())
                            && let Some(usage) = data.get("usage").filter(|usage| usage.is_object()) {
                            let mut rows = log.lock().await;
                            rows[index]["usage"] = usage.clone();
                            rows[index]["usage_frames"].as_array_mut().unwrap().push(usage.clone());
                            rows[index]["ms"] = json!(at.elapsed().as_secs_f64() * 1000.0);
                        }
                    }
                    yield Ok::<_, reqwest::Error>(bytes);
                }
                Err(error) => { yield Err(error); break; }
            }
        }
    };
    Body::from_stream(stream)
}

#[tokio::test]
async fn streaming_audit_survives_done_without_upstream_eof() {
    use axum::body::HttpBody;
    let handler = || async {
        let stream = async_stream::stream! {
            let wire = concat!(
                "data: {\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":5,\"total_tokens\":105}}\r\n\r\n",
                "data: {\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":10,\"total_tokens\":110}}\n\n",
                "data: [DONE]\n\n"
            );
            for chunk in wire.as_bytes().chunks(7) {
                yield Ok::<_, std::convert::Infallible>(chunk.to_vec());
            }
            std::future::pending::<()>().await;
        };
        Body::from_stream(stream)
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/", post(handler)))
            .await
            .unwrap();
    });
    let response = reqwest::Client::new().post(url).send().await.unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut body = audited_sse(
        response,
        log.clone(),
        json!({"stream":true}),
        Instant::now(),
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut received = Vec::new();
        while !String::from_utf8_lossy(&received).contains("[DONE]\n\n") {
            let frame = std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx))
                .await
                .unwrap()
                .unwrap();
            if let Ok(bytes) = frame.into_data() {
                received.extend_from_slice(&bytes);
            }
        }
    })
    .await
    .unwrap();
    drop(body);
    let rows = log.lock().await;
    assert_eq!(rows.len(), 1, "usage snapshots are one physical request");
    assert_eq!(rows[0]["usage"]["total_tokens"], 110);
    assert_eq!(rows[0]["usage_frames"].as_array().unwrap().len(), 2);
    server.abort();
}

async fn proxy() -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let log = calls.clone();
    // The pinned local server may close an idle keep-alive socket between
    // requests. A fresh upstream connection also keeps failure attribution clear.
    let client = reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap();
    let handler = move |uri: axum::http::Uri,
                        headers: axum::http::HeaderMap,
                        Json(body): Json<Value>| {
        let log = log.clone();
        let client = client.clone();
        async move {
            let at = Instant::now();
            // Fixed loopback upstream, no endpoint from environment, no answer synthesis.
            let path = uri.path();
            let upstream = match path {
                "/chat/completions" => "http://127.0.0.1:18091/v1/chat/completions".to_owned(),
                "/embed" | "/rerank" => format!("http://127.0.0.1:18090{path}"),
                "/db/neo4j/tx/commit" => format!("http://127.0.0.1:17474{path}"),
                _ => panic!("unexpected evaluation endpoint"),
            };
            let mut req = client.post(upstream).json(&body);
            if let Some(auth) = headers.get("authorization") {
                req = req.header("authorization", auth);
            }
            let response = req.send().await.unwrap();
            let status = response.status();
            if body["stream"] == true {
                (
                    status,
                    [("content-type", "text/event-stream")],
                    audited_sse(response, log, json!({"endpoint":path,"request":body,"stream":true,"status":status.as_u16()}), at).await,
                )
                    .into_response()
            } else {
                let bytes = response.bytes().await.unwrap();
                log.lock().await.push(json!({"endpoint":path,"request":if path=="/chat/completions"{body}else{Value::Null},"response":if path=="/chat/completions"{serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null)}else{Value::Null},"usage":serde_json::from_slice::<Value>(&bytes).ok().map(|v|v["usage"].clone()),"status":status.as_u16(),"ms":at.elapsed().as_secs_f64()*1000.0}));
                (
                    status,
                    [("content-type", "application/json")],
                    Body::from(bytes),
                )
                    .into_response()
            }
        }
    };
    let router = Router::new()
        .route("/chat/completions", post(handler.clone()))
        .route("/embed", post(handler.clone()))
        .route("/rerank", post(handler.clone()))
        .route("/db/neo4j/tx/commit", post(handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, calls, task)
}

#[tokio::test]
#[ignore = "real offline causal model and disposable services; scripts/memory-e2e.py"]
async fn real_local_e2e() {
    let run = std::env::var("MNEME_EVAL_RUN").expect("isolated services required");
    let output = std::env::var("MNEME_EVAL_OUTPUT").unwrap();
    let split = std::env::var("MNEME_E2E_SPLIT").unwrap_or("all".into());
    let mut data: Value = serde_json::from_str(if split == "blind5" {
        include_str!("fixtures/memory/e2e-blind5.json")
    } else if split == "blind4" {
        include_str!("fixtures/memory/e2e-blind4.json")
    } else if split == "blind3" {
        include_str!("fixtures/memory/e2e-blind3.json")
    } else if split == "blind2" {
        include_str!("fixtures/memory/e2e-blind2.json")
    } else if split == "blind" {
        include_str!("fixtures/memory/e2e-blind.json")
    } else if split == "confirm" {
        include_str!("fixtures/memory/e2e-confirm.json")
    } else if split == "unseen" {
        include_str!("fixtures/memory/e2e-unseen.json")
    } else {
        include_str!("fixtures/memory/e2e.json")
    })
    .unwrap();
    if split == "full" || split == "combined" || split == "core" || split == "quality" {
        for raw in [
            include_str!("fixtures/memory/e2e-unseen.json"),
            include_str!("fixtures/memory/e2e-confirm.json"),
            if split == "combined" || split == "quality" {
                include_str!("fixtures/memory/e2e-blind3.json")
            } else if split == "core" {
                "{\"profiles\":[]}"
            } else {
                include_str!("fixtures/memory/e2e-blind.json")
            },
        ] {
            let extra: Value = serde_json::from_str(raw).unwrap();
            data["profiles"]
                .as_array_mut()
                .unwrap()
                .extend(extra["profiles"].as_array().unwrap().clone());
        }
        if split == "quality" {
            for raw in [
                include_str!("fixtures/memory/e2e-blind4.json"),
                include_str!("fixtures/memory/e2e-blind5.json"),
            ] {
                let extra: Value = serde_json::from_str(raw).unwrap();
                data["profiles"]
                    .as_array_mut()
                    .unwrap()
                    .extend(extra["profiles"].as_array().unwrap().clone());
            }
        }
    }
    let (base, calls, proxy) = proxy().await;
    let mut results = Vec::new();
    for p in data["profiles"].as_array().unwrap().iter().filter(|p| {
        split == "all"
            || split == "full"
            || split == "combined"
            || split == "core"
            || p["split"] == split
    }) {
        let temp = tempfile::TempDir::new().unwrap();
        let mut app = setup(&run, &temp, &base).await;
        let (stop, rx) = tokio::sync::watch::channel(false);
        app.shutdown = rx;
        let worker = tokio::spawn(memory::worker(app.clone()));
        let session = app.store.conversation().await.unwrap();
        let profile_call_start = calls.lock().await.len();
        let mut turns = Vec::new();
        let mut history = Vec::new();
        for (i, t) in p["turns"].as_array().unwrap().iter().enumerate() {
            let BeginTurn::Started(mut source) = app
                .store
                .begin_turn(&session, &id(), t["say"].as_str().unwrap())
                .await
                .unwrap()
            else {
                panic!()
            };
            let (answer, context_ms, chat_ms) = generate(&app, &source, history.clone(), &[]).await;
            app.store.complete_turn(&source, &answer).await.unwrap();
            source.created_at = now() - 90 * 86400 + t["day"].as_i64().unwrap() * 86400;
            sqlx::query("UPDATE messages SET created_at=? WHERE id=?")
                .bind(source.created_at)
                .bind(&source.id)
                .execute(&app.store.pool)
                .await
                .unwrap();
            let count_before = calls.lock().await.len();
            let model_before = app.models.embed_metrics.snapshot();
            let write_before = app.store.memory_write_metrics.snapshot();
            let at = Instant::now();
            sqlx::query("INSERT INTO jobs(id,kind,payload,available_at,created_at) VALUES(?,'extract',?,?,?)")
                .bind(format!("e2e-{i:03}")).bind(&source.id).bind(now()).bind(now()).execute(&app.store.pool).await.unwrap();
            drain(&app).await;
            let total_ms = at.elapsed().as_secs_f64() * 1000.0;
            let writes:Vec<(String,String,String)>=sqlx::query_as("SELECT m.id,m.slot,m.content FROM memories m JOIN memory_sources s ON s.memory_id=m.id WHERE s.message_id=? ORDER BY m.id")
                .bind(&source.id).fetch_all(&app.store.pool).await.unwrap();
            let facts = t["facts"].as_array().unwrap();
            let tp = facts
                .iter()
                .filter(|f| writes.iter().any(|(_, _, text)| matches(text, f)))
                .count();
            let fp = writes
                .iter()
                .filter(|(_, _, text)| labels(text, facts).is_empty())
                .count();
            let records = calls.lock().await[count_before..].to_vec();
            let jobs = app.store.job_stats().await.unwrap();
            let model_after = app.models.embed_metrics.snapshot();
            let write_after = app.store.memory_write_metrics.snapshot();
            let active_state: Vec<_> = app
                .store
                .active_memories(100)
                .await
                .unwrap()
                .iter()
                .map(|m| json!({"id":m.id,"slot":m.slot,"content":m.content,"revision":m.revision}))
                .collect();
            let row = json!({"index":i,"gold":t,"source_id":source.id,"writes":writes,"tp":tp,"fp":fp,"fn":facts.len()-tp,
                "active_state":active_state,
                "exact":tp==facts.len() && fp==0,"jobs":jobs,"extraction_and_projection_ms":total_ms,"context_ms":context_ms,"conversation_generation_ms":chat_ms,"assistant":answer,
                "sqlite_transaction_ms":if write_after.requests>write_before.requests {Some(write_after.last_ms)}else{None},
                "request_usage":app.store.turn_model_requests(&source.turn_id).await.unwrap(),"model_requests":model_after.requests-model_before.requests,"model_last_ms":model_after.last_ms,"llm_calls":records});
            println!(
                "E2E_TURN {}",
                json!({"profile":p["id"],"index":i,"exact":row["exact"],"writes":row["writes"],"ms":total_ms})
            );
            turns.push(row);
            history.push(source.clone());
            history.push(Message {
                id: id(),
                session_id: session.clone(),
                turn_id: source.turn_id.clone(),
                role: "assistant".into(),
                content: answer,
                created_at: source.created_at,
            });
            if history.len() > 4 {
                history.drain(..2);
            }
            std::fs::write(
                format!("{output}.partial"),
                serde_json::to_string_pretty(&json!({"profile":p["id"],"turns":turns})).unwrap(),
            )
            .unwrap();
        }
        let final_memories = app.store.active_memories(100).await.unwrap();
        let facts = p["final"].as_array().unwrap();
        let final_tp = facts
            .iter()
            .filter(|f| final_memories.iter().any(|m| matches(&m.content, f)))
            .count();
        let final_fp = final_memories
            .iter()
            .filter(|m| labels(&m.content, facts).is_empty())
            .count();
        let mut queries = Vec::new();
        for q in p["queries"].as_array().unwrap() {
            let query = q["say"].as_str().unwrap();
            let at = Instant::now();
            let recalled = memory::debug_recall(&app, query).await.unwrap();
            let recall_ms = at.elapsed().as_secs_f64() * 1000.0;
            let expected: HashSet<_> = q["relevant"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l.as_str().unwrap())
                .collect();
            let mut selected = HashSet::new();
            let mut irrelevant = 0;
            let mut relevant_entries = 0;
            for r in &recalled.memories {
                let found = labels(&r.memory.content, facts);
                if found.is_disjoint(&expected) {
                    irrelevant += 1;
                } else {
                    relevant_entries += 1;
                }
                selected.extend(found);
            }
            let source = Message {
                id: id(),
                session_id: session.clone(),
                turn_id: id(),
                role: "user".into(),
                content: query.into(),
                created_at: now(),
            };
            // Empty history is intentional: recalled memories are the ONLY private facts supplied.
            let (answer, context_ms, chat_ms) =
                generate(&app, &source, vec![], &recalled.memories).await;
            let answer_ok = q["answer_all"].as_array().unwrap().iter().all(|group| {
                group
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|w| answer.contains(w.as_str().unwrap()))
            }) && !contains_forbidden(&answer, &q["answer_forbidden"])
                && !answer.starts_with("EVALUATION_ERROR");
            let row = json!({"gold":q,"selected":selected,"selected_memories":recalled.memories.iter().map(|r|json!({"id":r.memory.id,"content":r.memory.content})).collect::<Vec<_>>(),
                "tp":selected.intersection(&expected).count(),"precision_tp":relevant_entries,"fp":irrelevant,"fn":expected.difference(&selected).count(),"exact":expected.is_subset(&selected) && irrelevant==0,
                "degraded":recalled.degraded,"trace":recalled.trace,"recall_ms":recall_ms,"context_ms":context_ms,"generation_ms":chat_ms,"answer":answer,"answer_ok":answer_ok});
            println!(
                "E2E_QUERY {}",
                json!({"profile":p["id"],"query":query,"selected":row["selected"],"exact":row["exact"],"answer_ok":answer_ok})
            );
            queries.push(row);
        }
        stop.send(true).unwrap();
        worker.await.unwrap();
        // Remove only IDs owned by this fresh TempDir, never a global delete.
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM memories")
            .fetch_all(&app.store.pool)
            .await
            .unwrap();
        app.graph
            .query(
                "MATCH (m:Memory) WHERE m.id IN $ids DETACH DELETE m",
                json!({"ids":ids}),
            )
            .await
            .unwrap();
        let memories: Vec<_> = final_memories
            .iter()
            .map(|m| json!({"id":m.id,"slot":m.slot,"content":m.content,"revision":m.revision}))
            .collect();
        results.push(json!({"id":p["id"],"split":p["split"],"turns":turns,"queries":queries,"final_memories":memories,"final_tp":final_tp,"final_fp":final_fp,"final_expected":facts.len(),"all_calls":calls.lock().await[profile_call_start..].to_vec(),"all_requests":ledger(&app).await}));
        std::fs::write(
            &output,
            serde_json::to_string_pretty(&json!({"run":run,"profiles":results})).unwrap(),
        )
        .unwrap();
    }
    proxy.abort();
}

async fn ledger(app: &App) -> Vec<Value> {
    let rows: Vec<(String, Option<String>, String, String, String, String)> = sqlx::query_as(
        "SELECT id,turn_id,purpose,context,usage,status FROM model_requests ORDER BY seq",
    )
    .fetch_all(&app.store.pool)
    .await
    .unwrap();
    rows.into_iter().map(|(id,turn,purpose,context,usage,status)|json!({"id":id,"turn_id":turn,"purpose":purpose,"context":serde_json::from_str::<Value>(&context).unwrap(),"usage":serde_json::from_str::<Value>(&usage).unwrap(),"status":status})).collect()
}

#[tokio::test]
#[ignore = "real authenticated API/SSE + automatic extraction + native models + disposable Neo4j"]
async fn real_local_api() {
    use tower::ServiceExt;
    let run = std::env::var("MNEME_EVAL_RUN").unwrap();
    let output = std::env::var("MNEME_EVAL_OUTPUT").unwrap();
    let temp = tempfile::TempDir::new().unwrap();
    let (base, calls, proxy) = proxy().await;
    let mut app = setup_threshold(&run, &temp, &base, 1).await;
    let (stop, rx) = tokio::sync::watch::channel(false);
    app.shutdown = rx;
    let worker = tokio::spawn(memory::worker(app.clone()));
    let router = mneme::api::router(app.clone());
    let mut turns = Vec::new();
    for text in [
        "我主要使用 Haskell 开发后端，已经用了很多年。",
        "我不使用 OCaml 作为主力开发语言。",
        "Haskell 是我吃饭的本事吗？",
        "现在我改为主要使用 OCaml 开发后端了。",
    ] {
        let turn = id();
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/chat")
            .header("authorization", format!("Bearer {}", app.config.api_token))
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"request_id":turn,"message":text}).to_string(),
            ))
            .unwrap();
        let response = router.clone().oneshot(req).await.unwrap();
        assert!(response.status().is_success());
        let bytes = axum::body::to_bytes(response.into_body(), 2_097_152)
            .await
            .unwrap();
        let wire = String::from_utf8(bytes.to_vec()).unwrap();
        assert!(wire.contains("event: done"), "{wire}");
        drain(&app).await;
        let state: Vec<_> = app
            .store
            .active_memories(100)
            .await
            .unwrap()
            .into_iter()
            .map(|m| json!({"slot":m.slot,"content":m.content,"id":m.id}))
            .collect();
        if turns.len() == 1 {
            assert!(
                state
                    .iter()
                    .any(|m| m["content"].as_str().unwrap().contains("Haskell"))
            );
            assert!(
                state
                    .iter()
                    .any(|m| m["content"].as_str().unwrap().contains("OCaml"))
            );
        }
        turns.push(json!({"turn_id":turn,"input":text,"wire":wire,"state":state}));
        std::fs::write(&output,serde_json::to_string_pretty(&json!({"turns":turns,"all_calls":calls.lock().await.clone(),"all_requests":ledger(&app).await})).unwrap()).unwrap();
    }
    let active = app.store.active_memories(100).await.unwrap();
    assert!(active.iter().any(|m| m.content.contains("OCaml")
        && !m.content.contains("不用")
        && !m.content.contains("不使用")));
    assert!(
        active
            .iter()
            .all(|m| !m.content.contains("Haskell") || m.content.contains("不再")),
        "old primary language survived: {active:?}"
    );
    stop.send(true).unwrap();
    worker.await.unwrap();
    proxy.abort();
}

#[tokio::test]
#[ignore = "real model periodic batch path; scripts/memory-e2e.py --test real_local_batch"]
async fn real_local_batch() {
    let run = std::env::var("MNEME_EVAL_RUN").unwrap();
    let output = std::env::var("MNEME_EVAL_OUTPUT").unwrap();
    let split = std::env::var("MNEME_E2E_SPLIT").unwrap_or("all".into());
    let data: Value = serde_json::from_str(include_str!("fixtures/memory/e2e.json")).unwrap();
    let (base, calls, proxy) = proxy().await;
    let mut rows = Vec::new();
    for p in data["profiles"].as_array().unwrap().iter().filter(|p| {
        split == "all"
            || split == "full"
            || split == "combined"
            || split == "core"
            || p["split"] == split
    }) {
        let temp = tempfile::TempDir::new().unwrap();
        let mut app = setup_threshold(&run, &temp, &base, 200).await;
        let session = app.store.conversation().await.unwrap();
        let mut ids = Vec::new();
        let mut history = Vec::new();
        for t in p["turns"].as_array().unwrap().iter().take(12) {
            let BeginTurn::Started(mut source) = app
                .store
                .begin_turn(&session, &id(), t["say"].as_str().unwrap())
                .await
                .unwrap()
            else {
                panic!()
            };
            let (answer, _, _) = generate(&app, &source, history.clone(), &[]).await;
            let assistant_id = app.store.complete_turn(&source, &answer).await.unwrap();
            source.created_at = now() - 90 * 86400 + t["day"].as_i64().unwrap() * 86400;
            sqlx::query("UPDATE messages SET created_at=? WHERE id=?")
                .bind(source.created_at)
                .bind(&source.id)
                .execute(&app.store.pool)
                .await
                .unwrap();
            ids.push(source.id.clone());
            history.push(source.clone());
            history.push(Message {
                id: assistant_id,
                session_id: session.clone(),
                turn_id: source.turn_id,
                role: "assistant".into(),
                content: answer,
                created_at: source.created_at,
            });
            if history.len() > 4 {
                history.drain(..2);
            }
        }
        let automatic_jobs: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE kind='extract'")
                .fetch_one(&app.store.pool)
                .await
                .unwrap();
        assert!(
            automatic_jobs > 0,
            "complete_turn must schedule real periodic extraction"
        );
        // Flush any residual sources too; existing receipts make overlapping jobs idempotent.
        sqlx::query("INSERT INTO jobs(id,kind,payload,available_at,created_at) VALUES('batch','extract',?,?,?)")
            .bind(json!({"message_ids":ids}).to_string()).bind(now()).bind(now()).execute(&app.store.pool).await.unwrap();
        let (stop, rx) = tokio::sync::watch::channel(false);
        app.shutdown = rx;
        let before = calls.lock().await.len();
        let at = Instant::now();
        let worker = tokio::spawn(memory::worker(app.clone()));
        drain(&app).await;
        let ms = at.elapsed().as_secs_f64() * 1000.0;
        stop.send(true).unwrap();
        worker.await.unwrap();
        let final_memories = app.store.active_memories(100).await.unwrap();
        // First 12 turns include the current home/preference, relation, occupation,
        // hobby and negative fact, but no pet yet. Source text/gold remains frozen.
        let facts: Vec<_> = p["final"]
            .as_array()
            .unwrap()
            .iter()
            .take(6)
            .cloned()
            .collect();
        let tp = facts
            .iter()
            .filter(|f| final_memories.iter().any(|m| matches(&m.content, f)))
            .count();
        let fp = final_memories
            .iter()
            .filter(|m| labels(&m.content, &facts).is_empty())
            .count();
        let receipts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM extracted_messages")
            .fetch_one(&app.store.pool)
            .await
            .unwrap();
        rows.push(json!({"profile":p["id"],"sources":ids.len(),"automatic_jobs":automatic_jobs,"receipts":receipts,"jobs":app.store.job_stats().await.unwrap(),"tp":tp,"fp":fp,"expected":facts.len(),"ms":ms,
            "memories":final_memories,"calls":calls.lock().await[before..].to_vec()}));
        let owned: Vec<String> = sqlx::query_scalar("SELECT id FROM memories")
            .fetch_all(&app.store.pool)
            .await
            .unwrap();
        app.graph
            .query(
                "MATCH (m:Memory) WHERE m.id IN $ids DETACH DELETE m",
                json!({"ids":owned}),
            )
            .await
            .unwrap();
        std::fs::write(&output, serde_json::to_string_pretty(&rows).unwrap()).unwrap();
    }
    proxy.abort();
}

#[tokio::test]
#[ignore = "actual API unknown-personal-fact guard; scripts/memory-e2e.py --test real_local_unknown"]
async fn real_local_unknown() {
    let run = std::env::var("MNEME_EVAL_RUN").unwrap();
    let output = std::env::var("MNEME_EVAL_OUTPUT").unwrap();
    let temp = tempfile::TempDir::new().unwrap();
    let (base, _, proxy) = proxy().await;
    let app = setup(&run, &temp, &base).await;
    let session = app.store.conversation().await.unwrap();
    let mut rows = Vec::new();
    for question in [
        "我现在的工作地点和岗位是什么？",
        "我如今住在哪座城市？",
        "你知道我亲属的姓名吗？",
    ] {
        let source = Message {
            id: id(),
            session_id: session.clone(),
            turn_id: id(),
            role: "user".into(),
            content: question.into(),
            created_at: now(),
        };
        let (answer, _, ms) = generate(&app, &source, vec![], &[]).await;
        let unknown = [
            "不知",
            "不清楚",
            "不知道",
            "没有",
            "没法",
            "无法",
            "不能",
            "未",
        ]
        .iter()
        .any(|w| answer.contains(w));
        rows.push(json!({"question":question,"answer":answer,"unknown":unknown,"ms":ms}));
        std::fs::write(&output, serde_json::to_string_pretty(&rows).unwrap()).unwrap();
        assert!(
            unknown && !answer.starts_with("EVALUATION_ERROR"),
            "unsupported personal claim: {answer}"
        );
    }
    proxy.abort();
}

#[tokio::test]
#[ignore = "paired retrieval calibration on frozen extracted text; scripts/memory-e2e.py --test real_recall_replay"]
async fn real_recall_replay() {
    let run = std::env::var("MNEME_EVAL_RUN").unwrap();
    let output = std::env::var("MNEME_EVAL_OUTPUT").unwrap();
    let source_path = std::env::var("MNEME_REPLAY_SOURCE")
        .unwrap_or("artifacts/mneme-2026-10-19-accepted-core/results.json".into());
    let source: Value = serde_json::from_slice(&std::fs::read(&source_path).unwrap()).unwrap();
    let (base, _, proxy) = proxy().await;
    let mut variants = vec![
        json!({"threshold":0.6,"profiles":[]}),
        json!({"threshold":0.3,"profiles":[]}),
    ];
    for profile in source["profiles"].as_array().unwrap() {
        let temp = tempfile::TempDir::new().unwrap();
        let app = setup(&run, &temp, &base).await;
        let session = app.store.conversation().await.unwrap();
        let memories = profile["final_memories"].as_array().unwrap();
        let docs: Vec<_> = memories
            .iter()
            .map(|m| m["content"].as_str().unwrap().to_owned())
            .collect();
        let vectors = app.models.embed(&docs, false).await.unwrap();
        for (m, vector) in memories.iter().zip(vectors) {
            let text = m["content"].as_str().unwrap();
            let mut entities = Vec::new();
            for turn in profile["turns"].as_array().unwrap() {
                for call in turn["llm_calls"].as_array().unwrap() {
                    if call["endpoint"] != "/chat/completions" {
                        continue;
                    }
                    if let Some(raw) = call
                        .pointer("/response/choices/0/message/content")
                        .and_then(Value::as_str)
                        && let Ok(parsed) = serde_json::from_str::<Value>(raw)
                    {
                        for fact in parsed["memories"].as_array().into_iter().flatten() {
                            if fact["content"]
                                .as_str()
                                .is_some_and(|s| content_hash(s) == content_hash(text))
                            {
                                for entity in fact["entities"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(Value::as_str)
                                {
                                    if entities.len() < 8 && !entities.iter().any(|e| e == entity) {
                                        entities.push(entity.to_owned());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let BeginTurn::Started(user) =
                app.store.begin_turn(&session, &id(), text).await.unwrap()
            else {
                panic!()
            };
            app.store.complete_turn(&user, "replay seed").await.unwrap();
            let candidate = Candidate {
                key: m["slot"].as_str().unwrap().into(),
                content: text.into(),
                kind: "fact".into(),
                importance: 0.8,
                confidence: 0.95,
                entities,
                source_quote: text.into(),
                valid_until: None,
                sensitive: false,
                equivalent_to: None,
                attribute_of: None,
                relation: Default::default(),
                supersedes: Vec::new(),
                tier: "long_term".into(),
                evidence_type: "explicit_fact".into(),
            };
            app.store
                .apply_candidates(&user, vec![(candidate, vector, None)], "seed")
                .await
                .unwrap();
        }
        let stored = app.store.active_memories(100).await.unwrap();
        for memory in &stored {
            app.graph.project(memory).await.unwrap();
        }
        for variant in &mut variants {
            let mut configured = app.clone();
            let mut config = (*app.config).clone();
            config.memory.rerank_rescue_score = variant["threshold"].as_f64().unwrap();
            configured.config = Arc::new(config);
            let mut queries = Vec::new();
            for q in profile["queries"].as_array().unwrap() {
                let at = Instant::now();
                let result = memory::debug_recall(&configured, q["gold"]["say"].as_str().unwrap())
                    .await
                    .unwrap();
                queries.push(json!({"gold":q["gold"],"selected_memories":result.memories.iter().map(|r|json!({"id":r.memory.id,"content":r.memory.content})).collect::<Vec<_>>(),"trace":result.trace,"recall_ms":at.elapsed().as_secs_f64()*1000.0,"degraded":result.degraded,"answer":"","answer_ok":false,"context_ms":0,"generation_ms":0}));
            }
            let mut replay = profile.clone();
            replay["queries"] = json!(queries);
            variant["profiles"].as_array_mut().unwrap().push(replay);
        }
        let ids: Vec<_> = stored.iter().map(|m| &m.id).collect();
        app.graph
            .query(
                "MATCH (m:Memory) WHERE m.id IN $ids DETACH DELETE m",
                json!({"ids":ids}),
            )
            .await
            .unwrap();
        std::fs::write(&output,serde_json::to_string_pretty(&json!({"source":source_path,"mode":"retrieval_only_paired_replay","note":"Same frozen text and reconstructed entities; common importance/confidence/age; shared warm caches. No generation or paid API calls. Only retrieval metrics are applicable.","variants":variants})).unwrap()).unwrap();
    }
    proxy.abort();
}

#[tokio::test]
#[ignore = "bounded real end-to-end contention; scripts/memory-e2e.py --test real_local_load"]
async fn real_local_load() {
    use futures_util::{StreamExt, stream};
    let run = std::env::var("MNEME_EVAL_RUN").unwrap();
    let output = std::env::var("MNEME_EVAL_OUTPUT").unwrap();
    let (base, _, proxy) = proxy().await;
    let mut cells = Vec::new();
    for concurrency in [1, 2, 5] {
        let mut contexts = Vec::new();
        for city in ["合肥", "昆明", "泉州", "贵阳", "大连", "济南"] {
            let temp = tempfile::TempDir::new().unwrap();
            let app = setup(&run, &temp, &base).await;
            contexts.push((app, temp, city));
        }
        let at = Instant::now();
        let rows=stream::iter(contexts).enumerate().map(|(i,(mut app,temp,city))|async move {
            let began=Instant::now();let session=app.store.conversation().await.unwrap();
            let say=format!("我目前定居在{city}，已经在这里稳定生活了好几年。");
            let BeginTurn::Started(source)=app.store.begin_turn(&session,&id(),&say).await.unwrap()else{panic!()};
            let (reply,_,initial_generation_ms)=generate(&app,&source,vec![],&[]).await;
            app.store.complete_turn(&source,&reply).await.unwrap();
            sqlx::query("INSERT INTO jobs(id,kind,payload,available_at,created_at) VALUES('load','extract',?,?,?)")
                .bind(&source.id).bind(now()).bind(now()).execute(&app.store.pool).await.unwrap();
            let (stop,rx)=tokio::sync::watch::channel(false);app.shutdown=rx;let start=Instant::now();
            let worker=tokio::spawn(memory::worker(app.clone()));drain(&app).await;let extraction_ms=start.elapsed().as_secs_f64()*1000.0;
            let query=["我现在定居在哪座城市？","我的常住地是哪里？","我长期生活在哪儿？","我如今住的城市叫什么？","我的定居城市是哪座？","我目前在哪里稳定生活？"][i];
            let start=Instant::now();let recalled=memory::debug_recall(&app,query).await.unwrap();let recall_ms=start.elapsed().as_secs_f64()*1000.0;
            let mut q=source.clone();q.content=query.into();
            let (answer,context_ms,generation_ms)=generate(&app,&q,vec![],&recalled.memories).await;
            stop.send(true).unwrap();worker.await.unwrap();
            let memory_ok=recalled.memories.len()==1 && recalled.memories[0].memory.content.contains(city);
            let answer_ok=memory_ok && answer.contains(city);
            let row=json!({"city":city,"ms":began.elapsed().as_secs_f64()*1000.0,"extraction_ms":extraction_ms,"initial_generation_ms":initial_generation_ms,
                "recall_ms":recall_ms,"context_ms":context_ms,"generation_ms":generation_ms,"memory_ok":memory_ok,"answer_ok":answer_ok,"answer":answer,"degraded":recalled.degraded,"trace":recalled.trace,
                "jobs":app.store.job_stats().await.unwrap()});
            let ids:Vec<String>=sqlx::query_scalar("SELECT id FROM memories").fetch_all(&app.store.pool).await.unwrap();
            app.graph.query("MATCH (m:Memory) WHERE m.id IN $ids DETACH DELETE m",json!({"ids":ids})).await.unwrap();drop(temp);row
        }).buffer_unordered(concurrency).collect::<Vec<_>>().await;
        cells.push(json!({"concurrency":concurrency,"requests":rows.len(),"throughput_rps":rows.len()as f64/at.elapsed().as_secs_f64(),"rows":rows}));
        println!("REAL_E2E_LOAD {}", cells.last().unwrap());
        std::fs::write(&output, serde_json::to_string_pretty(&cells).unwrap()).unwrap();
    }
    proxy.abort();
}
