//! Official QQ C2C Webhook adapter. No Go runtime, gateway WebSocket or public Console.
use crate::{
    App,
    api::{ApiError, ApiResult},
    config::QqConfig,
    domain::now,
    llm::bounded_body,
    qq_store::{Delivery, Incoming},
    store::BeginTurn,
};
use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    routing::{get, post},
};
use ed25519_dalek::{Signature, Signer, SigningKey};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, mpsc};

#[derive(Clone)]
pub struct Client {
    config: QqConfig,
    http: reqwest::Client,
    token: Arc<Mutex<Option<Token>>>,
}
struct Token {
    value: String,
    expires: Instant,
}
impl Client {
    pub fn new(config: &QqConfig) -> Result<Self> {
        Ok(Self {
            config: config.clone(),
            token: Default::default(),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }
    async fn access_token(&self) -> Result<String> {
        let mut cached = self.token.lock().await;
        if let Some(token) = cached
            .as_ref()
            .filter(|t| t.expires > Instant::now() + Duration::from_secs(60))
        {
            return Ok(token.value.clone());
        }
        let response = self
            .http
            .post(&self.config.token_url)
            .json(&json!({"appId":self.config.app_id,"clientSecret":self.config.app_secret}))
            .send()
            .await?;
        if !response.status().is_success() {
            tracing::warn!(
                http_status = response.status().as_u16(),
                stage = "token",
                "QQ API request rejected"
            );
            anyhow::bail!("QQ token HTTP error");
        }
        let data: Value = serde_json::from_slice(&bounded_body(response, 65536).await?)?;
        if data.get("code").is_some_and(|c| c.as_i64() != Some(0)) {
            tracing::warn!(
                code = data["code"].as_i64(),
                stage = "token",
                "QQ API returned an error"
            );
        }
        ensure!(
            data.get("code").is_none_or(|c| c.as_i64() == Some(0)),
            "QQ token API error"
        );
        let value = data["access_token"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 8192)
            .ok_or_else(|| anyhow::anyhow!("QQ token missing"))?;
        let seconds = data["expires_in"]
            .as_u64()
            .or_else(|| data["expires_in"].as_str()?.parse().ok())
            .unwrap_or(0);
        ensure!((1..=86400).contains(&seconds), "QQ token expiry invalid");
        *cached = Some(Token {
            value: value.into(),
            expires: Instant::now() + Duration::from_secs(seconds),
        });
        Ok(value.into())
    }
    async fn send(
        &self,
        item: &Delivery,
        text: &str,
        sequence: usize,
    ) -> std::result::Result<(), bool> {
        for attempt in 0..2 {
            let token = self.access_token().await.map_err(|_| false)?;
            let response = self.http.post(format!("{}/v2/users/{}/messages",self.config.api_base.trim_end_matches('/'),item.user_openid))
                .header(header::AUTHORIZATION,format!("QQBot {token}"))
                .header("X-Union-Appid",&self.config.app_id)
                .json(&json!({"msg_type":0,"content":text,"msg_id":item.message_id,"msg_seq":sequence+1}))
                .send().await.map_err(|_|false)?;
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                *self.token.lock().await = None;
                continue;
            }
            let status = response.status();
            if !status.is_success() {
                tracing::warn!(
                    http_status = status.as_u16(),
                    stage = "send",
                    "QQ API request rejected"
                );
                return Err(status.is_client_error()
                    && status != StatusCode::TOO_MANY_REQUESTS
                    && status != StatusCode::UNAUTHORIZED);
            }
            let bytes = bounded_body(response, 65536).await.map_err(|_| false)?;
            let data: Value = serde_json::from_slice(&bytes).map_err(|_| false)?;
            if data.get("code").is_some_and(|c| c.as_i64() != Some(0)) {
                tracing::warn!(
                    code = data["code"].as_i64(),
                    stage = "send",
                    "QQ API returned an error"
                );
                return Err(false);
            }
            return Ok(());
        }
        Err(false)
    }
}

pub fn signing_key(secret: &str) -> SigningKey {
    // BotGo repeats the raw AppSecret bytes and takes the first 32 bytes as the seed.
    let mut seed = [0u8; 32];
    if !secret.is_empty() {
        for (i, byte) in seed.iter_mut().enumerate() {
            *byte = secret.as_bytes()[i % secret.len()];
        }
    }
    SigningKey::from_bytes(&seed)
}
pub fn sign(secret: &str, timestamp: &str, body: &[u8]) -> String {
    let mut data = timestamp.as_bytes().to_vec();
    data.extend_from_slice(body);
    signing_key(secret)
        .sign(&data)
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn verify(secret: &str, timestamp: &str, body: &[u8], hex: &str) -> bool {
    if secret.is_empty() || hex.len() != 128 {
        return false;
    }
    let mut bytes = [0u8; 64];
    for (i, byte) in bytes.iter_mut().enumerate() {
        let Some(value) = hex
            .get(i * 2..i * 2 + 2)
            .and_then(|s| u8::from_str_radix(s, 16).ok())
        else {
            return false;
        };
        *byte = value;
    }
    let mut data = timestamp.as_bytes().to_vec();
    data.extend_from_slice(body);
    signing_key(secret)
        .verifying_key()
        .verify_strict(&data, &Signature::from_bytes(&bytes))
        .is_ok()
}

pub fn router(app: App) -> Router {
    Router::new()
        .route("/qqbot", post(webhook))
        .layer(DefaultBodyLimit::max(65536))
        .with_state(app)
}
pub fn protected_routes() -> Router<App> {
    Router::new().route("/v1/qq/status", get(status))
}
async fn status(State(app): State<App>) -> ApiResult<Json<Value>> {
    let mut data = app.store.qq_status(&app.config.qq.app_id).await?;
    data["enabled"] = json!(app.qq.is_some());
    data["app_id"] = json!(app.config.qq.app_id);
    data["callback_path"] = json!("/qqbot");
    Ok(Json(data))
}
fn header_text<'a>(headers: &'a HeaderMap, key: &str) -> &'a str {
    headers.get(key).and_then(|v| v.to_str().ok()).unwrap_or("")
}
async fn webhook(
    State(app): State<App>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let config = &app.config.qq;
    if app.qq.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "qq_disabled"));
    }
    let timestamp = header_text(&headers, "X-Signature-Timestamp");
    if header_text(&headers, "X-Bot-Appid") != config.app_id
        || timestamp
            .parse::<i64>()
            .ok()
            .is_none_or(|ts| now().abs_diff(ts) > 300)
        || !verify(
            &config.app_secret,
            timestamp,
            &body,
            header_text(&headers, "X-Signature-Ed25519"),
        )
    {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "invalid_qq_signature"));
    }
    let payload: Value = serde_json::from_slice(&body)
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_qq_payload"))?;
    match payload["op"].as_i64() {
        Some(13) => {
            let token = payload["d"]["plain_token"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 1024)
                .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_qq_challenge"))?;
            let ts = payload["d"]["event_ts"]
                .as_str()
                .filter(|s| s.len() <= 32)
                .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_qq_challenge"))?;
            Ok(Json(
                json!({"plain_token":token,"signature":sign(&config.app_secret,ts,token.as_bytes())}),
            ))
        }
        Some(1) => Ok(Json(json!({"op":11,"d":payload["d"]}))),
        Some(0) => {
            if payload["t"] == "C2C_MESSAGE_CREATE" {
                let data = &payload["d"];
                let message_id = data["id"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= 512)
                    .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_qq_message"))?;
                let sender = data["author"]["user_openid"]
                    .as_str()
                    .or_else(|| data["author"]["id"].as_str())
                    .filter(|s| {
                        !s.is_empty()
                            && s.len() <= 128
                            && s.bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    })
                    .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_qq_sender"))?;
                if data["author"]["bot"] != true {
                    let sent_at = data["timestamp"]
                        .as_str()
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|ts| ts.timestamp())
                        .unwrap_or_else(now);
                    let expires_at = sent_at.min(now()) + 290;
                    if expires_at > now() {
                        match app
                            .store
                            .qq_accept(
                                &config.app_id,
                                Incoming {
                                    message_id: message_id.into(),
                                    sender: sender.into(),
                                    content: data["content"].as_str().unwrap_or("").into(),
                                    expires_at,
                                },
                            )
                            .await
                        {
                            Ok(outcome) => {
                                tracing::info!(outcome=?outcome, "QQ private message received")
                            }
                            Err(_) => {
                                tracing::error!(
                                    "QQ event was not committed; requesting platform redelivery"
                                );
                                return Ok(Json(json!({"op":12,"d":1})));
                            }
                        }
                    }
                }
            }
            Ok(Json(json!({"op":12,"d":0})))
        }
        _ => Err(ApiError(StatusCode::BAD_REQUEST, "unsupported_qq_opcode")),
    }
}

fn chunks(reply: &str) -> Vec<String> {
    let chars: Vec<_> = reply.chars().take(6001).collect();
    let mut result: Vec<String> = chars[..chars.len().min(6000)]
        .chunks(1500)
        .map(|c| c.iter().collect())
        .collect();
    if chars.len() > 6000 {
        result
            .last_mut()
            .unwrap()
            .push_str("\n（回复过长，完整内容已保存在 Mneme。）");
    }
    result
}

async fn process(
    app: &App,
    client: &Client,
    item: &Delivery,
) -> Result<std::result::Result<(), bool>> {
    if now() >= item.expires_at {
        return Ok(Err(true));
    }
    let reply = if let Some(reply) = &item.reply {
        reply.clone()
    } else {
        let permit = app.generation_slots.clone().acquire_owned().await?;
        if now() >= item.expires_at {
            return Ok(Err(true));
        }
        let reply = match app
            .store
            .begin_turn(&item.session_id, &item.id, &item.content)
            .await?
        {
            BeginTurn::Started(user) => {
                let (output, receiver) = mpsc::channel(1);
                drop(receiver);
                match crate::chat::generate(app, &user, &output).await {
                    Ok(reply) => reply,
                    Err(_) => {
                        tracing::warn!(delivery=%item.id, "QQ reply generation failed");
                        app.store.fail_turn(&user, "generation_failed").await?;
                        "这次回复没有完成，请稍后重新发送消息。".into()
                    }
                }
            }
            BeginTurn::Existing { status, content } if status == "complete" => {
                content.unwrap_or_default()
            }
            BeginTurn::Busy => return Ok(Err(false)),
            _ => "上次回复被中断，请重新发送消息。我已经保存的对话和记忆仍在。".into(),
        };
        drop(permit);
        app.store.qq_reply(&item.id, &reply).await?;
        reply
    };
    for (index, part) in chunks(&reply)
        .iter()
        .enumerate()
        .skip(item.next_chunk as usize)
    {
        if now() >= item.expires_at {
            return Ok(Err(true));
        }
        if let Err(permanent) = client.send(item, part, index).await {
            return Ok(Err(permanent));
        }
        app.store.qq_chunk(&item.id, index as i64 + 1).await?;
    }
    app.store.qq_sent(&item.id).await?;
    Ok(Ok(()))
}

pub async fn worker(app: App) {
    let Some(client) = app.qq.clone() else {
        return;
    };
    let mut shutdown = app.shutdown.clone();
    while !*shutdown.borrow() {
        let item = match app.store.qq_claim(&app.config.qq.app_id).await {
            Ok(Some(item)) => item,
            Ok(None) => {
                tokio::select! {_ = tokio::time::sleep(Duration::from_millis(250))=>{},_ = shutdown.changed()=>{}};
                continue;
            }
            Err(_) => {
                tracing::error!("QQ queue read failed");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let result =
            tokio::select! { r=process(&app,&client,&item)=>r,_=shutdown.changed()=>break };
        if matches!(result, Ok(Ok(()))) {
            tracing::info!(delivery=%item.id, "QQ reply sent");
        } else {
            let permanent = matches!(result, Ok(Err(true)));
            if app
                .store
                .qq_failed(
                    &item,
                    if permanent {
                        "qq_rejected_or_expired"
                    } else {
                        "qq_delivery_failed"
                    },
                    permanent,
                )
                .await
                .is_err()
            {
                tracing::error!(delivery=%item.id,"QQ queue update failed");
            }
            tracing::warn!(delivery=%item.id,"QQ delivery deferred or dead; inspect /v1/qq/status");
        }
    }
}
