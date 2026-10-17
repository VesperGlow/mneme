pub use crate::chat::build_context;
use crate::{App, domain::*, llm::emit, memory, store::BeginTurn};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response, Sse, sse::KeepAlive},
    routing::{get, post},
};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{convert::Infallible, time::Duration};
use subtle::ConstantTimeEq;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

pub struct ApiError(pub(crate) StatusCode, pub(crate) &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(_: anyhow::Error) -> Self {
        tracing::error!("request storage or processing failure");
        Self(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
    }
}
pub(crate) type ApiResult<T> = Result<T, ApiError>;

pub fn router(app: App) -> Router {
    let protected = Router::new()
        .route("/health/ready", get(ready))
        .route("/v1/conversation", get(conversation))
        .route("/v1/messages", get(conversation_history))
        .route("/v1/chat", post(conversation_chat))
        .route("/v1/sessions", post(create_session).get(list_sessions))
        .route("/v1/sessions/{id}/messages", get(history))
        .route("/v1/sessions/{id}/chat", post(chat))
        .route("/v1/turns/{id}", get(turn))
        .route(
            "/v1/memories",
            get(list_memories).post(crate::console::create_memory),
        )
        .route(
            "/v1/memories/{id}",
            get(get_memory)
                .put(crate::console::edit_memory)
                .delete(forget),
        )
        .merge(crate::console::protected_routes())
        .merge(crate::settings::protected_routes())
        .merge(crate::tools::routes())
        .merge(crate::assets::routes())
        .merge(crate::qq::protected_routes())
        .route("/v1/memories/search", post(search))
        .route("/v1/admin/jobs/retry", post(retry_jobs))
        .route("/v1/admin/reindex", post(reindex))
        .route_layer(middleware::from_fn_with_state(app.clone(), authenticate));
    Router::new()
        .route(
            "/health/live",
            get(|| async { Json(json!({"status":"alive"})) }),
        )
        .merge(protected)
        .merge(crate::console::public_routes())
        .merge(crate::settings::public_routes())
        .layer(DefaultBodyLimit::max(32 * 1024))
        .layer(middleware::from_fn(
            |request: Request, next: Next| async move {
                let mut response = next.run(request).await;
                response
                    .headers_mut()
                    .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
                response
                    .headers_mut()
                    .insert("x-content-type-options", "nosniff".parse().unwrap());
                response.headers_mut().insert("content-security-policy","default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' blob:; connect-src 'self'; font-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'".parse().unwrap());
                response.headers_mut().insert("referrer-policy","no-referrer".parse().unwrap());
                response.headers_mut().insert("x-frame-options","DENY".parse().unwrap());
                response
            },
        ))
        .with_state(app)
}

async fn authenticate(State(app): State<App>, mut request: Request, next: Next) -> Response {
    if crate::settings::cross_site(request.headers()) {
        return ApiError(StatusCode::FORBIDDEN, "cross_site_request").into_response();
    }
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let is_api_token = bool::from(supplied.as_bytes().ct_eq(app.config.api_token.as_bytes()));
    let cookie = crate::settings::cookie_token(request.headers()).map(str::to_owned);
    if !is_api_token {
        let valid = match &cookie {
            Some(token) => crate::settings::authenticate_cookie(&app, token).await,
            None => Ok(false),
        };
        match valid {
            Ok(true) => {}
            Ok(false) => return ApiError(StatusCode::UNAUTHORIZED, "unauthorized").into_response(),
            Err(error) => return ApiError::from(error).into_response(),
        }
    }
    request
        .extensions_mut()
        .insert(crate::settings::AuthMethod(is_api_token));
    let activity = if request.uri().path() == "/v1/admin/reset" {
        None
    } else {
        match app.maintenance.clone().try_read_owned() {
            Ok(guard) => Some(std::sync::Arc::new(guard)),
            Err(_) => return ApiError(StatusCode::CONFLICT, "agent_busy").into_response(),
        }
    };
    if let Some(guard) = &activity {
        request.extensions_mut().insert(guard.clone());
    }
    let path = request.uri().path().to_owned();
    let mut response = next.run(request).await;
    if !is_api_token
        && !matches!(path.as_str(), "/v1/console/logout" | "/v1/console/password")
        && let Some(token) = cookie
    {
        response.headers_mut().insert(
            header::SET_COOKIE,
            crate::settings::session_cookie(&app, &token, app.config.console.session_days * 86400)
                .parse()
                .unwrap(),
        );
    }
    response
}

async fn create_session(State(app): State<App>) -> ApiResult<(StatusCode, Json<Value>)> {
    let id = app.store.conversation().await?;
    Ok((StatusCode::CREATED, Json(json!({"id":id}))))
}
async fn list_sessions(State(app): State<App>) -> ApiResult<Json<Value>> {
    let id = app.store.conversation().await?;
    Ok(Json(json!({"sessions":[{"id":id,"preview":"持续对话"}]})))
}

pub(crate) async fn conversation(State(app): State<App>) -> ApiResult<Json<Value>> {
    let id = app.store.conversation().await?;
    let active: Option<String> = sqlx::query_scalar("SELECT active_turn FROM sessions WHERE id=?")
        .bind(&id)
        .fetch_one(&app.store.pool)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(Json(
        json!({"id":id,"active_turn":active,"context":crate::continuity::status(&app,&id).await?}),
    ))
}
async fn conversation_history(State(app): State<App>) -> ApiResult<Json<Value>> {
    let id = app.store.conversation().await?;
    history(State(app), Path(id)).await
}
async fn conversation_chat(
    State(app): State<App>,
    Extension(activity): Extension<crate::settings::ActivityGuard>,
    Json(input): Json<ChatRequest>,
) -> ApiResult<Response> {
    let id = app.store.conversation().await?;
    chat(State(app), Extension(activity), Path(id), Json(input)).await
}

async fn history(State(app): State<App>, Path(session): Path<String>) -> ApiResult<Json<Value>> {
    valid_id(&session)?;
    let session = app.store.resolve_conversation(&session).await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?)")
        .bind(&session)
        .fetch_one(&app.store.pool)
        .await
        .map_err(anyhow::Error::from)?;
    if !exists {
        return Err(ApiError(StatusCode::NOT_FOUND, "session_not_found"));
    }
    let history = app.store.history(&session, 100).await?;
    Ok(Json(
        json!({"messages":history,"assets":crate::assets::history_metadata(&app,&session).await?,"tool_calls":crate::tools::logs(&app,Some(&session)).await?}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatRequest {
    message: String,
    request_id: String,
    #[serde(default)]
    assets: Vec<String>,
}
async fn chat(
    State(app): State<App>,
    Extension(activity): Extension<crate::settings::ActivityGuard>,
    Path(session): Path<String>,
    Json(input): Json<ChatRequest>,
) -> ApiResult<Response> {
    valid_id(&session)?;
    let session = app.store.resolve_conversation(&session).await?;
    valid_id(&input.request_id)?;
    if !input.assets.is_empty() && app.tools.is_none() {
        return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "tools_disabled"));
    }
    if input.assets.len() > 4 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "too_many_images"));
    }
    for asset in &input.assets {
        valid_id(asset)?;
    }
    if input.message.trim().is_empty() || input.message.chars().count() > 4000 {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "message_must_be_1_to_4000_characters",
        ));
    }
    if *app.shutdown.borrow() {
        return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, "shutting_down"));
    }
    let permit = app
        .generation_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError(StatusCode::TOO_MANY_REQUESTS, "too_many_generations"))?;
    let (output, receiver) = mpsc::channel(128);
    match app
        .store
        .begin_turn_with_assets(&session, &input.request_id, &input.message, &input.assets)
        .await?
    {
        BeginTurn::Missing => return Err(ApiError(StatusCode::NOT_FOUND, "session_not_found")),
        BeginTurn::Busy => return Err(ApiError(StatusCode::CONFLICT, "session_busy")),
        BeginTurn::Conflict => return Err(ApiError(StatusCode::CONFLICT, "request_id_conflict")),
        BeginTurn::InvalidAssets => {
            return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_attachments"));
        }
        BeginTurn::Existing { status, content } => {
            if status != "complete" {
                return Err(ApiError(
                    StatusCode::CONFLICT,
                    "turn_already_exists_check_status",
                ));
            }
            let _ = emit(
                &output,
                "meta",
                json!({"turn_id":input.request_id,"replayed":true}),
            );
            let _ = emit(
                &output,
                "delta",
                json!({"content":content.unwrap_or_default()}),
            );
            let _ = emit(
                &output,
                "done",
                json!({"turn_id":input.request_id,"persisted":true,"replayed":true}),
            );
        }
        BeginTurn::Started(user) => {
            let output = output.clone();
            tokio::spawn(async move {
                let _activity = activity;
                let _permit = permit;
                let mut shutdown = app.shutdown.clone();
                let generation = crate::chat::generate(&app, &user, &output);
                let result = tokio::select! {
                    result = generation => result,
                    _ = shutdown.changed() => Err(anyhow::anyhow!("shutdown")),
                };
                if let Err(error) = result {
                    crate::diagnostics::warn("http.generate", &error);
                    // Partial deltas are provisional; only done means committed output.
                    if app
                        .store
                        .fail_turn(&user, "generation_failed")
                        .await
                        .is_err()
                    {
                        tracing::error!(turn_id=%user.turn_id,"could not mark failed turn");
                    }
                    let _ = emit(
                        &output,
                        "error",
                        json!({"turn_id":user.turn_id,"error":"generation_failed","persisted":false}),
                    );
                    tracing::warn!(turn_id=%user.turn_id,"generation failed");
                }
            });
        }
    }
    drop(output);
    Ok(
        Sse::new(ReceiverStream::new(receiver).map(Ok::<_, Infallible>))
            .keep_alive(
                KeepAlive::new()
                    .interval(Duration::from_secs(10))
                    .text("keep-alive"),
            )
            .into_response(),
    )
}

async fn turn(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    valid_id(&id)?;
    let row: Option<(String, String, Option<String>)> =
        sqlx::query_as("SELECT session_id,status,error_code FROM turns WHERE id=?")
            .bind(&id)
            .fetch_optional(&app.store.pool)
            .await
            .map_err(anyhow::Error::from)?;
    let Some((session, status, error)) = row else {
        return Err(ApiError(StatusCode::NOT_FOUND, "turn_not_found"));
    };
    let messages: Vec<Message> = sqlx::query_as("SELECT id,session_id,turn_id,role,content,created_at FROM messages WHERE turn_id=? ORDER BY seq")
        .bind(&id).fetch_all(&app.store.pool).await.map_err(anyhow::Error::from)?;
    Ok(Json(
        json!({"id":id,"session_id":session,"status":status,"error":error,"messages":messages}),
    ))
}
async fn list_memories(
    State(app): State<App>,
    Query(paging): Query<crate::console_store::MemoryFilter>,
) -> ApiResult<Json<Value>> {
    crate::console::validate_filter(&paging)?;
    Ok(Json(
        serde_json::to_value(app.store.memory_page(&paging).await?).map_err(anyhow::Error::from)?,
    ))
}
async fn get_memory(State(app): State<App>, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    valid_id(&id)?;
    let memory = app
        .store
        .memory(&id)
        .await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "memory_not_found"))?;
    let sources = app.store.sources(&id).await?;
    let versions = app.store.memory_versions(&id).await?;
    Ok(Json(
        json!({"memory":memory,"sources":sources,"versions":versions}),
    ))
}
async fn forget(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(query): Query<crate::console::DeleteQuery>,
) -> ApiResult<Json<Value>> {
    valid_id(&id)?;
    crate::console::write_result(
        app.store
            .delete_memory(&id, query.expected_revision)
            .await?,
    )?;
    Ok(Json(
        json!({"id":id,"status":"forgotten","graph_cleanup":"queued"}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Search {
    query: String,
}
async fn search(State(app): State<App>, Json(input): Json<Search>) -> ApiResult<Json<Value>> {
    if input.query.trim().is_empty() || input.query.chars().count() > 4000 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_query"));
    }
    let result = memory::recall(&app, &input.query).await?;
    Ok(Json(
        json!({"results":result.memories,"degraded":result.degraded}),
    ))
}
async fn retry_jobs(State(app): State<App>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"retried":app.store.retry_dead().await?})))
}
async fn reindex(State(app): State<App>) -> ApiResult<Json<Value>> {
    Ok(Json(json!({"queued":app.store.reindex().await?})))
}
async fn ready(State(app): State<App>) -> ApiResult<(StatusCode, Json<Value>)> {
    let (models, graph) = tokio::join!(
        tokio::time::timeout(Duration::from_secs(3), app.models.health()),
        tokio::time::timeout(Duration::from_secs(3), app.graph.health())
    );
    let models = matches!(models, Ok(true));
    let graph = matches!(graph, Ok(true));
    let jobs = app.store.job_stats().await?;
    let status = if models && graph {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    Ok((
        status,
        Json(
            json!({"status":if status==StatusCode::OK {"ready"}else{"degraded"},"sqlite":true,"models":models,"neo4j":graph,"jobs":jobs}),
        ),
    ))
}
pub(crate) fn valid_id(id: &str) -> ApiResult<()> {
    uuid::Uuid::parse_str(id)
        .map(|_| ())
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_uuid"))
}
