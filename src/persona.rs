//! The user-saved character background is the only runtime personality layer.
use crate::{
    App,
    api::{ApiError, ApiResult},
    config::Persona,
    domain::{Message, id, now},
};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub fn source_hash(persona: &Persona) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(persona).expect("persona serializes"))
    )
}
pub(crate) fn snapshot(app: &App) -> App {
    // Freeze the saved background for the whole generation/tool loop.
    let mut frozen = app.clone();
    frozen.persona =
        std::sync::Arc::new(std::sync::RwLock::new(app.persona.read().unwrap().clone()));
    frozen.stickers =
        std::sync::Arc::new(std::sync::RwLock::new(app.stickers.read().unwrap().clone()));
    frozen.llm = app.llm.with_sticker_ids(
        frozen
            .stickers
            .read()
            .unwrap()
            .stickers
            .iter()
            .map(|s| s.id.clone()),
    );
    frozen
}

fn preview_user(content: &str) -> Message {
    Message {
        id: id(),
        session_id: String::new(),
        turn_id: id(),
        role: "user".into(),
        content: content.into(),
        created_at: now(),
    }
}
pub fn routes() -> Router<App> {
    Router::new()
        .route("/v1/console/persona/context", get(inspect))
        .route("/v1/console/persona/archive", get(archive))
        .route("/v1/console/persona/derivation", get(archive).put(retired))
        .route("/v1/console/persona/derivation/generate", post(retired))
        .route("/v1/console/persona/derivation/rollback", post(retired))
        .route("/v1/console/persona/evolution", get(archive).put(retired))
        .route("/v1/console/persona/evolution/reflect", post(retired))
        .route("/v1/console/persona/evolution/rollback", post(retired))
        .route("/v1/console/persona/preview", post(preview))
        .route(
            "/v1/console/persona/state",
            get(interaction_view).delete(interaction_clear),
        )
}
async fn retired() -> ApiError {
    ApiError(StatusCode::GONE, "persona_layer_retired")
}
async fn archive(State(app): State<App>) -> ApiResult<Json<Value>> {
    // Retain legacy data for owner inspection; never apply it or run a model.
    let metadata: Vec<(String,String)> = sqlx::query_as("SELECT key,value FROM metadata WHERE key IN ('console.persona_derivation.v1','agent.evolution.v1')")
        .fetch_all(&app.store.pool).await.map_err(anyhow::Error::from)?;
    let history: Vec<String> =
        sqlx::query_scalar("SELECT record FROM evolution_revisions ORDER BY version DESC LIMIT 30")
            .fetch_all(&app.store.pool)
            .await
            .map_err(anyhow::Error::from)?;
    let decode = |text: String| serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
    Ok(Json(
        json!({"status":"archived","applied":false,"metadata":metadata.into_iter().map(|(key,value)|json!({"key":key,"value":decode(value)})).collect::<Vec<_>>(),"history":history.into_iter().map(decode).collect::<Vec<_>>()}),
    ))
}
async fn inspect(State(app): State<App>) -> Json<Value> {
    let frozen = snapshot(&app);
    let persona = frozen.persona.read().unwrap().clone();
    let mut messages: Vec<Value> =
        crate::chat::build_context(&frozen, vec![], &preview_user(""), &[])
            .into_iter()
            .map(|m| json!(m))
            .collect();
    if app.tools.is_some() {
        crate::chat::add_tool_instructions(&mut messages);
    }
    Json(
        json!({"source_hash":source_hash(&persona),"persona":persona,"model":app.config.llm.model,"system_prompt":messages[0]["content"],"system_tokens":app.tokens.count(messages[0]["content"].as_str().unwrap())}),
    )
}
async fn interaction_view(State(app): State<App>) -> ApiResult<Json<Value>> {
    let session = app.store.conversation().await?;
    let summary: Option<String> =
        sqlx::query_scalar("SELECT summary FROM conversation_context WHERE session_id=?")
            .bind(&session)
            .fetch_optional(&app.store.pool)
            .await
            .map_err(anyhow::Error::from)?;
    let legacy: Option<String> =
        sqlx::query_scalar("SELECT payload FROM interaction_state WHERE session_id=?")
            .bind(&session)
            .fetch_optional(&app.store.pool)
            .await
            .map_err(anyhow::Error::from)?;
    Ok(Json(
        json!({"snapshot":null,"mode":"history_and_summary","summary":summary.unwrap_or_default(),"legacy_notes":legacy.map(|s|serde_json::from_str::<Value>(&s).unwrap_or(Value::String(s)))}),
    ))
}
async fn interaction_clear(State(app): State<App>) -> ApiResult<Json<Value>> {
    let session = app.store.conversation().await?;
    let mut tx = app
        .store
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(anyhow::Error::from)?;
    let busy: bool = sqlx::query_scalar("SELECT active_turn IS NOT NULL FROM sessions WHERE id=?")
        .bind(&session)
        .fetch_one(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    if busy {
        return Err(ApiError(StatusCode::CONFLICT, "interaction_turn_running"));
    }
    sqlx::query("DELETE FROM interaction_state WHERE session_id=?")
        .bind(session)
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    Ok(Json(json!({"cleared":true})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preview {
    message: String,
    source_hash: String,
    #[serde(default)]
    use_context: bool,
}
async fn preview(State(app): State<App>, Json(input): Json<Preview>) -> ApiResult<Json<Value>> {
    if input.message.trim().is_empty() || input.message.chars().count() > 4000 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_preview_message"));
    }
    let mut app = snapshot(&app);
    if input.source_hash != source_hash(&app.persona.read().unwrap()) {
        return Err(ApiError(StatusCode::CONFLICT, "persona_changed"));
    }
    let _permit = app
        .generation_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError(StatusCode::TOO_MANY_REQUESTS, "too_many_generations"))?;
    let mut history = vec![];
    let mut summary = String::new();
    let mut memories = vec![];
    let mut user = preview_user(&input.message);
    if input.use_context {
        user.session_id = app.store.conversation().await?;
        let state: Option<(String, i64)> = sqlx::query_as(
            "SELECT summary,through_seq FROM conversation_context WHERE session_id=?",
        )
        .bind(&user.session_id)
        .fetch_optional(&app.store.pool)
        .await
        .map_err(anyhow::Error::from)?;
        let (text, through) = state.unwrap_or_default();
        summary = text;
        history = crate::continuity::active_history(&app, &user.session_id, through).await?;
        let mut scoped = app.clone();
        scoped.llm = app.llm.for_purpose("persona_preview");
        memories = crate::memory::recall(&scoped, &input.message)
            .await?
            .memories;
    }
    crate::chat::attach_recall_sources(&mut app, &memories).await?;
    let mut messages = crate::chat::context_with_summary(&app, history, &user, &memories, &summary);
    messages[0]
        .content
        .push_str("\n独立预览：工具不可用，结果不写入对话或记忆。");
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    drop(rx);
    let reply = app
        .llm
        .for_purpose("persona_preview")
        .stream(&messages, &tx)
        .await
        .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "persona_preview_failed"))?;
    // Match the formal response's text cleanup without writing media or a turn.
    let (reply, _, _) = app.stickers.read().unwrap().parse_reply(&reply);
    Ok(Json(
        json!({"reply":reply,"source_hash":input.source_hash,"use_context":input.use_context}),
    ))
}
