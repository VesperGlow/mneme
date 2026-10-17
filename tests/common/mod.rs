use axum::{
    Json, Router,
    body::Body,
    response::IntoResponse,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::RwLock;

pub async fn fake_services() -> (String, tokio::task::JoinHandle<()>) {
    let graph = Arc::new(RwLock::new(
        std::collections::BTreeMap::<String, String>::new(),
    ));
    let api = Router::new()
        .route("/health",get(||async{Json(json!({"ready":true,"queued":0}))}))
        .route("/models",get(||async{Json(json!({"data":[{"id":"fixture-model"}]}))}))
        .route("/embed",post(|Json(body):Json<Value>|async move {
            let vectors: Vec<_> = body["texts"].as_array().unwrap().iter().map(|text| {
                let mut vector=vec![0f32;1024];
                vector[usize::from(!text.as_str().unwrap().contains('住'))]=1.0;
                vector
            }).collect();
            Json(json!({"embeddings":vectors}))
        }))
        .route("/rerank",post(|Json(body):Json<Value>|async move {
            let scores:Vec<_>=body["documents"].as_array().unwrap().iter().map(|d|if d.as_str().unwrap().contains('住'){0.95}else{0.2}).collect();
            Json(json!({"scores":scores}))
        }))
        .route("/chat/completions",post(|Json(body):Json<Value>|async move {
            if body["stream"] == true {
                let messages = body["messages"].as_array().unwrap();
                let broken=messages.last().unwrap()["content"].as_str().unwrap().contains("断流");
                let recalls = messages.iter().filter_map(|m|m["content"].as_str()).collect::<Vec<_>>().join("\n");
                let answer=if recalls.contains("用户住在成都") {"你住在成都，我记得。"} else {"我会认真记住你告诉我的事情。"};
                let first=json!({"choices":[{"delta":{"content":answer},"finish_reason":null}]}).to_string();
                let finish=json!({"choices":[{"delta":{},"finish_reason":"stop"}]}).to_string();
                // Fragment UTF-8 and SSE frames across arbitrary network body chunks.
                let wire=if broken{format!("data: {first}\n\n")}else{format!("data: {first}\r\n\r\ndata: {finish}\n\ndata: [DONE]\n\n")};
                let chunks:Vec<_>=wire.into_bytes().chunks(7).map(|x|Ok::<_,std::convert::Infallible>(x.to_vec())).collect();
                ([("content-type","text/event-stream")],Body::from_stream(futures_util::stream::iter(chunks))).into_response()
            } else {
                let input:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
                let text=input["target"]["content"].as_str().unwrap();
                let mut facts=Vec::new();
                if text.contains("我住在成都") {
                    facts.push(json!({"key":"user.residence","content":"用户住在成都。","kind":"fact","importance":0.8,"confidence":0.98,"entities":["成都"],"source_quote":"我住在成都","valid_until":null,"sensitive":false,"equivalent_to":null}));
                }
                Json(json!({"choices":[{"message":{"content":json!({"memories":facts}).to_string()},"finish_reason":"stop"}]})).into_response()
            }
        }))
        .route("/db/neo4j/tx/commit",post(move |Json(body):Json<Value>| {
            let graph=graph.clone();
            async move {
                let statement=&body["statements"][0];
                let cypher=statement["statement"].as_str().unwrap();
                let p=&statement["parameters"];
                let mut nodes=graph.write().await;
                let rows:Vec<Value>=if cypher.starts_with("MERGE (m:Memory") {
                    nodes.insert(p["id"].as_str().unwrap().into(),p["content"].as_str().unwrap().into());
                    vec![json!({"row":[p["id"]]})]
                } else if cypher.contains("DETACH DELETE") {
                    if let Some(id) = p["id"].as_str() { nodes.remove(id); } else { nodes.clear(); }
                    vec![]
                } else if cypher.contains("queryNodes") || cypher.starts_with("MATCH (n:ActiveMemory)") || cypher.starts_with("MATCH (m:ActiveMemory)") {
                    nodes.keys().map(|id|json!({"row":[id,0.87]})).collect()
                } else { vec![] };
                Json(json!({"results":[{"data":rows}],"errors":[]}))
            }
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, api).await.unwrap() });
    (base, task)
}
