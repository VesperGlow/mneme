//! QQ 桥接：官方开放平台协议的 Rust 实现（原 Go/botgo 版的移植）。
//! 支持 WebSocket 与 HTTPS Webhook 两种事件模式，仅处理私聊 C2C。
//! AI 调用直接走进程内 Agent，不再经 HTTP 回环。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use ed25519_dalek::{Signer, Verifier};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

use crate::agent::Agent;
use crate::config::{Config, QqEventMode};
use crate::shutdown::{Listener, Pending};

const TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";
const API_BASE: &str = "https://api.sgroup.qq.com";
/// 群/C2C 消息事件的 intent 位（botgo IntentGroupMessages）。
const INTENT_GROUP_AND_C2C: u64 = 1 << 25;

// WS / Webhook 操作码（与官方协议一致）
const OP_DISPATCH: i64 = 0;
const OP_HEARTBEAT: i64 = 1;
const OP_IDENTIFY: i64 = 2;
const OP_RESUME: i64 = 6;
const OP_RECONNECT: i64 = 7;
const OP_INVALID_SESSION: i64 = 9;
const OP_HELLO: i64 = 10;
const OP_HEARTBEAT_ACK: i64 = 11;
const OP_CALLBACK_ACK: i64 = 12;
const OP_CALLBACK_VALIDATION: i64 = 13;

/// 与 botgo 相同的 ed25519 seed 派生：secret 自倍增到 >=32 字节后截断。
fn signing_key(secret: &str) -> Result<ed25519_dalek::SigningKey> {
    if secret.is_empty() {
        bail!("QQ_APP_SECRET 为空");
    }
    let mut seed = secret.to_string();
    while seed.len() < 32 {
        seed = seed.repeat(2);
    }
    let bytes: [u8; 32] = seed.as_bytes()[..32].try_into().unwrap();
    Ok(ed25519_dalek::SigningKey::from_bytes(&bytes))
}

fn verify_signature(secret: &str, timestamp: &str, body: &[u8], signature_hex: &str) -> bool {
    let Ok(key) = signing_key(secret) else { return false };
    let Ok(sig_bytes) = hex::decode(signature_hex) else { return false };
    let Ok(signature) = ed25519_dalek::Signature::from_slice(&sig_bytes) else { return false };
    let mut message = timestamp.as_bytes().to_vec();
    message.extend_from_slice(body);
    key.verifying_key().verify(&message, &signature).is_ok()
}

/// 长回复分片：与 Go 版 splitMessage 行为一致（前缀 (i/n)、超容截断标记）。
pub fn split_message(text: &str, max_runes: usize, max_parts: usize) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return vec!["（空回复）".to_string()];
    }
    let runes: Vec<char> = text.chars().collect();
    if runes.len() <= max_runes {
        return vec![text.to_string()];
    }
    let chunk_size = if max_runes > 16 { max_runes - 16 } else { max_runes };
    let marker: Vec<char> = "…（回复过长，已截断）".chars().collect();
    let capacity = chunk_size * max_parts;
    let mut runes = runes;
    if runes.len() > capacity {
        let keep = if capacity > marker.len() { capacity - marker.len() } else { capacity };
        runes.truncate(keep);
        runes.extend_from_slice(&marker);
    }
    let count = runes.len().div_ceil(chunk_size);
    runes
        .chunks(chunk_size)
        .enumerate()
        .map(|(index, chunk)| {
            format!("（{}/{count}）{}", index + 1, chunk.iter().collect::<String>())
        })
        .collect()
}

/// QQ OpenID 只以稳定哈希形式入库，不保存原始 OpenID。
pub fn stable_ids(sender_id: &str) -> (String, String) {
    let user_hash = Sha256::digest(format!("c2c\x00{sender_id}\x00{sender_id}").as_bytes());
    let convo_hash =
        Sha256::digest(format!("conversation\x00c2c\x00{sender_id}\x00{sender_id}").as_bytes());
    (
        format!("qq:c2c:{}", hex::encode(&user_hash[..16])),
        format!("qqc:c2c:{}", hex::encode(&convo_hash[..16])),
    )
}

fn sanitize_utf8(value: &str) -> String {
    // serde_json 解析出的字符串必为合法 UTF-8，这里只做占位对齐 Go 版行为。
    value.to_string()
}

#[derive(Debug, Clone)]
struct MessageJob {
    message_id: String,
    reply_target: String,
    user_id: String,
    conversation_id: String,
    content: String,
    has_attachments: bool,
    /// 图片附件的 CDN 下载地址（已截断到配置的张数上限）。
    image_urls: Vec<String>,
}

struct Deduper {
    entries: Mutex<HashMap<String, Instant>>,
    ttl: Duration,
}

impl Deduper {
    fn new(ttl: Duration) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    fn accept(&self, key: &str) -> bool {
        if key.is_empty() {
            return true;
        }
        let now = Instant::now();
        let mut entries = self.entries.lock().unwrap();
        if let Some(expires_at) = entries.get(key) {
            if now < *expires_at {
                return false;
            }
        }
        entries.insert(key.to_string(), now + self.ttl);
        if entries.len() > 2048 {
            entries.retain(|_, expires_at| now < *expires_at);
        }
        true
    }
}

/// 按会话 ID 互斥；引用计数归零即从 map 删除，避免历史会话永久占内存。
#[derive(Default)]
struct KeyedMutex {
    locks: Mutex<HashMap<String, (Arc<tokio::sync::Mutex<()>>, usize)>>,
}

impl KeyedMutex {
    async fn lock(&self, key: &str) -> KeyedGuard<'_> {
        let entry = {
            let mut locks = self.locks.lock().unwrap();
            let entry = locks
                .entry(key.to_string())
                .or_insert_with(|| (Arc::new(tokio::sync::Mutex::new(())), 0));
            entry.1 += 1;
            entry.0.clone()
        };
        let guard = entry.clone().lock_owned().await;
        KeyedGuard {
            owner: self,
            key: key.to_string(),
            _guard: guard,
        }
    }
}

struct KeyedGuard<'a> {
    owner: &'a KeyedMutex,
    key: String,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

impl Drop for KeyedGuard<'_> {
    fn drop(&mut self) {
        let mut locks = self.owner.locks.lock().unwrap();
        if let Some(entry) = locks.get_mut(&self.key) {
            entry.1 -= 1;
            if entry.1 == 0 {
                locks.remove(&self.key);
            }
        }
    }
}

/// Access Token 管理：过期前 60 秒主动刷新，互斥防并发重复刷。
struct TokenManager {
    cfg: Arc<Config>,
    http: reqwest::Client,
    cached: tokio::sync::Mutex<Option<(String, Instant)>>,
}

impl TokenManager {
    fn new(cfg: Arc<Config>, http: reqwest::Client) -> Self {
        Self {
            cfg,
            http,
            cached: tokio::sync::Mutex::new(None),
        }
    }

    async fn token(&self) -> Result<String> {
        let mut cached = self.cached.lock().await;
        if let Some((token, expiry)) = cached.as_ref() {
            if Instant::now() + Duration::from_secs(60) < *expiry {
                return Ok(token.clone());
            }
        }
        let response: Value = self
            .http
            .post(TOKEN_URL)
            .json(&json!({
                "appId": self.cfg.qq_app_id,
                "clientSecret": self.cfg.qq_app_secret,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let token = response["access_token"]
            .as_str()
            .filter(|t| !t.is_empty())
            .ok_or_else(|| anyhow!("获取 QQ Access Token 失败：{response}"))?
            .to_string();
        let expires_in: u64 = response["expires_in"]
            .as_str()
            .and_then(|v| v.parse().ok())
            .or_else(|| response["expires_in"].as_u64())
            .unwrap_or(7200);
        *cached = Some((token.clone(), Instant::now() + Duration::from_secs(expires_in)));
        Ok(token)
    }
}

pub struct QqBridge {
    cfg: Arc<Config>,
    agent: Agent,
    token: Arc<TokenManager>,
    http: reqwest::Client,
    deduper: Deduper,
    locks: Arc<KeyedMutex>,
    jobs_tx: mpsc::Sender<MessageJob>,
    jobs_rx: Mutex<Option<mpsc::Receiver<MessageJob>>>,
    shutdown: Listener,
    pending: Pending,
}

impl QqBridge {
    pub fn new(
        cfg: Arc<Config>,
        agent: Agent,
        shutdown: Listener,
        pending: Pending,
    ) -> Result<Arc<Self>> {
        if cfg.qq_app_id.is_empty() || cfg.qq_app_secret.is_empty() {
            bail!("QQ_APP_ID 和 QQ_APP_SECRET 不能为空");
        }
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.qq_openapi_timeout_seconds))
            .build()?;
        let (jobs_tx, jobs_rx) = mpsc::channel(cfg.qq_queue_size);
        Ok(Arc::new(Self {
            token: Arc::new(TokenManager::new(cfg.clone(), http.clone())),
            deduper: Deduper::new(Duration::from_secs(cfg.qq_dedup_ttl_seconds)),
            locks: Arc::new(KeyedMutex::default()),
            cfg,
            agent,
            http,
            jobs_tx,
            jobs_rx: Mutex::new(Some(jobs_rx)),
            shutdown,
            pending,
        }))
    }

    /// 启动 worker、HTTP 监听（healthz + 可选 webhook）、以及 websocket 模式下的网关连接。
    pub async fn run(self: Arc<Self>) -> Result<()> {
        // worker 池：从队列取消息、按会话互斥处理。
        let receiver = self
            .jobs_rx
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| anyhow!("QQ 桥接重复启动"))?;
        let receiver = Arc::new(tokio::sync::Mutex::new(receiver));
        for _ in 0..self.cfg.qq_workers {
            let bridge = self.clone();
            let receiver = receiver.clone();
            tokio::spawn(async move {
                loop {
                    // 停机信号后不再取新消息；正在处理的消息由 pending guard
                    // 保护，main 会等它连同回复一起做完。
                    let job = tokio::select! {
                        job = async { receiver.lock().await.recv().await } => job,
                        _ = bridge.shutdown.clone().wait() => break,
                    };
                    let Some(job) = job else { break };
                    let _pending = bridge.pending.guard();
                    let _guard = bridge.locks.lock(&job.conversation_id).await;
                    bridge.process(job).await;
                }
            });
        }

        // HTTP 端：healthz（两种模式都有）+ webhook 回调（仅 webhook 模式）。
        let listen = normalize_listen_addr(&self.cfg.qq_listen_addr);
        let mut router = Router::new().route(
            "/healthz",
            get({
                let mode = match self.cfg.qq_event_mode {
                    QqEventMode::Webhook => "webhook",
                    QqEventMode::WebSocket => "websocket",
                };
                move || async move { Json(json!({"status": "ok", "event_mode": mode})) }
            }),
        );
        if self.cfg.qq_event_mode == QqEventMode::Webhook {
            router = router
                .route(&self.cfg.qq_webhook_path, post(webhook_handler))
                .layer(axum::extract::DefaultBodyLimit::max(self.cfg.qq_max_webhook_bytes));
        }
        let router = router.with_state(self.clone());
        let listener = tokio::net::TcpListener::bind(&listen)
            .await
            .with_context(|| format!("QQ 桥接监听 {listen} 失败"))?;
        let graceful = self.shutdown.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(graceful.wait())
                .await
        });

        match self.cfg.qq_event_mode {
            QqEventMode::Webhook => {
                tracing::info!(
                    "QQ Bot Webhook 已启动: {listen}{} (私聊 C2C)",
                    self.cfg.qq_webhook_path
                );
                server.await??;
                Ok(())
            }
            QqEventMode::WebSocket => {
                tracing::info!("QQ Bot WebSocket 正在启动（健康检查: {listen}/healthz，私聊 C2C）");
                self.run_websocket().await
            }
        }
    }

    fn submit(&self, job: MessageJob) {
        if job.message_id.is_empty() || job.reply_target.is_empty() || job.user_id.is_empty() {
            tracing::warn!("忽略字段不完整的 QQ 消息: msg={:?}", job.message_id);
            return;
        }
        if !self.deduper.accept(&format!("c2c:{}", job.message_id)) {
            tracing::info!("忽略 QQ 重复事件: {}", job.message_id);
            return;
        }
        if self.jobs_tx.try_send(job).is_err() {
            tracing::warn!("QQ 消息队列已满，丢弃消息");
        }
    }

    async fn process(&self, job: MessageJob) {
        let content = job.content.trim().to_string();
        let received_at = std::time::Instant::now();
        tracing::info!(
            "收到 QQ 消息 msg={} 文字{}字 图片{}张",
            job.message_id,
            content.chars().count(),
            job.image_urls.len(),
        );

        // 下载图片附件转 data URI；单张失败只记日志，尽量把拿到的图带上。
        let mut images: Vec<String> = Vec::new();
        for url in &job.image_urls {
            match self.fetch_image(url).await {
                Ok(uri) => images.push(uri),
                Err(error) => {
                    tracing::warn!("下载 QQ 图片附件失败: msg={} err={error:#}", job.message_id)
                }
            }
        }

        if content.is_empty() && images.is_empty() {
            let hint = if !job.image_urls.is_empty() {
                "刚才的图片我没能加载出来，可以再发一次吗？"
            } else if job.has_attachments {
                "我目前只能处理文字和图片，这类附件暂时还看不了。"
            } else {
                return;
            };
            let _ = self.send_text(&job, hint).await;
            return;
        }
        let reply = tokio::time::timeout(
            Duration::from_secs(self.cfg.qq_ai_timeout_seconds),
            self.agent.chat(
                &job.user_id,
                &content,
                Some(job.conversation_id.clone()),
                None,
                &images,
            ),
        )
        .await;
        match reply {
            Ok(Ok(result)) => match self.send_text(&job, &result.content).await {
                Ok(()) => tracing::info!(
                    "已回复 QQ 消息 msg={} {}字 全程{:.1}s",
                    job.message_id,
                    result.content.chars().count(),
                    received_at.elapsed().as_secs_f32(),
                ),
                Err(error) => {
                    tracing::warn!("回复 QQ 消息失败: msg={} err={error:#}", job.message_id)
                }
            },
            Ok(Err(error)) => {
                tracing::warn!("AI 处理 QQ 消息失败: msg={} err={error:#}", job.message_id);
                let _ = self.send_text(&job, "这次处理失败了，请稍后再试。").await;
            }
            Err(_) => {
                tracing::warn!("AI 处理 QQ 消息超时: msg={}", job.message_id);
                let _ = self.send_text(&job, "这次处理失败了，请稍后再试。").await;
            }
        }
    }

    /// 从 QQ CDN 下载图片并转成 data URI；超过配置的大小上限直接放弃。
    async fn fetch_image(&self, url: &str) -> Result<String> {
        let response = self.http.get(url).send().await?.error_for_status()?;
        let limit = self.cfg.chat_image_max_bytes;
        if let Some(length) = response.content_length() {
            if length as usize > limit {
                bail!("图片 {length} 字节，超过上限 {limit}");
            }
        }
        let bytes = response.bytes().await?;
        if bytes.is_empty() {
            bail!("图片内容为空");
        }
        if bytes.len() > limit {
            bail!("图片 {} 字节，超过上限 {limit}", bytes.len());
        }
        tracing::debug!("QQ 图片下载完成 {} 字节", bytes.len());
        Ok(crate::image::to_data_uri(&bytes))
    }

    async fn send_text(&self, job: &MessageJob, text: &str) -> Result<()> {
        let parts = split_message(text, self.cfg.qq_reply_max_runes, self.cfg.qq_reply_max_parts);
        let total = parts.len();
        for (index, part) in parts.into_iter().enumerate() {
            let token = self.token.token().await?;
            let response = self
                .http
                .post(format!(
                    "{API_BASE}/v2/users/{}/messages",
                    job.reply_target
                ))
                .header("Authorization", format!("QQBot {token}"))
                .json(&json!({
                    "content": part,
                    "msg_type": 0,
                    "msg_id": job.message_id,
                    "msg_seq": index + 1,
                }))
                .send()
                .await?;
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                bail!("QQ 发消息失败 HTTP {status}：{}", &body.chars().take(500).collect::<String>());
            }
            if index + 1 < total {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
        Ok(())
    }

    fn handle_dispatch(&self, payload: &Value) {
        let event_type = payload["t"].as_str().unwrap_or("");
        if event_type != "C2C_MESSAGE_CREATE" {
            return;
        }
        let data = &payload["d"];
        let author = &data["author"];
        if author["bot"].as_bool().unwrap_or(false) {
            return;
        }
        let sender_id = author["user_openid"]
            .as_str()
            .or_else(|| author["id"].as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let message_id = data["id"].as_str().unwrap_or("").trim().to_string();
        let content = sanitize_utf8(data["content"].as_str().unwrap_or(""));
        let attachments = data["attachments"].as_array().cloned().unwrap_or_default();
        let has_attachments = !attachments.is_empty();
        // 只取图片类附件；QQ 的 url 字段可能不带协议前缀。
        let image_urls: Vec<String> = if self.cfg.chat_image_enabled {
            attachments
                .iter()
                .filter(|item| {
                    // 平台一般给 image/jpeg 等完整 MIME，个别场景只给 "image"。
                    item["content_type"]
                        .as_str()
                        .unwrap_or("")
                        .starts_with("image")
                })
                .filter_map(|item| item["url"].as_str())
                .map(|url| {
                    if url.starts_with("http://") || url.starts_with("https://") {
                        url.to_string()
                    } else {
                        format!("https://{url}")
                    }
                })
                .take(self.cfg.chat_image_max_count)
                .collect()
        } else {
            Vec::new()
        };
        let (user_id, conversation_id) = stable_ids(&sender_id);
        self.submit(MessageJob {
            message_id,
            reply_target: sender_id,
            user_id,
            conversation_id,
            content,
            has_attachments,
            image_urls,
        });
    }

    // ---------- WebSocket 模式 ----------

    async fn run_websocket(self: &Arc<Self>) -> Result<()> {
        let mut session: Option<(String, u64)> = None; // (session_id, last_seq)
        let mut backoff = 1u64;
        loop {
            match self.websocket_once(&mut session).await {
                Ok(()) => backoff = 1,
                Err(error) => {
                    tracing::warn!("QQ WebSocket 连接异常，{backoff}s 后重连: {error:#}");
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    backoff = (backoff * 2).min(60);
                }
            }
        }
    }

    async fn websocket_once(self: &Arc<Self>, session: &mut Option<(String, u64)>) -> Result<()> {
        let token = self.token.token().await?;
        let gateway: Value = self
            .http
            .get(format!("{API_BASE}/gateway"))
            .header("Authorization", format!("QQBot {token}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let url = gateway["url"]
            .as_str()
            .ok_or_else(|| anyhow!("网关响应缺少 url：{gateway}"))?;

        let (stream, _) = tokio_tungstenite::connect_async(url).await?;
        let (mut sink, mut source) = stream.split();

        let mut heartbeat_interval = Duration::from_secs(40);
        let mut heartbeat = tokio::time::interval(heartbeat_interval);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut identified = false;

        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    if identified {
                        let seq = session.as_ref().map(|(_, s)| *s).unwrap_or(0);
                        let frame = json!({"op": OP_HEARTBEAT, "d": if seq == 0 { Value::Null } else { json!(seq) }});
                        sink.send(tokio_tungstenite::tungstenite::Message::text(frame.to_string())).await?;
                    }
                }
                frame = source.next() => {
                    let Some(frame) = frame else { bail!("QQ WebSocket 连接被关闭") };
                    let frame = frame?;
                    let text = match frame {
                        tokio_tungstenite::tungstenite::Message::Text(text) => text.to_string(),
                        tokio_tungstenite::tungstenite::Message::Close(reason) => {
                            bail!("QQ WebSocket 服务端关闭：{reason:?}")
                        }
                        tokio_tungstenite::tungstenite::Message::Ping(data) => {
                            sink.send(tokio_tungstenite::tungstenite::Message::Pong(data)).await?;
                            continue;
                        }
                        _ => continue,
                    };
                    let payload: Value = match serde_json::from_str(&text) {
                        Ok(payload) => payload,
                        Err(error) => {
                            tracing::warn!("解析 QQ WebSocket 帧失败：{error}");
                            continue;
                        }
                    };
                    if let Some(seq) = payload["s"].as_u64() {
                        if let Some((_, last)) = session.as_mut() {
                            *last = seq;
                        }
                    }
                    match payload["op"].as_i64().unwrap_or(-1) {
                        OP_HELLO => {
                            if let Some(interval) = payload["d"]["heartbeat_interval"].as_u64() {
                                heartbeat_interval = Duration::from_millis(interval);
                                heartbeat = tokio::time::interval(heartbeat_interval);
                                heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                            }
                            let token = self.token.token().await?;
                            let frame = if let Some((session_id, seq)) = session.as_ref() {
                                // 尝试恢复上次会话，失败时服务端会回 op9。
                                json!({"op": OP_RESUME, "d": {
                                    "token": format!("QQBot {token}"),
                                    "session_id": session_id,
                                    "seq": seq,
                                }})
                            } else {
                                json!({"op": OP_IDENTIFY, "d": {
                                    "token": format!("QQBot {token}"),
                                    "intents": INTENT_GROUP_AND_C2C,
                                    "shard": [0, 1],
                                    "properties": {},
                                }})
                            };
                            sink.send(tokio_tungstenite::tungstenite::Message::text(frame.to_string())).await?;
                            identified = true;
                        }
                        OP_DISPATCH => {
                            let event_type = payload["t"].as_str().unwrap_or("");
                            match event_type {
                                "READY" => {
                                    let session_id = payload["d"]["session_id"].as_str().unwrap_or("").to_string();
                                    let shard = &payload["d"]["shard"];
                                    tracing::info!("QQ WebSocket 已连接 (shard={shard})");
                                    *session = Some((session_id, payload["s"].as_u64().unwrap_or(0)));
                                }
                                "RESUMED" => {
                                    tracing::info!("QQ WebSocket 会话已恢复");
                                }
                                _ => self.handle_dispatch(&payload),
                            }
                        }
                        OP_RECONNECT => bail!("服务端要求重连"),
                        OP_INVALID_SESSION => {
                            tracing::warn!("QQ WebSocket 会话失效，将重新 identify");
                            *session = None;
                            bail!("会话失效");
                        }
                        OP_HEARTBEAT_ACK => {}
                        other => tracing::debug!("忽略未知 QQ WebSocket op={other}"),
                    }
                }
            }
        }
    }
}

fn normalize_listen_addr(addr: &str) -> String {
    if let Some(port) = addr.strip_prefix(':') {
        format!("0.0.0.0:{port}")
    } else {
        addr.to_string()
    }
}

// ---------- Webhook 模式 ----------

async fn webhook_handler(
    State(bridge): State<Arc<QqBridge>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> (StatusCode, String) {
    let secret = &bridge.cfg.qq_app_secret;
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(_) => return (StatusCode::BAD_REQUEST, String::new()),
    };
    let op = payload["op"].as_i64().unwrap_or(-1);

    // 回调地址校验：用 event_ts + plain_token 签名返回。
    if op == OP_CALLBACK_VALIDATION {
        let plain_token = payload["d"]["plain_token"].as_str().unwrap_or("");
        let event_ts = payload["d"]["event_ts"].as_str().unwrap_or("");
        let Ok(key) = signing_key(secret) else {
            return (StatusCode::INTERNAL_SERVER_ERROR, String::new());
        };
        let mut message = event_ts.as_bytes().to_vec();
        message.extend_from_slice(plain_token.as_bytes());
        let signature = hex::encode(key.sign(&message).to_bytes());
        return (
            StatusCode::OK,
            json!({"plain_token": plain_token, "signature": signature}).to_string(),
        );
    }

    // 事件推送：先验签再处理。
    let signature = headers
        .get("X-Signature-Ed25519")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let timestamp = headers
        .get("X-Signature-Timestamp")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !verify_signature(secret, timestamp, &body, signature) {
        tracing::warn!("QQ Webhook 签名验证失败");
        return (StatusCode::FORBIDDEN, String::new());
    }
    match op {
        OP_HEARTBEAT => {
            let seq = payload["d"].as_u64().unwrap_or(0);
            (StatusCode::OK, json!({"op": OP_HEARTBEAT_ACK, "d": seq}).to_string())
        }
        OP_DISPATCH => {
            bridge.handle_dispatch(&payload);
            (StatusCode::OK, json!({"op": OP_CALLBACK_ACK, "d": 0}).to_string())
        }
        _ => (StatusCode::OK, String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_message_kept_whole() {
        assert_eq!(split_message("你好", 1800, 4), vec!["你好"]);
    }

    #[test]
    fn long_message_split_with_index_prefix() {
        let text = "啊".repeat(2000);
        let parts = split_message(&text, 1800, 4);
        assert_eq!(parts.len(), 2);
        assert!(parts[0].starts_with("（1/2）"));
        assert!(parts[1].starts_with("（2/2）"));
    }

    #[test]
    fn overlong_message_truncated_with_marker() {
        let text = "啊".repeat(10000);
        let parts = split_message(&text, 1000, 2);
        assert_eq!(parts.len(), 2);
        assert!(parts[1].contains("已截断"));
    }

    #[test]
    fn stable_ids_deterministic_and_hashed() {
        let (user1, convo1) = stable_ids("openid-abc");
        let (user2, _) = stable_ids("openid-abc");
        assert_eq!(user1, user2);
        assert!(user1.starts_with("qq:c2c:"));
        assert!(convo1.starts_with("qqc:c2c:"));
        assert!(!user1.contains("openid-abc"));
        assert_eq!(user1.len(), "qq:c2c:".len() + 32);
    }

    #[test]
    fn signature_roundtrip() {
        let secret = "0123456789abcdef";
        let key = signing_key(secret).unwrap();
        let mut message = b"1700000000".to_vec();
        message.extend_from_slice(b"{\"op\":0}");
        let signature = hex::encode(key.sign(&message).to_bytes());
        assert!(verify_signature(secret, "1700000000", b"{\"op\":0}", &signature));
        assert!(!verify_signature(secret, "1700000001", b"{\"op\":0}", &signature));
    }

    #[test]
    fn deduper_rejects_within_ttl() {
        let deduper = Deduper::new(Duration::from_secs(60));
        assert!(deduper.accept("m1"));
        assert!(!deduper.accept("m1"));
        assert!(deduper.accept("m2"));
    }
}
