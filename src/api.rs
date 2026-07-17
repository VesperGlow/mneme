//! HTTP API（axum）：与 Python 版 FastAPI 路由一一对应。

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent::Agent;
use crate::config::Config;
use crate::store::{EntityView, NewMemory};

static INDEX_HTML: &str = include_str!("../static/index.html");

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub agent: Agent,
}

struct ApiError {
    status: StatusCode,
    detail: String,
}

impl ApiError {
    fn new(status: StatusCode, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: detail.into(),
        }
    }

    fn internal(error: anyhow::Error) -> Self {
        tracing::error!("请求处理失败：{error:#}");
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"detail": self.detail}))).into_response()
    }
}

fn require_api_key(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    if state.cfg.app_api_key.is_empty() {
        return Ok(());
    }
    let expected = format!("Bearer {}", state.cfg.app_api_key);
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if provided != expected {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "缺少或无效的 APP_API_KEY",
        ));
    }
    Ok(())
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health/live", get(health_live))
        .route("/health", get(health))
        .route("/v1/config", get(config_endpoint))
        .route("/v1/chat", post(chat))
        .route("/v1/memories", post(create_memory))
        .route("/v1/memories/search", get(search_memories))
        .route("/v1/memories/recent", get(recent_memories))
        .route("/v1/memories/link", post(link_memories))
        .route("/v1/memories/{memory_id}", delete(forget_memory))
        .route("/v1/memories/{memory_id}/history", get(memory_history))
        .route("/v1/mood/{user_id}", get(mood_timeline))
        .route("/v1/graph/{user_id}", get(graph))
        .with_state(state)
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn health_live() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    let database_ok = state.agent.store().ping().await;
    let embedding_ok = state.agent.embedder().ready();
    let llm_configured = !state.cfg.ai_base_url.is_empty()
        && !state.cfg.chat_model.is_empty()
        && !state.cfg.memory_model.is_empty();
    let status = if database_ok && embedding_ok && llm_configured {
        "ok"
    } else {
        "degraded"
    };
    Json(json!({
        "status": status,
        "database": database_ok,
        "embedding": embedding_ok,
        "llm_configured": llm_configured,
        "mcp_tools": state.agent_mcp_tool_count(),
        "config": state.cfg.safe_summary(),
    }))
}

impl AppState {
    fn agent_mcp_tool_count(&self) -> usize {
        self.agent.mcp_tool_count()
    }
}

async fn config_endpoint(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_api_key(&state, &headers)?;
    Ok(Json(state.cfg.safe_summary()))
}

#[derive(Deserialize)]
struct ChatRequest {
    user_id: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    conversation_id: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    /// 图片列表：裸 base64、data URI 或 http(s) URL；需 CHAT_MODEL 支持视觉。
    #[serde(default)]
    images: Vec<String>,
}

async fn chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ChatRequest>,
) -> Result<Json<Value>, ApiError> {
    require_api_key(&state, &headers)?;
    validate_len("user_id", &body.user_id, 1, 128)?;
    // 带图片时允许 message 为空（纯图片消息）。
    let message_min = if body.images.is_empty() { 1 } else { 0 };
    validate_len("message", &body.message, message_min, 200_000)?;
    if !body.images.is_empty() && !state.cfg.chat_image_enabled {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "图片输入未启用（CHAT_IMAGE_ENABLED=false）",
        ));
    }
    if body.images.len() > state.cfg.chat_image_max_count {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("images 最多 {} 张", state.cfg.chat_image_max_count),
        ));
    }
    let images: Vec<String> = body
        .images
        .iter()
        .map(|raw| crate::image::normalize_input(raw, state.cfg.chat_image_max_bytes))
        .collect::<Result<_, _>>()
        .map_err(|error| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()))?;
    let result = state
        .agent
        .chat(
            &body.user_id,
            &body.message,
            body.conversation_id,
            body.system_prompt,
            &images,
        )
        .await
        .map_err(|error| {
            if let Some(llm) = error.downcast_ref::<crate::llm::LlmError>() {
                ApiError::new(StatusCode::BAD_GATEWAY, llm.to_string())
            } else {
                ApiError::internal(error)
            }
        })?;
    Ok(Json(json!({
        "conversation_id": result.conversation_id,
        "message": result.content,
        "retrieved_memories": result.retrieved,
        "saved_memories": result.saved,
        "tool_events": result.tool_events,
        "warnings": result.warnings,
    })))
}

fn validate_len(field: &str, value: &str, min: usize, max: usize) -> Result<(), ApiError> {
    let len = value.chars().count();
    if len < min || len > max {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("{field} 长度必须在 {min}..={max} 之间"),
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct SearchQuery {
    user_id: String,
    q: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

fn default_search_limit() -> usize {
    8
}

async fn search_memories(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Value>, ApiError> {
    require_api_key(&state, &headers)?;
    validate_len("q", &query.q, 1, 50_000)?;
    let vector = state
        .agent
        .embedder()
        .embed(&[query.q.clone()], true)
        .await
        .map_err(ApiError::internal)?
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "查询向量为空"))?;
    let items = state
        .agent
        .store()
        .search_memories(
            query.user_id,
            vector,
            Some(query.limit.clamp(1, 50)),
            None,
            true,
            query.q,
        )
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(serde_json::to_value(items).map_err(|e| ApiError::internal(e.into()))?))
}

#[derive(Deserialize)]
struct RecentQuery {
    user_id: String,
    #[serde(default = "default_recent_limit")]
    limit: usize,
}

fn default_recent_limit() -> usize {
    10
}

async fn recent_memories(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RecentQuery>,
) -> Result<Json<Value>, ApiError> {
    require_api_key(&state, &headers)?;
    let items = state
        .agent
        .store()
        .recent_memories(query.user_id, query.limit.clamp(1, 100))
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(serde_json::to_value(items).map_err(|e| ApiError::internal(e.into()))?))
}

#[derive(Deserialize)]
struct CreateMemoryRequest {
    user_id: String,
    text: String,
    #[serde(default = "default_kind")]
    kind: String,
    #[serde(default = "default_level")]
    level: i64,
    #[serde(default = "default_subject")]
    subject: String,
    #[serde(default)]
    entities: Vec<EntityInput>,
}

#[derive(Deserialize)]
struct EntityInput {
    name: String,
    #[serde(default = "default_entity_type", rename = "type")]
    kind: String,
}

fn default_kind() -> String {
    "other".into()
}
fn default_level() -> i64 {
    5
}
fn default_subject() -> String {
    "user".into()
}
fn default_entity_type() -> String {
    "entity".into()
}

async fn create_memory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateMemoryRequest>,
) -> Result<Json<Value>, ApiError> {
    require_api_key(&state, &headers)?;
    validate_len("text", &body.text, 1, 50_000)?;
    if !(1..=10).contains(&body.level) {
        return Err(ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "level 必须在 1..=10"));
    }
    let vector = state
        .agent
        .embedder()
        .embed(&[body.text.clone()], false)
        .await
        .map_err(ApiError::internal)?
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "向量为空"))?;
    let view = state
        .agent
        .store()
        .create_memory(NewMemory {
            user_id: body.user_id,
            text: body.text,
            kind: body.kind,
            level: body.level,
            subject: body.subject,
            entities: body
                .entities
                .into_iter()
                .map(|e| EntityView { name: e.name, kind: e.kind })
                .collect(),
            embedding: vector,
            source: "manual_api".into(),
        })
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(serde_json::to_value(view).map_err(|e| ApiError::internal(e.into()))?))
}

#[derive(Deserialize)]
struct UserQuery {
    user_id: String,
}

async fn forget_memory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(memory_id): Path<String>,
    Query(query): Query<UserQuery>,
) -> Result<Json<Value>, ApiError> {
    require_api_key(&state, &headers)?;
    let changed = state
        .agent
        .store()
        .forget_memory(query.user_id, memory_id)
        .await
        .map_err(ApiError::internal)?;
    if !changed {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "没有找到该用户的有效记忆",
        ));
    }
    Ok(Json(json!({"forgotten": true})))
}

async fn memory_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(memory_id): Path<String>,
    Query(query): Query<UserQuery>,
) -> Result<Json<Value>, ApiError> {
    require_api_key(&state, &headers)?;
    let items = state
        .agent
        .store()
        .memory_history(query.user_id, memory_id)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(serde_json::to_value(items).map_err(|e| ApiError::internal(e.into()))?))
}

#[derive(Deserialize)]
struct LinkRequest {
    user_id: String,
    from_memory_id: String,
    to_memory_id: String,
    #[serde(default = "default_relation")]
    relation: String,
}

fn default_relation() -> String {
    "related".into()
}

async fn link_memories(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<LinkRequest>,
) -> Result<Json<Value>, ApiError> {
    require_api_key(&state, &headers)?;
    let linked = state
        .agent
        .store()
        .link_memories(
            body.user_id,
            body.from_memory_id,
            body.to_memory_id,
            body.relation,
        )
        .await
        .map_err(ApiError::internal)?;
    if !linked {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "两条记忆必须存在且属于同一用户",
        ));
    }
    Ok(Json(json!({"linked": true})))
}

#[derive(Deserialize)]
struct MoodQuery {
    #[serde(default = "default_mood_days")]
    days: i64,
    #[serde(default = "default_mood_limit")]
    limit: usize,
}

fn default_mood_days() -> i64 {
    7
}
fn default_mood_limit() -> usize {
    50
}

async fn mood_timeline(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<MoodQuery>,
) -> Result<Json<Value>, ApiError> {
    require_api_key(&state, &headers)?;
    let trend = state
        .agent
        .store()
        .mood_trend(user_id.clone(), query.days.clamp(1, 90))
        .await
        .map_err(ApiError::internal)?;
    let recent = state
        .agent
        .store()
        .recent_moods(user_id, query.limit.clamp(1, 500))
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({"trend": trend, "recent": recent})))
}

#[derive(Deserialize)]
struct GraphQuery {
    #[serde(default = "default_graph_limit")]
    limit: usize,
}

fn default_graph_limit() -> usize {
    100
}

async fn graph(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<GraphQuery>,
) -> Result<Json<Value>, ApiError> {
    require_api_key(&state, &headers)?;
    let snapshot = state
        .agent
        .store()
        .graph_snapshot(user_id, query.limit.clamp(1, 500))
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(snapshot))
}
