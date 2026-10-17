use crate::{
    App,
    api::{ApiError, ApiResult},
    config::{Config, Persona},
    store::Store,
};
use axum::{
    Extension, Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post, put},
};
use ring::pbkdf2;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    num::NonZeroU32,
    time::{Duration, Instant},
};
use uuid::Uuid;

const PERSONA: &str = "console.persona";
const PASSWORD: &str = "console.password";
pub(crate) const GRAPH_RESET: &str = "agent.graph_reset_pending";
const SESSION_TTL: Duration = Duration::from_secs(30 * 60);

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
    sessions: HashMap<String, Instant>,
    retry_after: Option<Instant>,
}
#[derive(Clone, Copy)]
pub(crate) struct AuthMethod(pub bool); // true for the deployment API token (recovery credential).
pub(crate) type ActivityGuard = std::sync::Arc<tokio::sync::OwnedRwLockReadGuard<()>>;

impl ConsoleSecurity {
    pub(crate) fn authenticate(&mut self, token: &str) -> bool {
        let now = Instant::now();
        self.sessions.retain(|_, expiry| *expiry > now);
        if let Some(expiry) = self.sessions.get_mut(token) {
            *expiry = now + SESSION_TTL;
            true
        } else {
            false
        }
    }
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
    let password = metadata(store, PASSWORD)
        .await?
        .map(|value| serde_json::from_str(&value))
        .transpose()?;
    Ok((
        persona,
        ConsoleSecurity {
            password,
            ..Default::default()
        },
    ))
}
pub fn public_routes() -> Router<App> {
    Router::new().route("/v1/console/login", post(login))
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
        json!({"persona":persona,"default_persona":app.config.persona,"password_configured":configured,
        "graph_cleanup_pending":metadata(&app.store, GRAPH_RESET).await?.is_some()}),
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
async fn login(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<Login>,
) -> ApiResult<Json<Value>> {
    if headers
        .get("sec-fetch-site")
        .is_some_and(|site| site == "cross-site")
    {
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
    security
        .sessions
        .retain(|_, expiry| *expiry > Instant::now());
    if security.sessions.len() >= 32 {
        let oldest = security
            .sessions
            .iter()
            .min_by_key(|(_, at)| **at)
            .map(|(id, _)| id.clone())
            .unwrap();
        security.sessions.remove(&oldest);
    }
    let token = format!(
        "console-{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    );
    security
        .sessions
        .insert(token.clone(), Instant::now() + SESSION_TTL);
    Ok(Json(
        json!({"token":token,"expires_in":SESSION_TTL.as_secs()}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordUpdate {
    #[serde(default)]
    current_password: String,
    // Empty means return to API-token-only login.
    new_password: String,
}
async fn update_password(
    State(app): State<App>,
    Extension(auth): Extension<AuthMethod>,
    Json(input): Json<PasswordUpdate>,
) -> ApiResult<Json<Value>> {
    let length = input.new_password.chars().count();
    if (!input.new_password.is_empty()
        && (!(10..=128).contains(&length) || input.new_password.trim().is_empty()))
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
    let hash = if input.new_password.is_empty() {
        None
    } else {
        Some(
            tokio::task::spawn_blocking(move || PasswordHash::new(&input.new_password))
                .await
                .map_err(anyhow::Error::from)?,
        )
    };
    if let Some(hash) = &hash {
        save(
            &app.store,
            PASSWORD,
            serde_json::to_string(hash).map_err(anyhow::Error::from)?,
        )
        .await?;
    } else {
        sqlx::query("DELETE FROM metadata WHERE key=?")
            .bind(PASSWORD)
            .execute(&app.store.pool)
            .await
            .map_err(anyhow::Error::from)?;
    }
    security.password = hash;
    security.sessions.clear();
    security.retry_after = None;
    Ok(Json(
        json!({"password_configured":security.password.is_some(),"reauthenticate":true}),
    ))
}
async fn logout(State(app): State<App>, headers: HeaderMap) -> Json<Value> {
    if let Some(token) = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
    {
        app.console_security.lock().await.sessions.remove(token);
    }
    Json(json!({"ok":true}))
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
        "DELETE FROM jobs",
        "DELETE FROM qq_inbox",
        "DELETE FROM qq_sessions",
        "DELETE FROM memory_sources",
        "DELETE FROM memory_revisions",
        "DELETE FROM suppressed_sources",
        "DELETE FROM memories",
        "DELETE FROM forget_barriers",
        "DELETE FROM content_barriers",
        "DELETE FROM messages",
        "DELETE FROM turns",
        "DELETE FROM sessions",
        "INSERT INTO metadata(key,value) VALUES('agent.graph_reset_pending','1') ON CONFLICT(key) DO UPDATE SET value='1'",
    ] {
        sqlx::query(statement)
            .execute(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
    }
    if input.reset_persona {
        sqlx::query("DELETE FROM metadata WHERE key=?")
            .bind(PERSONA)
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
