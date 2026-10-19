//! Scripted model replay validates downstream contracts, NOT LLM semantic accuracy.
mod common;
use axum::{Json, Router, routing::post};
use mneme::{App, config::Config, domain::*, memory, store::BeginTurn};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

#[tokio::test]
async fn replay_256_natural_turns_with_labelled_writes_and_final_state() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/memory/dialogues.json")).unwrap();
    let profiles: Vec<Vec<Value>> = [
        [
            "林舟", "成都", "杭州", "林悦", "苏州", "星河", "团子", "西湖",
        ],
        [
            "沈岚",
            "武汉",
            "重庆",
            "沈珂",
            "合肥",
            "北辰",
            "年糕",
            "南滨路",
        ],
        [
            "齐远",
            "济南",
            "青岛",
            "齐宁",
            "宁波",
            "远帆",
            "饭团",
            "奥帆中心",
        ],
        [
            "许棠",
            "长沙",
            "厦门",
            "许墨",
            "福州",
            "流光",
            "糯米",
            "环岛路",
        ],
        [
            "周遥",
            "西安",
            "郑州",
            "周禾",
            "洛阳",
            "山海",
            "豆包",
            "如意湖",
        ],
        [
            "江澈",
            "昆明",
            "贵阳",
            "江宁",
            "南宁",
            "云杉",
            "布丁",
            "观山湖",
        ],
        [
            "陆川",
            "天津",
            "大连",
            "陆星",
            "沈阳",
            "启明",
            "芝麻",
            "星海广场",
        ],
        [
            "宋宁",
            "太原",
            "石家庄",
            "宋悦",
            "保定",
            "清风",
            "汤圆",
            "滹沱河",
        ],
    ]
    .iter()
    .map(|values| {
        cases
            .iter()
            .map(|case| {
                let mut case = case.clone();
                for field in ["say", "fact"] {
                    if let Some(text) = case[field].as_str() {
                        let mut text = text.to_owned();
                        for (from, to) in [
                            "林舟", "成都", "杭州", "林悦", "苏州", "星河", "团子", "西湖",
                        ]
                        .iter()
                        .zip(values)
                        {
                            text = text.replace(from, to);
                        }
                        case[field] = json!(text);
                    }
                }
                case
            })
            .collect()
    })
    .collect();
    let lookup: std::sync::Arc<HashMap<String, Value>> = std::sync::Arc::new(
        profiles
            .iter()
            .flatten()
            .map(|c| (c["say"].as_str().unwrap().to_owned(), c.clone()))
            .collect(),
    );
    let router=Router::new().route("/chat/completions",post(move |Json(body):Json<Value>|{
        let lookup=lookup.clone();async move {
            let input:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            let target=&input["target"];
            let c=&lookup[target["content"].as_str().unwrap()];
            let candidates=if c["key"].is_string() {
                vec![json!({"key":c["key"],"content":c["fact"],"kind":c["kind"].as_str().unwrap_or("fact"),
                    "importance":c["importance"].as_f64().unwrap_or(0.8),"confidence":c["confidence"].as_f64().unwrap_or(0.95),
                    "source_quote":c["say"],"entities":[],"sensitive":c["sensitive"]==true,
                    "valid_until":if c["expired"]==true {Some(chrono::DateTime::from_timestamp(now()-86400,0).unwrap().to_rfc3339())} else {None},"equivalent_to":null})]
            } else {vec![]};
            Json(json!({"choices":[{"message":{"content":json!({"memories":candidates}).to_string()},"finish_reason":"stop"}]}))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let llm_base = format!("http://{}", listener.local_addr().unwrap());
    let llm = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let mut evaluated = 0;
    let mut correct = 0;
    let mut categories = BTreeMap::<String, usize>::new();
    for cases in profiles {
        let temp = tempfile::TempDir::new().unwrap();
        let (base, services) = common::fake_services().await;
        let mut c: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
        c.memory.reconcile_updates = false; // This fixture does not implement semantic adjudication.
        c.memory.verify_recall = false;
        c.database = temp.path().join("db").to_str().unwrap().into();
        c.api_token = "memory-evaluation-only-01234567890123456789".into();
        c.llm_key = "fixture".into();
        c.llm.base_url = llm_base.clone();
        c.llm.model = "scripted-replay".into();
        c.models.base_url = base.clone();
        c.neo4j.base_url = base;
        c.neo4j_password = "fixture-password".into();
        c.memory.poll_seconds = 1;
        c.memory.max_attempts = 1;
        let (stop, rx) = tokio::sync::watch::channel(false);
        let app = App::new(c, rx).await.unwrap();
        let session = app.store.conversation().await.unwrap();
        let mut sources = Vec::new();
        let mut expected = BTreeMap::new();
        for (index, case) in cases.iter().enumerate() {
            let BeginTurn::Started(mut source) = app
                .store
                .begin_turn(&session, &id(), case["say"].as_str().unwrap())
                .await
                .unwrap()
            else {
                panic!()
            };
            app.store
                .complete_turn(&source, "我听到了，会以你的最新表达为准。")
                .await
                .unwrap();
            source.created_at = now() - (90 - index as i64 * 2) * 86400;
            sqlx::query("UPDATE messages SET created_at=? WHERE id=?")
                .bind(source.created_at)
                .bind(&source.id)
                .execute(&app.store.pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO jobs(id,kind,payload,available_at,created_at) VALUES(?,'extract',?,?,?)")
                .bind(format!("replay:{index:03}" )).bind(&source.id).bind(now()).bind(now()).execute(&app.store.pool).await.unwrap();
            if case["key"].is_string() && case["skip"] != true {
                expected.insert(
                    case["key"].as_str().unwrap().to_owned(),
                    case["fact"].as_str().unwrap().to_owned(),
                );
            }
            sources.push(source);
        }
        let worker = tokio::spawn(memory::worker(app.clone()));
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                let stats = app.store.job_stats().await.unwrap();
                assert_eq!(
                    stats["dead"], 0,
                    "scripted replay must not lose extraction jobs"
                );
                if stats["pending"] == 0 && stats["running"] == 0 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        for (case, source) in cases.iter().zip(sources) {
            let written:Vec<String>=sqlx::query_scalar("SELECT m.content FROM memories m JOIN memory_sources s ON s.memory_id=m.id WHERE s.message_id=?")
                .bind(&source.id).fetch_all(&app.store.pool).await.unwrap();
            let expected_write = if case["key"].is_string() && case["skip"] != true {
                vec![case["fact"].as_str().unwrap().to_owned()]
            } else {
                vec![]
            };
            evaluated += 1;
            correct += usize::from(written == expected_write);
            *categories
                .entry(case["category"].as_str().unwrap().into())
                .or_default() += 1;
            assert_eq!(written, expected_write, "{}", case["say"]);
        }
        let actual: BTreeMap<_, _> = app
            .store
            .active_memories(100)
            .await
            .unwrap()
            .into_iter()
            .map(|m| (m.slot, m.content))
            .collect();
        assert_eq!(actual, expected);
        stop.send(true).unwrap();
        worker.await.unwrap();
        services.abort();
    }
    println!(
        "REPLAY {}",
        json!({"turns":evaluated,"correct_write_decisions":correct,"profiles":8,"simulated_days":90,"categories":categories,"interpretation":"scripted model output; downstream validation only"})
    );
    assert_eq!(evaluated, 256);
    llm.abort();
}
