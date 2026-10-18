//! Real local causal model -> durable worker -> real embedding/Neo4j -> recall -> real answer.
//! Only scripts/memory-e2e.py may start this ignored suite, with disposable graph marker.
use axum::{Json, Router, body::Body, response::IntoResponse, routing::post};
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
    c.memory.extract_every_chars = threshold;
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
                    Body::from_stream(response.bytes_stream()),
                )
                    .into_response()
            } else {
                let bytes = response.bytes().await.unwrap();
                log.lock().await.push(json!({"endpoint":path,"request":if path=="/chat/completions"{body}else{Value::Null},"response":if path=="/chat/completions"{serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null)}else{Value::Null},"status":status.as_u16(),"ms":at.elapsed().as_secs_f64()*1000.0}));
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
    let data: Value = serde_json::from_str(if split == "confirm" {
        include_str!("fixtures/memory/e2e-confirm.json")
    } else if split == "unseen" {
        include_str!("fixtures/memory/e2e-unseen.json")
    } else {
        include_str!("fixtures/memory/e2e.json")
    })
    .unwrap();
    let (base, calls, proxy) = proxy().await;
    let mut results = Vec::new();
    for p in data["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| split == "all" || p["split"] == split)
    {
        let temp = tempfile::TempDir::new().unwrap();
        let mut app = setup(&run, &temp, &base).await;
        let (stop, rx) = tokio::sync::watch::channel(false);
        app.shutdown = rx;
        let worker = tokio::spawn(memory::worker(app.clone()));
        let session = app.store.conversation().await.unwrap();
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
                "model_requests":model_after.requests-model_before.requests,"model_last_ms":model_after.last_ms,"llm_calls":records});
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
            }) && !q["answer_forbidden"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| answer.contains(w.as_str().unwrap()))
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
        results.push(json!({"id":p["id"],"split":p["split"],"turns":turns,"queries":queries,"final_memories":memories,"final_tp":final_tp,"final_fp":final_fp,"final_expected":facts.len()}));
        std::fs::write(
            &output,
            serde_json::to_string_pretty(&json!({"run":run,"profiles":results})).unwrap(),
        )
        .unwrap();
    }
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
    for p in data["profiles"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| split == "all" || p["split"] == split)
    {
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
