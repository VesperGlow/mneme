//! Explicit opt-in: run only against the disposable services created by scripts/smoke-services.sh.
use mneme::{App, config::Config, domain::*, memory, store::BeginTurn};
use serde_json::json;

#[tokio::test]
#[ignore = "requires real CPU models on :18090 and disposable Neo4j on :17474"]
async fn real_chinese_models_and_neo4j_projection() {
    let temp = tempfile::TempDir::new().unwrap();
    let mut config: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
    config.api_token = "smoke-test-token-01234567890123456789".into();
    config.llm_key = "unused".into();
    config.neo4j_password = "mneme-smoke-only-password".into();
    config.database = temp.path().join("db").to_str().unwrap().into();
    config.models.base_url = "http://127.0.0.1:18090".into();
    config.neo4j.base_url = "http://127.0.0.1:17474".into();
    let (shutdown, rx) = tokio::sync::watch::channel(false);
    let app = App::new(config, rx).await.unwrap();
    assert!(app.models.health().await);
    app.graph.initialize().await.unwrap();
    let docs = vec![
        "用户现在住在成都。".into(),
        "用户喜欢不加糖的拿铁，经常在成都喝咖啡。".into(),
    ];
    let vectors = app.models.embed(&docs, false).await.unwrap();
    let query = "我现在住在哪座城市？";
    let query_vector = app
        .models
        .embed(&[query.into()], true)
        .await
        .unwrap()
        .remove(0);
    let scores = app.models.rerank(query, &docs).await.unwrap();
    assert!(scores[0] > scores[1], "住所应排在咖啡偏好前：{scores:?}");
    let session = app.store.create_session().await.unwrap();
    let mut ids = Vec::new();
    for (i, (content, vector)) in docs.iter().zip(vectors).enumerate() {
        let BeginTurn::Started(user) = app
            .store
            .begin_turn(&session, &id(), content)
            .await
            .unwrap()
        else {
            panic!()
        };
        app.store.complete_turn(&user, "已收到").await.unwrap();
        let candidate = Candidate {
            key: format!("user.smoke_{session}.{i}"),
            content: content.clone(),
            kind: "fact".into(),
            importance: 0.9,
            confidence: 0.99,
            entities: vec!["成都".into()],
            source_quote: content.clone(),
            valid_until: None,
            sensitive: false,
            equivalent_to: None,
        };
        app.store
            .apply_candidates(
                &user,
                vec![(candidate, vector, None)],
                &format!("extract:{}", user.turn_id),
            )
            .await
            .unwrap();
        let memory = app
            .store
            .active_memories(10)
            .await
            .unwrap()
            .into_iter()
            .find(|memory| memory.content == *content)
            .unwrap();
        app.graph.project(&memory).await.unwrap();
        ids.push(memory.id);
    }
    assert!(
        app.graph
            .keyword("成都", 10)
            .await
            .unwrap()
            .contains(&ids[0])
    );
    assert!(
        app.graph
            .vector(&query_vector, 10)
            .await
            .unwrap()
            .contains(&ids[0])
    );
    assert!(
        app.graph
            .neighbors(&[ids[0].clone()], 10)
            .await
            .unwrap()
            .contains(&ids[1])
    );
    assert!(
        app.graph
            .expand("你还记得成都吗", 10)
            .await
            .unwrap()
            .contains(&ids[0])
    );
    let recalled = memory::recall(&app, query).await.unwrap();
    assert!(!recalled.degraded);
    assert_eq!(recalled.memories[0].memory.id, ids[0]);
    // The Console writes SQLite first; its actual worker must replace both vector and entity indexes.
    let original = app.store.memory(&ids[0]).await.unwrap().unwrap();
    let write = mneme::console_store::MemoryWrite {
        content: "用户现在住在杭州。".into(),
        kind: "fact".into(),
        importance: 0.9,
        confidence: 0.99,
        entities: vec!["杭州".into()],
        valid_until: None,
        expected_revision: Some(original.revision),
    };
    assert!(matches!(
        app.store
            .manual_memory(Some(&ids[0]), &write)
            .await
            .unwrap(),
        mneme::console_store::WriteResult::Saved(_)
    ));
    let worker = tokio::spawn(memory::worker(app.clone()));
    async fn synced(app: &App, id: &str) {
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            loop {
                let m = app.store.memory(id).await.unwrap().unwrap();
                if m.indexed_revision == m.revision {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("real projection did not synchronize");
    }
    synced(&app, &ids[0]).await;
    assert!(
        app.graph
            .keyword("杭州", 10)
            .await
            .unwrap()
            .contains(&ids[0])
    );
    assert!(
        !app.graph
            .keyword("成都", 10)
            .await
            .unwrap()
            .contains(&ids[0])
    );
    let entities = app
        .graph
        .query(
            "MATCH (m:Memory {id:$id})-[:MENTIONS]->(e:Entity) RETURN e.name",
            json!({"id":ids[0]}),
        )
        .await
        .unwrap();
    assert_eq!(entities, vec![vec![json!("杭州")]]);
    let debug = memory::debug_recall(&app, query).await.unwrap();
    assert!(!debug.degraded);
    assert_eq!(debug.memories[0].memory.content, "用户现在住在杭州。");
    assert!(debug.trace.unwrap().channels[0].hits[0].score.is_some());
    app.store.delete_memory(&ids[0], None).await.unwrap();
    assert!(
        !app.store
            .save_embedding(&ids[0], original.revision, &query_vector)
            .await
            .unwrap()
    );
    synced(&app, &ids[0]).await;
    assert_eq!(
        app.graph
            .query(
                "MATCH (m:Memory {id:$id}) RETURN count(m)",
                json!({"id":ids[0]})
            )
            .await
            .unwrap(),
        vec![vec![json!(0)]]
    );
    assert_eq!(
        app.graph
            .query("MATCH (e:Entity {name:'杭州'}) RETURN count(e)", json!({}))
            .await
            .unwrap(),
        vec![vec![json!(0)]]
    );
    shutdown.send(true).unwrap();
    worker.await.unwrap();
    // A new store handle after closing the database still reads its authoritative memory.
    let path = app.config.database.clone();
    let graph = app.graph.clone();
    app.store.pool.close().await;
    drop(app);
    let store = mneme::store::Store::open(&path).await.unwrap();
    assert_eq!(store.active_memories(10).await.unwrap().len(), 1);
    for id in ids {
        store.forget(&id).await.unwrap();
        graph
            .project(&store.memory(&id).await.unwrap().unwrap())
            .await
            .unwrap();
    }
    graph
        .query("MATCH (e:Entity) WHERE NOT (e)--() DELETE e", json!({}))
        .await
        .unwrap();
}
