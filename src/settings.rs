use crate::{
    App,
    api::{ApiError, ApiResult},
    config::{Config, Persona},
    store::Store,
};
use axum::{
    Extension, Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    routing::{get, post, put},
};
use ring::pbkdf2;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    num::NonZeroU32,
    time::{Duration, Instant},
};
use uuid::Uuid;

const PERSONA: &str = "console.persona";
const PASSWORD: &str = "console.password";
pub(crate) const GRAPH_RESET: &str = "agent.graph_reset_pending";
pub(crate) const COOKIE_NAME: &str = "mneme_console";

#[derive(Clone, Serialize, Deserialize)]
struct PasswordHash {
    salt: [u8; 16],
    hash: [u8; 32],
}
impl PasswordHash {
    fn new(password: &str) -> Self {
        let salt = *Uuid::new_v4().as_bytes();
        let mut hash = [0; 32];
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            Self::iterations(),
            &salt,
            password.as_bytes(),
            &mut hash,
        );
        Self { salt, hash }
    }
    fn iterations() -> NonZeroU32 {
        NonZeroU32::new(600_000).unwrap()
    }
    fn verify(&self, password: &str) -> bool {
        pbkdf2::verify(
            pbkdf2::PBKDF2_HMAC_SHA256,
            Self::iterations(),
            &self.salt,
            password.as_bytes(),
            &self.hash,
        )
        .is_ok()
    }
}

#[derive(Default)]
pub struct ConsoleSecurity {
    password: Option<PasswordHash>,
    retry_after: Option<Instant>,
}
#[derive(Clone, Copy)]
pub(crate) struct AuthMethod(pub bool); // true for the deployment API token (recovery credential).
pub(crate) type ActivityGuard = std::sync::Arc<tokio::sync::OwnedRwLockReadGuard<()>>;

pub(crate) fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            (name == COOKIE_NAME
                && value.len() == 64
                && value.bytes().all(|b| b.is_ascii_hexdigit()))
            .then_some(value)
        })
}
fn token_hash(token: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
pub(crate) fn session_cookie(app: &App, token: &str, max_age: u64) -> String {
    format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{}",
        if app.config.console.secure_cookie {
            "; Secure"
        } else {
            ""
        }
    )
}
pub(crate) async fn authenticate_cookie(app: &App, token: &str) -> anyhow::Result<bool> {
    let now = crate::domain::now();
    Ok(
        sqlx::query("UPDATE console_sessions SET expires_at=? WHERE token_hash=? AND expires_at>?")
            .bind(now + (app.config.console.session_days * 86400) as i64)
            .bind(token_hash(token))
            .bind(now)
            .execute(&app.store.pool)
            .await?
            .rows_affected()
            == 1,
    )
}
/// Fetch Metadata protects modern browsers; Origin also covers clients without that header.
/// Never trust forwarded host/proto headers to decide the cookie's Secure attribute.
pub(crate) fn cross_site(headers: &HeaderMap) -> bool {
    if headers
        .get("sec-fetch-site")
        .is_some_and(|site| site == "cross-site" || site == "same-site")
    {
        return true;
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let Ok(origin) = origin.to_str().unwrap_or("").parse::<reqwest::Url>() else {
            return true;
        };
        let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) else {
            return true;
        };
        let Ok(expected) = reqwest::Url::parse(&format!("{}://{host}", origin.scheme())) else {
            return true;
        };
        return origin.origin() != expected.origin()
            || !matches!(origin.scheme(), "http" | "https");
    }
    false
}

async fn metadata(store: &Store, key: &str) -> anyhow::Result<Option<String>> {
    Ok(sqlx::query_scalar("SELECT value FROM metadata WHERE key=?")
        .bind(key)
        .fetch_optional(&store.pool)
        .await?)
}
async fn save(store: &Store, key: &str, value: String) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO metadata(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(key).bind(value).execute(&store.pool).await?;
    Ok(())
}
pub(crate) async fn load(
    store: &Store,
    config: &Config,
) -> anyhow::Result<(Persona, ConsoleSecurity)> {
    let persona = match metadata(store, PERSONA).await? {
        Some(value) => serde_json::from_str(&value)?,
        None => config.persona.clone(),
    };
    let mut effective = config.clone();
    effective.persona = persona.clone();
    effective.validate()?;
    let mut password = metadata(store, PASSWORD)
        .await?
        .map(|value| serde_json::from_str(&value))
        .transpose()?;
    if password.is_none() && !config.console.password.is_empty() {
        let plain = config.console.password.clone();
        let hash = tokio::task::spawn_blocking(move || PasswordHash::new(&plain)).await?;
        save(store, PASSWORD, serde_json::to_string(&hash)?).await?;
        password = Some(hash);
    }
    Ok((
        persona,
        ConsoleSecurity {
            password,
            ..Default::default()
        },
    ))
}
pub fn public_routes() -> Router<App> {
    Router::new().route("/v1/console/login", post(login).get(login_status))
}
pub fn protected_routes() -> Router<App> {
    Router::new()
        .route("/v1/console/settings", get(settings))
        .route("/v1/console/persona", put(update_persona))
        .route("/v1/console/password", put(update_password))
        .route("/v1/console/logout", post(logout))
        .route("/v1/admin/reset", post(reset))
}
async fn settings(State(app): State<App>) -> ApiResult<Json<Value>> {
    let persona = app.persona.read().unwrap().clone();
    let configured = app.console_security.lock().await.password.is_some();
    Ok(Json(
        json!({"persona":persona,"default_persona":app.config.persona,"model":app.config.llm.model,"password_configured":configured,
        "session_days":app.config.console.session_days,"graph_cleanup_pending":metadata(&app.store, GRAPH_RESET).await?.is_some()}),
    ))
}
async fn update_persona(
    State(app): State<App>,
    Json(persona): Json<Persona>,
) -> ApiResult<Json<Value>> {
    let mut effective = (*app.config).clone();
    effective.persona = persona.clone();
    if effective.validate().is_err() || persona.name.chars().count() > 80 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_persona"));
    }
    let _settings = app.settings_lock.lock().await;
    save(
        &app.store,
        PERSONA,
        serde_json::to_string(&persona).map_err(anyhow::Error::from)?,
    )
    .await?;
    *app.persona.write().unwrap() = persona.clone();
    Ok(Json(json!({"persona":persona})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    password: String,
}
async fn login_status(State(app): State<App>) -> Json<Value> {
    Json(
        json!({"password_configured":app.console_security.lock().await.password.is_some(),"session_days":app.config.console.session_days}),
    )
}
async fn login(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<Login>,
) -> ApiResult<(HeaderMap, Json<Value>)> {
    if cross_site(&headers) {
        return Err(ApiError(StatusCode::FORBIDDEN, "cross_site_request"));
    }
    let _activity = app.maintenance.read().await;
    let mut security = app.console_security.lock().await;
    if security.retry_after.is_some_and(|at| at > Instant::now()) {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "login_rate_limited",
        ));
    }
    let valid = if input.password.chars().count() <= 128 {
        if let Some(hash) = security.password.clone() {
            tokio::task::spawn_blocking(move || hash.verify(&input.password))
                .await
                .map_err(anyhow::Error::from)?
        } else {
            false
        }
    } else {
        false
    };
    if !valid {
        security.retry_after = Some(Instant::now() + Duration::from_secs(1));
        return Err(ApiError(StatusCode::UNAUTHORIZED, "unauthorized"));
    }
    security.retry_after = None;
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let ttl = app.config.console.session_days * 86400;
    let mut tx = app
        .store
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(anyhow::Error::from)?;
    sqlx::query("DELETE FROM console_sessions WHERE expires_at<=?")
        .bind(crate::domain::now())
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    sqlx::query("DELETE FROM console_sessions WHERE token_hash IN (SELECT token_hash FROM console_sessions ORDER BY expires_at DESC LIMIT -1 OFFSET 31)").execute(&mut *tx).await.map_err(anyhow::Error::from)?;
    sqlx::query("INSERT INTO console_sessions(token_hash,expires_at) VALUES(?,?)")
        .bind(token_hash(&token))
        .bind(crate::domain::now() + ttl as i64)
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        session_cookie(&app, &token, ttl).parse().unwrap(),
    );
    Ok((
        response_headers,
        Json(json!({"authenticated":true,"expires_in":ttl})),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordUpdate {
    #[serde(default)]
    current_password: String,
    new_password: String,
}
async fn update_password(
    State(app): State<App>,
    Extension(auth): Extension<AuthMethod>,
    Json(input): Json<PasswordUpdate>,
) -> ApiResult<Json<Value>> {
    let length = input.new_password.chars().count();
    if (!(10..=128).contains(&length) || input.new_password.trim().is_empty())
        || input.current_password.chars().count() > 128
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_password"));
    }
    let mut security = app.console_security.lock().await;
    if !auth.0 {
        let hash = security
            .password
            .clone()
            .ok_or(ApiError(StatusCode::UNAUTHORIZED, "unauthorized"))?;
        if !tokio::task::spawn_blocking(move || hash.verify(&input.current_password))
            .await
            .map_err(anyhow::Error::from)?
        {
            return Err(ApiError(
                StatusCode::BAD_REQUEST,
                "current_password_incorrect",
            ));
        }
    }
    let hash = tokio::task::spawn_blocking(move || PasswordHash::new(&input.new_password))
        .await
        .map_err(anyhow::Error::from)?;
    let mut tx = app
        .store
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(anyhow::Error::from)?;
    sqlx::query("INSERT INTO metadata(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(PASSWORD).bind(serde_json::to_string(&hash).map_err(anyhow::Error::from)?)
        .execute(&mut *tx).await.map_err(anyhow::Error::from)?;
    sqlx::query("DELETE FROM console_sessions")
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    security.password = Some(hash);
    security.retry_after = None;
    Ok(Json(
        json!({"password_configured":true,"reauthenticate":true}),
    ))
}
async fn logout(State(app): State<App>, headers: HeaderMap) -> ApiResult<(HeaderMap, Json<Value>)> {
    if let Some(token) = cookie_token(&headers) {
        sqlx::query("DELETE FROM console_sessions WHERE token_hash=?")
            .bind(token_hash(token))
            .execute(&app.store.pool)
            .await
            .map_err(anyhow::Error::from)?;
    }
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::SET_COOKIE,
        session_cookie(&app, "", 0).parse().unwrap(),
    );
    Ok((response_headers, Json(json!({"ok":true}))))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reset {
    confirmation: String,
    #[serde(default)]
    reset_persona: bool,
}
async fn reset(State(app): State<App>, Json(input): Json<Reset>) -> ApiResult<Json<Value>> {
    if input.confirmation != "RESET" {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "reset_confirmation_required",
        ));
    }
    // Queue a writer so workers finish their current item and stop claiming more work.
    let _exclusive = tokio::time::timeout(Duration::from_secs(3), app.maintenance.write())
        .await
        .map_err(|_| ApiError(StatusCode::CONFLICT, "agent_busy"))?;
    let mut tx = app
        .store
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(anyhow::Error::from)?;
    let messages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
        .fetch_one(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    let memories: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM memories")
        .fetch_one(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    // Foreign-key order matters. Keep deployment configuration, credentials and model selection.
    for statement in [
        "DELETE FROM model_requests",
        "DELETE FROM tool_calls",
        "DELETE FROM message_stickers",
        "DELETE FROM message_assets",
        "DELETE FROM assets",
        "DELETE FROM jobs",
        "DELETE FROM qq_inbox",
        "DELETE FROM qq_sessions",
        "DELETE FROM memory_sources",
        "DELETE FROM forget_requests",
        "DELETE FROM memory_revisions",
        "DELETE FROM suppressed_sources",
        "DELETE FROM memories",
        "DELETE FROM memory_slot_aliases",
        "DELETE FROM forget_barriers",
        "DELETE FROM content_barriers",
        "DELETE FROM conversation_context",
        "DELETE FROM interaction_state",
        "DELETE FROM conversation_aliases",
        "DELETE FROM extracted_messages",
        "DELETE FROM metadata WHERE key IN ('conversation.id','conversation.scope') OR key LIKE 'memory.extraction.%'",
        "DELETE FROM evolution_feedback",
        "DELETE FROM evolution_revisions",
        "DELETE FROM metadata WHERE key='agent.evolution.v1'",
        "DELETE FROM messages",
        "DELETE FROM turns",
        "DELETE FROM sessions",
        "INSERT INTO metadata(key,value) VALUES('agent.graph_reset_pending','1') ON CONFLICT(key) DO UPDATE SET value='1'",
        "INSERT INTO metadata(key,value) VALUES('tools.reset_pending','1') ON CONFLICT(key) DO UPDATE SET value='1'",
    ] {
        sqlx::query(statement)
            .execute(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
    }
    crate::stickers::cleanup(&mut tx).await?;
    if input.reset_persona {
        sqlx::query("DELETE FROM metadata WHERE key=?")
            .bind(PERSONA)
            .execute(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
        sqlx::query("DELETE FROM metadata WHERE key='console.persona_derivation.v1'")
            .execute(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
    }
    tx.commit().await.map_err(anyhow::Error::from)?;
    if input.reset_persona {
        *app.persona.write().unwrap() = app.config.persona.clone();
    }
    *app.status_cache.lock().await = None;
    Ok(Json(
        json!({"reset":true,"deleted_messages":messages,"deleted_memories":memories,"graph_cleanup":"queued"}),
    ))
}

// Run before projecting new memories. The durable marker survives crashes and graph outages.
pub(crate) async fn cleanup_graph(app: &App) -> anyhow::Result<()> {
    if metadata(&app.store, GRAPH_RESET).await?.is_some() {
        app.graph
            .query(
                "MATCH (n) WHERE n:Memory OR n:Entity DETACH DELETE n",
                json!({}),
            )
            .await?;
        sqlx::query("DELETE FROM metadata WHERE key=?")
            .bind(GRAPH_RESET)
            .execute(&app.store.pool)
            .await?;
    }
    Ok(())
}
