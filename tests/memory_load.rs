//! No external LLM. Ignored workload; scripts/memory-eval.py owns disposable services.
use axum::{Json, Router, http::StatusCode, routing::post};
use futures_util::{StreamExt, stream};
use mneme::{App, config::Config, domain::*, memory};
use serde_json::{Value, json};
use std::time::Instant;

async fn fixture() -> (String, tokio::task::JoinHandle<()>) {
    let router = Router::new()
        .route(
            "/embed",
            post(|Json(body): Json<Value>| async move {
                let texts = body["texts"].as_array().unwrap();
                if texts.len() > 8 {
                    return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({})));
                }
                let vectors: Vec<_> = texts
                    .iter()
                    .map(|_| {
                        let mut v = vec![0.0; 1024];
                        v[0] = 1.0;
                        v
                    })
                    .collect();
                (StatusCode::OK, Json(json!({"embeddings":vectors})))
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
                        if body["query"] == "档案00000" && d.as_str().unwrap().contains("档案00000")
                        {
                            0.99
                        } else {
                            0.001
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
fn percentile(values: &[f64], p: f64) -> f64 {
    values[((values.len() - 1) as f64 * p).ceil() as usize]
}
fn process_memory() -> Value {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    json!(
        status
            .lines()
            .filter(|l| l.starts_with("VmRSS:")
                || l.starts_with("VmHWM:")
                || l.starts_with("Threads:"))
            .collect::<Vec<_>>()
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "isolated load matrix; use scripts/memory-eval.py"]
async fn isolated_load_matrix() {
    let run = std::env::var("MNEME_EVAL_RUN").expect("runner must supply isolation marker");
    assert!(run.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'));
    let (base, fixture) = fixture().await;
    let output = std::env::var("MNEME_EVAL_OUTPUT").unwrap();
    let mut cells = Vec::new();
    for size in [100, 1000, 10000] {
        let temp = tempfile::TempDir::new().unwrap();
        let mut c: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
        c.database = temp.path().join("eval.db").to_str().unwrap().into();
        c.llm.model = "fixture".into();
        c.llm.base_url = base.clone();
        c.models.base_url = base.clone();
        c.neo4j.base_url = "http://127.0.0.1:17474".into();
        c.neo4j_password = "mneme-memory-eval-only".into();
        c.api_token = "memory-evaluation-only-01234567890123456789".into();
        c.llm_key = "fixture".into();
        let (_stop, rx) = tokio::sync::watch::channel(false);
        let app = App::new(c, rx).await.unwrap();
        let marker = app
            .graph
            .query(
                "MATCH (e:MnemeEvaluation {id:$id}) RETURN e.id",
                json!({"id":run}),
            )
            .await
            .unwrap();
        assert_eq!(
            marker.len(),
            1,
            "refusing to write without disposable database marker"
        );
        app.graph.initialize().await.unwrap();
        let seed_start = Instant::now();
        let prefix = format!("eval-{run}-{size}-");
        let mut tx = app.store.pool.begin().await.unwrap();
        let mut graph_rows = Vec::new();
        for i in 0..size {
            let memory_id = format!("{prefix}{i:05}");
            let content = format!("用户的档案{i:05}记录了长期项目第{i}项的个人偏好。");
            let mut vector = vec![0.0f32; 1024];
            vector[usize::from(i != 0)] = 1.0;
            sqlx::query("INSERT INTO memories(id,slot,content,content_hash,kind,importance,confidence,entities,embedding,status,created_at,updated_at,source_seq) VALUES(?,?,?,?,'fact',0.8,0.95,'[]',?,'active',?,?,?)")
                .bind(&memory_id).bind(&memory_id).bind(&content).bind(content_hash(&content))
                .bind(serde_json::to_string(&vector).unwrap()).bind(now()).bind(now()+i as i64).bind(i as i64)
                .execute(&mut *tx).await.unwrap();
            graph_rows.push(json!({"id":memory_id,"content":content,"vector":vector,"updated_at":now()+i as i64}));
            if graph_rows.len() == 128 || i == size - 1 {
                app.graph.query("UNWIND $rows AS r CREATE (m:Memory:ActiveMemory {id:r.id,content:r.content,embedding:r.vector,updated_at:r.updated_at})",json!({"rows":graph_rows})).await.unwrap();
                graph_rows.clear();
            }
        }
        tx.commit().await.unwrap();
        let seed_ms = seed_start.elapsed().as_secs_f64() * 1000.0;
        for concurrency in [1, 5, 20, 50] {
            // Warm indexes before each cell. CPU models are not part of this matrix.
            for _ in 0..3 {
                let _ = memory::recall(&app, "档案00000").await.unwrap();
            }
            let count = 120;
            let began = Instant::now();
            let mut rows=stream::iter(0..count).map(|i|{
                let app=app.clone();async move {
                    let start=Instant::now();
                    let query=if i%4==3 {"一个完全无关的话题"} else {"档案00000"};
                    let result=tokio::time::timeout(std::time::Duration::from_secs(12),memory::debug_recall(&app,query)).await;
                    let elapsed=start.elapsed().as_secs_f64()*1000.0;
                    match result {
                        Ok(Ok(r))=>{
                            let selected:Vec<_>=r.memories.iter().map(|m|m.memory.content.clone()).collect();
                            let relevant=query=="档案00000";
                            let correct=if relevant {selected.len()==1 && selected[0].contains("档案00000")} else {selected.is_empty()};
                            let context_at=Instant::now();
                            let source=Message{id:id(),session_id:id(),turn_id:id(),role:"user".into(),content:query.into(),created_at:now()};
                            let context=mneme::chat::build_context(&app,vec![],&source,&r.memories);
                            assert!(context.last().unwrap().content==query);
                            json!({"ms":elapsed,"error":false,"correct":correct,"degraded":r.degraded,
                                "tp":usize::from(relevant && selected.iter().any(|s|s.contains("档案00000"))),
                                "retrieved":selected.len(),"expected":usize::from(relevant),
                                "context_ms":context_at.elapsed().as_secs_f64()*1000.0,"trace":r.trace})
                        },
                        _=>json!({"ms":elapsed,"error":true,"correct":false,"tp":0,"retrieved":0,"expected":usize::from(query=="档案00000")})
                    }
                }
            }).buffer_unordered(concurrency).collect::<Vec<_>>().await;
            let wall = began.elapsed().as_secs_f64();
            rows.sort_by(|a, b| {
                a["ms"]
                    .as_f64()
                    .unwrap()
                    .total_cmp(&b["ms"].as_f64().unwrap())
            });
            let times: Vec<_> = rows.iter().map(|v| v["ms"].as_f64().unwrap()).collect();
            let sum = |field: &str| {
                rows.iter()
                    .map(|v| v[field].as_u64().unwrap_or(0))
                    .sum::<u64>()
            };
            let mean = |field: &str| {
                rows.iter()
                    .map(|v| v.pointer(field).and_then(Value::as_f64).unwrap_or(0.0))
                    .sum::<f64>()
                    / count as f64
            };
            let cell = json!({"size":size,"concurrency":concurrency,"requests":count,"p50_ms":percentile(&times,0.50),"p95_ms":percentile(&times,0.95),"p99_ms":percentile(&times,0.99),"throughput_rps":count as f64/wall,
                "errors":rows.iter().filter(|v|v["error"]==true).count(),"correct":rows.iter().filter(|v|v["correct"]==true).count(),"degraded":rows.iter().filter(|v|v["degraded"]==true).count(),
                "precision":sum("tp") as f64/sum("retrieved").max(1) as f64,"recall":sum("tp") as f64/sum("expected").max(1) as f64,
                "embedding_ms":mean("/trace/embedding_ms"),"rerank_ms":mean("/trace/rerank_ms"),"context_ms":mean("/context_ms"),
                "channel_ms":rows[0]["trace"]["channels"].as_array().map(|channels|channels.iter().map(|c|json!({"name":c["name"],"duration_ms":c["duration_ms"],"state":c["state"]})).collect::<Vec<_>>()),"process_memory":process_memory(),"sqlite_pool":app.store.pool.size(),"seed_ms":seed_ms});
            println!("CELL {}", cell);
            cells.push(cell);
            std::fs::write(
                &output,
                serde_json::to_string_pretty(
                    &json!({"mode":"real_neo4j_fixture_models","run":run,"cells":cells}),
                )
                .unwrap(),
            )
            .unwrap();
            assert!(
                rows.iter()
                    .all(|r| r["error"] == false && r["correct"] == true),
                "retrieval content must match labels, including negative queries"
            );
        }
        loop {
            let rows=app.graph.query("MATCH (m:Memory) WHERE m.id STARTS WITH $prefix WITH m LIMIT 256 DETACH DELETE m RETURN count(*)",json!({"prefix":prefix})).await.unwrap();
            if rows[0][0].as_i64() == Some(0) {
                break;
            }
        }
        app.store.pool.close().await;
    }
    fixture.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "isolated concurrent SQLite writes; use scripts/memory-eval.py"]
async fn isolated_write_matrix() {
    use mneme::store::BeginTurn;
    let output = std::env::var("MNEME_EVAL_OUTPUT").expect("runner output path required");
    let (base, server) = fixture().await;
    let mut cells = Vec::new();
    for size in [100, 1000, 10000] {
        let temp = tempfile::TempDir::new().unwrap();
        let mut c: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
        c.database = temp.path().join("db").to_str().unwrap().into();
        c.llm.model = "fixture".into();
        c.api_token = "memory-evaluation-only-01234567890123456789".into();
        c.llm_key = "fixture".into();
        c.neo4j_password = "fixture-password".into();
        c.models.base_url = base.clone();
        c.llm.base_url = base.clone();
        c.memory.extract_every_chars = 100000;
        let (_stop, rx) = tokio::sync::watch::channel(false);
        let app = App::new(c, rx).await.unwrap();
        let mut tx = app.store.pool.begin().await.unwrap();
        let vector = {
            let mut v = vec![0.0f32; 1024];
            v[0] = 1.0;
            v
        };
        let encoded = serde_json::to_string(&vector).unwrap();
        for i in 0..size {
            let text = format!("负载种子{i}的无关记忆。");
            sqlx::query("INSERT INTO memories(id,slot,content,content_hash,kind,importance,confidence,entities,embedding,status,created_at,updated_at,source_seq) VALUES(?,?,?,?,'fact',0.8,0.95,'[]',?,'active',?,?,0)")
                .bind(format!("seed-{i}")).bind(format!("seed.{i}")).bind(&text).bind(content_hash(&text)).bind(&encoded).bind(now()).bind(now())
                .execute(&mut *tx).await.unwrap();
        }
        tx.commit().await.unwrap();
        let session = app.store.conversation().await.unwrap();
        for concurrency in [1, 5, 20, 50] {
            let mut records = Vec::new();
            let mut expected = std::collections::HashMap::new();
            let validation_at = Instant::now();
            for i in 0..120 {
                let text = format!(
                    "用户的项目{}配置在并发{concurrency}阶段更新为第{i}版。",
                    i % 20
                );
                let BeginTurn::Started(source) =
                    app.store.begin_turn(&session, &id(), &text).await.unwrap()
                else {
                    panic!()
                };
                app.store.complete_turn(&source, "收到").await.unwrap();
                let candidate = Candidate {
                    key: format!("user.project.{}", i % 20),
                    content: text.clone(),
                    kind: "fact".into(),
                    importance: 0.8,
                    confidence: 0.95,
                    entities: vec![],
                    source_quote: text,
                    valid_until: None,
                    sensitive: false,
                    equivalent_to: None,
                };
                memory::validate_candidate(&candidate, &source).unwrap();
                expected.insert(candidate.key.clone(), candidate.content.clone());
                records.push((source, candidate));
            }
            let preparation_ms = validation_at.elapsed().as_secs_f64() * 1000.0;
            let at = Instant::now();
            let mut rows = stream::iter(records.iter().chain(&records).cloned())
                .map(|(source, candidate)| {
                    let store = app.store.clone();
                    let vector = vector.clone();
                    async move {
                        let began = Instant::now();
                        let result = store
                            .apply_candidates(
                                &source,
                                vec![(candidate, vector, None)],
                                &format!("extract:{}", source.id),
                            )
                            .await;
                        (began.elapsed().as_secs_f64() * 1000.0, result.is_ok())
                    }
                })
                .buffer_unordered(concurrency)
                .collect::<Vec<_>>()
                .await;
            let wall = at.elapsed().as_secs_f64();
            rows.sort_by(|a, b| a.0.total_cmp(&b.0));
            let times: Vec<_> = rows.iter().map(|r| r.0).collect();
            let actual:std::collections::HashMap<String,String>=sqlx::query_as::<_,(String,String)>("SELECT slot,content FROM memories WHERE status='active' AND slot LIKE 'user.project.%'")
                .fetch_all(&app.store.pool).await.unwrap().into_iter().collect();
            let processed: i64 = sqlx::query_scalar("SELECT count(*) FROM extracted_messages")
                .fetch_one(&app.store.pool)
                .await
                .unwrap();
            let cell = json!({"size":size,"concurrency":concurrency,"requests":rows.len(),"p50_ms":percentile(&times,0.5),"p95_ms":percentile(&times,0.95),"p99_ms":percentile(&times,0.99),
                "throughput_rps":rows.len() as f64/wall,"errors":rows.iter().filter(|r|!r.1).count(),"latest_values_correct":actual==expected,"active_slots":actual.len(),"processed_sources":processed,"preparation_ms":preparation_ms,
                "sqlite_pool":app.store.pool.size(),"process_memory":process_memory()});
            println!("WRITE_CELL {cell}");
            cells.push(cell);
            std::fs::write(
                &output,
                serde_json::to_string_pretty(
                    &json!({"mode":"sqlite_actual_transactions","cells":cells}),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(
                actual, expected,
                "latest observation wins regardless of delivery order"
            );
            assert!(rows.iter().all(|r| r.1));
        }
        app.store.pool.close().await;
    }
    server.abort();
}
