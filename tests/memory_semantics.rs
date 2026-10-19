//! Real, offline CPU embedding/reranker and disposable Neo4j; never calls an LLM.
use mneme::{App, config::Config, domain::*, memory};
use serde_json::{Value, json};
use std::{collections::HashSet, time::Instant};

#[tokio::test]
#[ignore = "offline semantic evaluation; use scripts/memory-eval.py"]
async fn real_model_semantics() {
    let run = std::env::var("MNEME_EVAL_RUN").expect("disposable service marker required");
    let output = std::env::var("MNEME_EVAL_OUTPUT").unwrap();
    let data: Value = serde_json::from_str(
        if std::env::var("MNEME_EVAL_DATASET").as_deref() == Ok("heldout") {
            include_str!("fixtures/memory/heldout.json")
        } else {
            include_str!("fixtures/memory/retrieval.json")
        },
    )
    .unwrap();
    let temp = tempfile::TempDir::new().unwrap();
    let mut c: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    c.memory.reconcile_updates = false; // This fixture does not implement semantic adjudication.
    c.memory.verify_recall = false;
    c.database = temp.path().join("db").to_str().unwrap().into();
    c.api_token = "memory-evaluation-only-01234567890123456789".into();
    c.llm_key = "unused".into();
    c.llm.model = "unused".into();
    c.llm.base_url = "http://127.0.0.1:9".into();
    c.neo4j.base_url = "http://127.0.0.1:17474".into();
    c.neo4j_password = "mneme-memory-eval-only".into();
    c.models.base_url = "http://127.0.0.1:18090".into();
    if let Ok(score) = std::env::var("MNEME_EVAL_RERANK_MIN") {
        c.memory.rerank_min_score = score.parse().unwrap();
    }
    let threshold = c.memory.rerank_min_score;
    let (_stop, rx) = tokio::sync::watch::channel(false);
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
        1
    );
    app.graph.initialize().await.unwrap();
    let prefix = format!("semantic-{run}-");
    let docs: Vec<_> = data["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["content"].as_str().unwrap().to_owned())
        .collect();
    let embed_at = Instant::now();
    let dataset = if std::env::var("MNEME_EVAL_DATASET").as_deref() == Ok("heldout") {
        "heldout"
    } else {
        "retrieval"
    };
    let vectors: Vec<Vec<f32>> = if let Ok(directory) = std::env::var("MNEME_EVAL_IMPORT_VECTORS") {
        let file = std::path::Path::new(&directory).join(format!("{dataset}.json"));
        let saved: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        assert_eq!(
            saved["documents"],
            json!(docs),
            "cross-precision corpus changed"
        );
        serde_json::from_value(saved["vectors"].clone()).unwrap()
    } else {
        // Keep baseline compatible with the original 8-document API.
        let mut vectors = Vec::new();
        for chunk in docs.chunks(8) {
            vectors.extend(app.models.embed(chunk, false).await.unwrap());
        }
        vectors
    };
    assert_eq!(vectors.len(), docs.len());
    if let Ok(directory) = std::env::var("MNEME_EVAL_EXPORT_VECTORS") {
        let directory = std::path::Path::new(&directory);
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(
            directory.join(format!("{dataset}.json")),
            serde_json::to_vec(&json!({"documents":docs,"vectors":vectors})).unwrap(),
        )
        .unwrap();
    }
    let document_embedding_ms = embed_at.elapsed().as_secs_f64() * 1000.0;
    for (m, vector) in data["memories"].as_array().unwrap().iter().zip(vectors) {
        let memory_id = format!("{prefix}{}", m["id"].as_str().unwrap());
        let expired = m["expired"] == true;
        let deleted = m["deleted"] == true;
        let status = if deleted {
            "forgotten"
        } else {
            m["status"].as_str().unwrap_or("active")
        };
        sqlx::query("INSERT INTO memories(id,slot,content,content_hash,kind,importance,confidence,entities,embedding,status,created_at,updated_at,source_seq,valid_until,deleted_at) VALUES(?,?,?,?,'fact',0.9,0.99,?,?,?,?,?,0,?,?)")
            .bind(&memory_id).bind(&memory_id).bind(m["content"].as_str().unwrap()).bind(content_hash(m["content"].as_str().unwrap()))
            .bind(m["entities"].to_string()).bind(serde_json::to_string(&vector).unwrap()).bind(status).bind(now()).bind(now())
            .bind(expired.then(||now()-86400)).bind(deleted.then(now)).execute(&app.store.pool).await.unwrap();
        let stored = app.store.memory(&memory_id).await.unwrap().unwrap();
        app.graph.project(&stored).await.unwrap();
    }
    let mut rows = Vec::new();
    let mut tp = 0;
    let mut fp = 0;
    let mut total = 0;
    let mut exact = 0;
    for q in data["queries"].as_array().unwrap() {
        let query = q["query"].as_str().unwrap();
        let expected: HashSet<_> = q["relevant"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        let at = Instant::now();
        let result = memory::debug_recall(&app, query).await.unwrap();
        let ms = at.elapsed().as_secs_f64() * 1000.0;
        let selected: HashSet<_> = result
            .memories
            .iter()
            .map(|r| r.memory.id.strip_prefix(&prefix).unwrap().to_owned())
            .collect();
        let correct = selected == expected;
        tp += selected.intersection(&expected).count();
        fp += selected.difference(&expected).count();
        total += expected.len();
        exact += usize::from(correct);
        let row = json!({"query":query,"kind":q["kind"],"expected":expected,"selected":selected,"exact":correct,"ms":ms,"degraded":result.degraded,"trace":result.trace});
        println!(
            "SEMANTIC {}",
            json!({"query":query,"selected":row["selected"],"expected":row["expected"],"ms":ms})
        );
        rows.push(row);
        std::fs::write(&output,serde_json::to_string_pretty(&json!({"mode":"real_cpu_models_real_neo4j","run":run,"threshold":threshold,"document_embedding_ms":document_embedding_ms,"queries":rows,"tp":tp,"fp":fp,"expected":total,"exact":exact,"precision":tp as f64/(tp+fp).max(1) as f64,"recall":tp as f64/total.max(1) as f64})).unwrap()).unwrap();
    }
    if std::env::var("MNEME_EVAL_REAL_LOAD").as_deref() == Ok("1") {
        use futures_util::{StreamExt, stream};
        let queries = data["queries"].as_array().unwrap();
        let mut cells = Vec::new();
        for (cold, concurrency) in [
            (false, 1),
            (false, 5),
            (false, 20),
            (false, 50),
            (true, 1),
            (true, 50),
        ] {
            let at = Instant::now();
            let rows=stream::iter(0..120).map(|i|{
                let app=app.clone();let q=&queries[i%queries.len()];let prefix=&prefix;
                async move {
                    let expected:HashSet<_>=q["relevant"].as_array().unwrap().iter().map(|v|v.as_str().unwrap().to_owned()).collect();
                    let began=Instant::now();
                    let query=if cold {format!("{}（这是第{concurrency}组第{i}次提问，请按已有信息回答。）",q["query"].as_str().unwrap())} else {q["query"].as_str().unwrap().to_owned()};
                    let result=memory::recall(&app,&query).await.unwrap();
                    let actual:HashSet<_>=result.memories.iter().map(|r|r.memory.id.strip_prefix(prefix).unwrap().to_owned()).collect();
                    json!({"ms":began.elapsed().as_secs_f64()*1000.0,"degraded":result.degraded,"exact":actual==expected,
                        "tp":actual.intersection(&expected).count(),"retrieved":actual.len(),"expected":expected.len()})
                }
            }).buffer_unordered(concurrency).collect::<Vec<_>>().await;
            let mut times: Vec<_> = rows.iter().map(|r| r["ms"].as_f64().unwrap()).collect();
            times.sort_by(f64::total_cmp);
            let sum = |field: &str| {
                rows.iter()
                    .map(|r| r[field].as_u64().unwrap_or(0))
                    .sum::<u64>()
            };
            cells.push(json!({"workload":if cold {"distinct_query_inputs"}else{"repeated_queries"},"concurrency":concurrency,"requests":rows.len(),"p50_ms":times[59],"p95_ms":times[113],"p99_ms":times[118],
                "throughput_rps":120.0/at.elapsed().as_secs_f64(),"degraded":rows.iter().filter(|r|r["degraded"]==true).count(),"exact":rows.iter().filter(|r|r["exact"]==true).count(),
                "precision":sum("tp") as f64/sum("retrieved").max(1) as f64,"recall":sum("tp") as f64/sum("expected").max(1) as f64}));
            println!("REAL_LOAD {}", cells.last().unwrap());
            std::fs::write(
                format!("{output}.load.json"),
                serde_json::to_string_pretty(&cells).unwrap(),
            )
            .unwrap();
            // Allow cancelled pending work and the current non-preemptible CPU batch to drain.
            for _ in 0..50 {
                if app.models.info().await.unwrap()["queued"] == 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
    app.graph
        .query(
            "MATCH (m:Memory) WHERE m.id STARTS WITH $prefix DETACH DELETE m",
            json!({"prefix":prefix}),
        )
        .await
        .unwrap();
}
