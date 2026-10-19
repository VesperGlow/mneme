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
                if messages.last().unwrap()["content"].as_str().unwrap().contains("__slow_chat__") {
                    tokio::time::sleep(std::time::Duration::from_millis(1800)).await;
                }
                let broken=messages.last().unwrap()["content"].as_str().unwrap().contains("断流");
                let recalls = messages.iter().filter_map(|m|m["content"].as_str()).collect::<Vec<_>>().join("\n");
                let answer=if recalls.contains("__markdown_chat__") {"**已整理**\n\n- 第一项\n- 第二项\n\n[参考资料](https://example.org/source) [危险链接](javascript:alert%281%29)\n\n```html\n<img src=x onerror=\"window.__answer_xss=1\">\n```\n\n<img src=x onerror=\"window.__answer_xss=1\">"} else if recalls.contains("用户住在成都") || messages.iter().take(messages.len()-1).any(|m|m["content"].as_str().is_some_and(|s|s.contains("我住在成都"))) {"你住在成都，我记得。"} else {"我会认真记住你告诉我的事情。"};
                let first=json!({"choices":[{"delta":{"content":answer},"finish_reason":null}]}).to_string();
                let finish=json!({"choices":[{"delta":{},"finish_reason":"stop"}]}).to_string();
                // Fragment UTF-8 and SSE frames across arbitrary network body chunks.
                let thinking=messages.last().unwrap()["content"].as_str().unwrap().contains("__reasoning_chat__");
                let usage=json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":50,"completion_tokens_details":{"reasoning_tokens":30},"total_tokens":150}});
                let reasoning=json!({"choices":[{"delta":{"reasoning_content":"再组织回答。"}}]});
                let wire=if broken{format!("data: {first}\n\n")}else if thinking {format!("data: {reasoning}\n\ndata: {first}\n\ndata: {finish}\n\ndata: {usage}\n\ndata: [DONE]\n\n")}else{format!("data: {first}\r\n\r\ndata: {finish}\n\ndata: [DONE]\n\n")};
                let chunks:Vec<_>=wire.into_bytes().chunks(7).map(|x|Ok::<_,std::convert::Infallible>(x.to_vec())).collect();
                let stream=async_stream::stream! {
                    if thinking {
                        let reasoning=json!({"choices":[{"delta":{"reasoning_content":"先核对信息。<img src=x onerror=\"window.__reasoning_xss=1\">"}}]});
                        yield Ok::<_,std::convert::Infallible>(format!("data: {reasoning}\n\n").into_bytes());
                        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                    }
                    for chunk in chunks { yield chunk; }
                };
                ([("content-type","text/event-stream")],Body::from_stream(stream)).into_response()
            } else {
                let input:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
                if input.get("max_chars").is_some() {
                    let mut summary = input["previous_summary"].as_str().unwrap_or("").to_owned();
                    if input["messages"].to_string().contains("__summary_fail__") {
                        return Json(json!({"choices":[{"message":{"content":"{}"},"finish_reason":"stop"}]})).into_response();
                    }
                    for message in input["messages"].as_array().unwrap() {
                        if message["role"] == "user" {
                            summary.push_str(message["content"].as_str().unwrap());
                            summary.push('。');
                        }
                    }
                    summary = summary.replace("我住在成都", "用户住在成都");
                    summary = summary.chars().take(input["max_chars"].as_u64().unwrap() as usize).collect();
                    return Json(json!({"choices":[{"message":{"content":json!({"summary":summary}).to_string()},"finish_reason":"stop"}]})).into_response();
                }
                let mut facts=Vec::new();
                let target = input["targets"].as_array().and_then(|targets| targets.iter().rev().find(|t|t["content"].as_str().unwrap().contains("我住在成都"))).unwrap_or(&input["target"]);
                if target["content"].as_str().unwrap_or("").contains("我住在成都") {
                    let mut fact=json!({"key":"user.residence","content":"用户住在成都。","kind":"fact","importance":0.8,"confidence":0.98,"entities":["成都"],"source_quote":"我住在成都","valid_until":null,"sensitive":false,"equivalent_to":null});
                    if input.get("targets").is_some() { fact["source_id"]=if target["content"].as_str().unwrap().contains("__bad_batch_source__") {json!("unknown-source")} else {target["id"].clone()}; }
                    facts.push(fact);
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
                } else if cypher.contains("queryNodes") || cypher.starts_with("MATCH (n:ActiveMemory)") || cypher.starts_with("MATCH (m:ActiveMemory)") || cypher.starts_with("UNWIND $seeds") {
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
