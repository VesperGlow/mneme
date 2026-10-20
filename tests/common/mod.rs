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
                let mut answer = answer.to_owned();
                if recalls.contains("__sticker_chat__")
                    && let Some(sticker) = messages[0]["content"].as_str().and_then(|s| s.split("\"id\":\"").nth(1)).and_then(|s| s.split('"').next()) {
                    answer.push_str(&format!("[[sticker:{sticker}]]"));
                }
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
                if body["messages"][1]["content"].is_array() {
                    return Json(json!({"choices":[{"message":{"content":json!({"name":"开心表情","description":"角色兴奋庆祝，适合开心回应或表达祝贺。"}).to_string()},"finish_reason":"stop"}],"usage":{"prompt_tokens":1100,"completion_tokens":40,"total_tokens":1140}})).into_response();
                }
                let input:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
                if input["evolution_request"]==true {
                    let source=&input["sources"][0];
                    let notes=if input["notes"].as_array().is_some_and(|n|!n.is_empty()){input["notes"].clone()}else{json!([{"id":"quiet_chat","area":"communication","text":"聊天留意对方的具体分享，避免反复追加无关追问。","evidence":[{"source_id":source["id"],"quote":source["content"].as_str().unwrap().chars().take(160).collect::<String>()}]}])};
                    return Json(json!({"choices":[{"message":{"content":json!({"action":"revise","reason":"根据真实用户反馈调整沟通习惯。","notes":notes}).to_string()},"finish_reason":"stop"}]})).into_response();
                }
                if input["evolution_validation"]==true {
                    let result=json!({"core_consistent":true,"evidence_supported":true,"low_risk":true,"natural":true,"reason":"测试夹具：核对候选、证据和基础能力。","replies":[{"id":"chat","reply":"猫大概睡得很舒服。"},{"id":"emotion","reply":"陪你待会儿。"},{"id":"tech","reply":"42"},{"id":"unknown","reply":"UNKNOWN"},{"id":"tool","reply":"UNVERIFIED"}]});
                    return Json(json!({"choices":[{"message":{"content":result.to_string()},"finish_reason":"stop"}]})).into_response();
                }
                if let Some(persona)=input.get("persona") {
                    let style=persona["style"].as_str().unwrap();
                    let quote:String=style.chars().take(80).collect();
                    let dimensions:Vec<_>=["overall","chat","emotion","task","tools","memory"].iter().map(|id|json!({"id":id,"description":format!("{id}：{}",style.chars().take(200).collect::<String>()),"basis":"来自原始交流方式的测试衍生，不添加角色经历。","evidence":[{"field":"style","quote":quote}],"signals":["参考倾向"]})).collect();
                    return Json(json!({"choices":[{"message":{"content":json!({"dimensions":dimensions}).to_string()},"finish_reason":"stop"}]})).into_response();
                }
                if input.get("max_tokens").is_some() {
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
                    summary = mneme::tokens::Counter::default().truncate(&summary, input["max_tokens"].as_u64().unwrap() as usize);
                    return Json(json!({"choices":[{"message":{"content":json!({"summary":summary}).to_string()},"finish_reason":"stop"}]})).into_response();
                }
                let mut facts=Vec::new();
                let target = input["targets"].as_array().and_then(|targets| targets.iter().rev().find(|t|t["content"].as_str().unwrap().contains("我住在成都"))).unwrap_or(&input["target"]);
                if target["content"].as_str().unwrap_or("").contains("我住在成都") {
                    let mut fact=json!({"key":"user.residence","content":"用户住在成都。","kind":"fact","importance":0.8,"confidence":0.98,"entities":["成都"],"source_quote":"我住在成都","valid_until":null,"sensitive":false,"equivalent_to":null});
                    if input.get("targets").is_some() { fact["source_id"]=if target["content"].as_str().unwrap().contains("__bad_batch_source__") {json!("unknown-source")} else {target["id"].clone()}; }
                    facts.push(fact);
                }
                let mut result=json!({"memories":facts});
                if let Some(targets)=input["targets"].as_array(){result["processed_source_ids"]=json!(targets.iter().map(|t|&t["id"]).collect::<Vec<_>>());}
                Json(json!({"choices":[{"message":{"content":result.to_string()},"finish_reason":"stop"}]})).into_response()
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
