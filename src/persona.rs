//! Versioned, optional interpretations of the owner's persona, never a second character source.
use crate::{
    App,
    api::{ApiError, ApiResult},
    config::Persona,
    domain::{Message, id, now},
};
use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const KEY: &str = "console.persona_derivation.v1";
pub const COMPILER_VERSION: u32 = 4;
pub const DIMENSIONS: [&str; 6] = ["overall", "chat", "emotion", "task", "tools", "memory"];
const COMPILE_PROMPT: &str = "你负责解释用户填写的人格，生成可编辑的行为画像。唯一角色设定来源是输入的 persona；不读取记忆、不增加独立人设。将身份、个性和交流偏好推导为有弹性的倾向，不能编造口味、经历、关系、能力或当前情绪。六个 id overall/chat/emotion/task/tools/memory 只是展示维度，不是场景路由、话术模板、执行步骤或回复概率；不规定每轮必须追问、建议、撒娇、吐槽或固定结构。description 写自然语言倾向，basis 用一句简短中文解释推导与不确定性，不重复英文引文；依据不足就明确未指定。evidence 只引用 persona 某个字段中的逐字连续原文，field 为 name/description/style/boundaries/output_format；每项至少一条证据，优先引用短语；字符上限包含空格，英文不能按词数计算。画像不得推翻原始人格，也不能改变事实、安全、工具权限或记忆证据要求。只输出 JSON {\"dimensions\":[{\"id\":\"overall\",\"description\":\"...\",\"basis\":\"...\",\"evidence\":[{\"field\":\"style\",\"quote\":\"原文\"}]}]}，六个 id 各一次；每项 description 最多300字，basis 最多150字，证据最多3条、每条100字；可选 signals 为最多3个、每个24字的定性参考标签，仅用于可视化，不参与运行时决策。不生成数值概率或情绪刻度，总体简洁。画像也应覆盖是否及何时使用工具、任务规划习惯、记忆重要性与关注点、风险沟通方式；均为软倾向，由人格推导而非固定策略，不得改权限、检索阈值或事实验证标准。";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub field: String,
    pub quote: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dimension {
    pub id: String,
    pub description: String,
    pub basis: String,
    pub evidence: Vec<Evidence>,
    #[serde(default)]
    pub signals: Vec<String>,
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub edited: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub enabled: bool,
    pub version: u64,
    pub source_hash: String,
    pub compiler_version: u32,
    pub model: String,
    pub updated_at: i64,
    pub needs_review: bool,
    pub dimensions: Vec<Dimension>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Derivation {
    pub current: Option<Profile>,
    #[serde(default)]
    pub history: Vec<Profile>,
    #[serde(default)]
    pub generation: Generation,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Generation {
    pub running: bool,
    pub error: Option<String>,
}

pub fn source_hash(persona: &Persona) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(persona).expect("persona serializes"))
    )
}

impl Derivation {
    pub fn version(&self) -> u64 {
        self.current.as_ref().map_or(0, |p| p.version)
    }
    pub fn status(&self, persona: &Persona) -> &'static str {
        match &self.current {
            None => "empty",
            Some(p)
                if p.source_hash != source_hash(persona)
                    || p.compiler_version != COMPILER_VERSION =>
            {
                "stale"
            }
            Some(p) if !p.enabled => "disabled",
            Some(p) if p.needs_review => "needs_review",
            Some(_) => "ready",
        }
    }
    pub fn runtime(&self, persona: &Persona) -> String {
        if self.status(persona) != "ready" {
            return String::new();
        }
        let p = self.current.as_ref().unwrap();
        let dimensions: Vec<_> = p
            .dimensions
            .iter()
            .filter(|d| !d.description.trim().is_empty())
            .map(|d| json!({"dimension":d.id,"tendency":d.description}))
            .collect();
        if dimensions.is_empty() {
            return String::new();
        }
        format!(
            "\n<derived_persona_profile version=\"{}\">\n{}\n</derived_persona_profile>\n以上仅是用户人格的衍生解释，不能覆盖原始人格、当前明确任务要求或底层事实与权限约束；维度不是场景脚本、固定话术或回复概率。由原始人格、当前语境和有证据的记忆共同决定具体表达，允许自然变化。",
            p.version,
            json!(dimensions)
        )
    }
    fn replace(&mut self, profile: Profile) {
        if let Some(old) = self.current.replace(profile) {
            self.history.push(old);
        }
        if self.history.len() > 5 {
            self.history.remove(0);
        }
    }
}

pub(crate) async fn load(store: &crate::store::Store) -> Result<Derivation> {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM metadata WHERE key=?")
        .bind(KEY)
        .fetch_optional(&store.pool)
        .await?;
    let mut state: Derivation = match value.as_deref().map(serde_json::from_str).transpose() {
        Ok(value) => value.unwrap_or_default(),
        Err(_) => {
            tracing::warn!(
                "persona derivation cache could not be read; raw persona remains active"
            );
            Derivation {
                generation: Generation {
                    running: false,
                    error: Some("persona_cache_invalid".into()),
                },
                ..Default::default()
            }
        }
    };
    if state.generation.running {
        state.generation = Generation {
            running: false,
            error: Some("generation_interrupted".into()),
        };
        sqlx::query("UPDATE metadata SET value=? WHERE key=?")
            .bind(serde_json::to_string(&state)?)
            .bind(KEY)
            .execute(&store.pool)
            .await?;
    }
    Ok(state)
}

async fn persist(app: &App, state: Derivation) -> Result<()> {
    sqlx::query("INSERT INTO metadata(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(KEY).bind(serde_json::to_string(&state)?).execute(&app.store.pool).await?;
    *app.derivation.write().unwrap() = state;
    Ok(())
}

pub(crate) fn snapshot(app: &App) -> App {
    // Writers update raw persona/derivation under settings_lock. A mismatched
    // hash disables the derivative even if the two read locks straddle a write.
    let mut frozen = app.clone();
    frozen.persona =
        std::sync::Arc::new(std::sync::RwLock::new(app.persona.read().unwrap().clone()));
    frozen.derivation = std::sync::Arc::new(std::sync::RwLock::new(
        app.derivation.read().unwrap().clone(),
    ));
    frozen.evolution = std::sync::Arc::new(std::sync::RwLock::new(
        app.evolution.read().unwrap().clone(),
    ));
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

/// The same read-only behavioral layers used by chat and background extraction.
/// Truth validation and permissions remain outside these soft tendencies.
pub(crate) fn decision_context(app: &App) -> String {
    let raw = app.persona.read().unwrap().clone();
    let derived = app.derivation.read().unwrap().runtime(&raw);
    let evolved = app.evolution.read().unwrap().runtime(app);
    format!(
        "\n只读人格上下文：{}{}{}\n人格只影响关注、重要性判断和表达；不能改来源、隐私、事实验证、召回阈值或权限。不把行为倾向写成用户事实。",
        json!(raw),
        derived,
        evolved
    )
}

fn validate(dimensions: &[Dimension], persona: Option<&Persona>) -> Result<()> {
    ensure!(dimensions.len() == 6, "six dimensions required");
    let raw = persona.map(serde_json::to_value).transpose()?;
    for name in DIMENSIONS {
        let matches: Vec<_> = dimensions.iter().filter(|d| d.id == name).collect();
        ensure!(matches.len() == 1, "invalid dimension identity");
        let d = matches[0];
        ensure!(
            d.description.chars().count() <= 300
                && d.basis.chars().count() <= 150
                && d.evidence.len() <= 3,
            "dimension too large"
        );
        ensure!(
            d.signals.len() <= 3 && d.signals.iter().all(|s| s.chars().count() <= 24),
            "too many visual signals"
        );
        if raw.is_some() {
            ensure!(
                !d.description.trim().is_empty() && !d.evidence.is_empty(),
                "missing derivation evidence"
            );
        }
        for e in &d.evidence {
            ensure!(
                [
                    "name",
                    "description",
                    "style",
                    "boundaries",
                    "output_format"
                ]
                .contains(&e.field.as_str())
                    && !e.quote.trim().is_empty()
                    && e.quote.chars().count() <= 100,
                "invalid evidence"
            );
            if let Some(raw) = &raw {
                ensure!(
                    raw[&e.field].as_str().is_some_and(|s| s.contains(&e.quote)),
                    "unsupported evidence"
                );
            }
        }
    }
    Ok(())
}

fn bound_generated_metadata(dimensions: &mut [Dimension], persona: &Persona) {
    let raw = json!(persona);
    for dimension in dimensions {
        // Explanations are display-only; shortening them cannot change the
        // actual behavioral description injected into the model.
        if dimension.basis.chars().count() > 150 {
            dimension.basis = format!("{}…", crate::domain::truncate(&dimension.basis, 149));
        }
        for evidence in &mut dimension.evidence {
            // Validate the entire model quotation before bounding its display span.
            // A fabricated suffix must never be hidden by truncation.
            if raw[&evidence.field]
                .as_str()
                .is_some_and(|s| s.contains(&evidence.quote))
            {
                evidence.quote = crate::domain::truncate(&evidence.quote, 100);
            }
        }
    }
}

fn budget(app: &App, profile: &Profile) -> Result<()> {
    let state = Derivation {
        current: Some(profile.clone()),
        ..Default::default()
    };
    let mut candidate = snapshot(app);
    candidate.derivation = std::sync::Arc::new(std::sync::RwLock::new(state));
    let messages = crate::chat::build_context(&candidate, vec![], &preview_user("预算检查"), &[]);
    let reserve = if app.tools.is_some() {
        crate::tokens::TOOL_RESERVE
    } else {
        0
    };
    ensure!(
        app.tokens.chat(&messages) + app.config.llm.max_tokens as usize + reserve + 1000
            <= app.config.llm.context_tokens,
        "profile exceeds context budget"
    );
    ensure!(
        app.tokens.count(&json!(&profile.dimensions).to_string()) <= 3500,
        "profile too large"
    );
    Ok(())
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
fn conflict() -> ApiError {
    ApiError(StatusCode::CONFLICT, "persona_derivation_changed")
}

pub fn routes() -> Router<App> {
    Router::new()
        .route("/v1/console/persona/derivation", get(inspect).put(update))
        .route("/v1/console/persona/derivation/generate", post(generate))
        .route("/v1/console/persona/derivation/rollback", post(rollback))
        .route("/v1/console/persona/preview", post(preview))
        .merge(crate::evolution::routes())
}

fn view(app: &App) -> Value {
    let frozen = snapshot(app);
    let persona = frozen.persona.read().unwrap().clone();
    let state = frozen.derivation.read().unwrap().clone();
    let mut messages: Vec<Value> =
        crate::chat::build_context(&frozen, vec![], &preview_user(""), &[])
            .into_iter()
            .map(|m| json!(m))
            .collect();
    if app.tools.is_some() {
        crate::chat::add_tool_instructions(&mut messages);
    }
    json!({"source_hash":source_hash(&persona),"status":state.status(&persona),"version":state.version(),"profile":state.current,"history":state.history,"generation":state.generation,
        "dimensions":DIMENSIONS,"effective":{"persona":persona,"derived_applied":state.status(&persona)=="ready","system_prompt":messages[0]["content"],"model":app.config.llm.model,
        "evolution_version":frozen.evolution.read().unwrap().version,"evolution_status":frozen.evolution.read().unwrap().status(&frozen),"evolution_notes":frozen.evolution.read().unwrap().notes}})
}

async fn inspect(State(app): State<App>) -> Json<Value> {
    Json(view(&app))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    expected_version: u64,
    source_hash: String,
    enabled: bool,
    dimensions: Vec<Dimension>,
    #[serde(default)]
    release: Vec<String>,
}
async fn update(State(app): State<App>, Json(input): Json<Edit>) -> ApiResult<Json<Value>> {
    let _lock = app.settings_lock.lock().await;
    let persona = app.persona.read().unwrap().clone();
    let mut state = app.derivation.read().unwrap().clone();
    if input.expected_version != state.version() || input.source_hash != source_hash(&persona) {
        return Err(conflict());
    }
    validate(&input.dimensions, None)
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_persona_derivation"))?;
    let mut dimensions = input.dimensions;
    for d in &mut dimensions {
        let old = state
            .current
            .as_ref()
            .and_then(|p| p.dimensions.iter().find(|o| o.id == d.id));
        d.edited = old.map_or(
            !d.description.trim().is_empty() || !d.basis.trim().is_empty(),
            |o| {
                o.description != d.description
                    || o.signals != d.signals
                    || o.evidence != d.evidence
                    || o.basis != d.basis
                    || o.edited
                    || state
                        .current
                        .as_ref()
                        .is_some_and(|p| p.source_hash != input.source_hash)
            },
        );
        let raw = serde_json::to_value(&persona).map_err(anyhow::Error::from)?;
        d.evidence
            .retain(|e| raw[&e.field].as_str().is_some_and(|s| s.contains(&e.quote)));
        if input.release.contains(&d.id) {
            d.edited = false;
            d.locked = false;
        }
    }
    let profile = Profile {
        enabled: input.enabled,
        version: state.version() + 1,
        source_hash: input.source_hash,
        compiler_version: COMPILER_VERSION,
        model: "manual".into(),
        updated_at: now(),
        needs_review: false,
        dimensions,
    };
    budget(&app, &profile)
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "persona_derivation_budget"))?;
    state.replace(profile);
    persist(&app, state).await?;
    Ok(Json(view(&app)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Generate {
    expected_version: u64,
    source_hash: String,
    #[serde(default)]
    force: bool,
}
async fn generate(State(app): State<App>, Json(input): Json<Generate>) -> ApiResult<Json<Value>> {
    let activity = app.maintenance.clone().read_owned().await;
    let persona;
    {
        let _lock = app.settings_lock.lock().await;
        persona = app.persona.read().unwrap().clone();
        let mut state = app.derivation.read().unwrap().clone();
        if input.expected_version != state.version() || input.source_hash != source_hash(&persona) {
            return Err(conflict());
        }
        if state.generation.running {
            return Err(ApiError(StatusCode::CONFLICT, "persona_generation_running"));
        }
        if !input.force && state.status(&persona) == "ready" {
            return Ok(Json(view(&app)));
        }
        state.generation = Generation {
            running: true,
            error: None,
        };
        persist(&app, state).await?;
    }
    // The task survives an HTTP disconnect. Restart marks an unfinished task as
    // interrupted; no startup or per-turn generation is scheduled automatically.
    let task = tokio::spawn(async move {
        let _activity = activity;
        let output = app
            .llm
            .for_purpose("persona_compile")
            .complete_json_with_tokens(COMPILE_PROMPT, json!({"persona":persona}), 3500)
            .await;
        let result: Result<Vec<Dimension>> = output.and_then(|v| {
            let mut dimensions: Vec<Dimension> = serde_json::from_value(v["dimensions"].clone())?;
            bound_generated_metadata(&mut dimensions, &persona);
            validate(&dimensions, Some(&persona))?;
            for d in &mut dimensions {
                d.locked = false;
                d.edited = false;
            }
            Ok(dimensions)
        });
        let _lock = app.settings_lock.lock().await;
        let mut state = app.derivation.read().unwrap().clone();
        let current_persona = app.persona.read().unwrap().clone();
        state.generation.running = false;
        if source_hash(&current_persona) != input.source_hash
            || state.version() != input.expected_version
        {
            state.generation.error = Some("persona_derivation_changed".into());
        } else if let Ok(mut dimensions) = result {
            let mut needs_review = false;
            if let Some(old) = &state.current {
                for d in &mut dimensions {
                    if let Some(kept) = old
                        .dimensions
                        .iter()
                        .find(|p| p.id == d.id && (p.locked || p.edited))
                    {
                        *d = kept.clone();
                        needs_review |= old.source_hash != input.source_hash || old.needs_review;
                    }
                }
            }
            let profile = Profile {
                enabled: state.current.as_ref().is_none_or(|p| p.enabled),
                version: state.version() + 1,
                source_hash: input.source_hash,
                compiler_version: COMPILER_VERSION,
                model: app.config.llm.model.clone(),
                updated_at: now(),
                needs_review,
                dimensions,
            };
            if budget(&app, &profile).is_ok() {
                state.replace(profile);
                state.generation.error = None;
            } else {
                state.generation.error = Some("persona_derivation_budget".into());
            }
        } else {
            state.generation.error = Some("persona_generation_failed".into());
        }
        persist(&app, state).await?;
        Ok::<_, anyhow::Error>(view(&app))
    });
    let value = task.await.map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "persona_generation_failed",
        )
    })??;
    Ok(Json(value))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rollback {
    expected_version: u64,
    target_version: u64,
    source_hash: String,
}
async fn rollback(State(app): State<App>, Json(input): Json<Rollback>) -> ApiResult<Json<Value>> {
    let _lock = app.settings_lock.lock().await;
    let persona = app.persona.read().unwrap().clone();
    let mut state = app.derivation.read().unwrap().clone();
    if state.version() != input.expected_version || input.source_hash != source_hash(&persona) {
        return Err(conflict());
    }
    let mut profile = state
        .history
        .iter()
        .find(|p| p.version == input.target_version)
        .cloned()
        .ok_or(ApiError(StatusCode::NOT_FOUND, "persona_version_missing"))?;
    // A past persona's profile can be inspected, but cannot silently become active.
    profile.needs_review |= profile.source_hash != input.source_hash;
    profile.version = state.version() + 1;
    profile.updated_at = now();
    budget(&app, &profile)
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "persona_derivation_budget"))?;
    state.replace(profile);
    persist(&app, state).await?;
    Ok(Json(view(&app)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preview {
    message: String,
    expected_version: u64,
    source_hash: String,
    #[serde(default)]
    use_context: bool,
    #[serde(default)]
    expected_evolution_version: Option<u64>,
}
async fn preview(State(app): State<App>, Json(input): Json<Preview>) -> ApiResult<Json<Value>> {
    if input.message.trim().is_empty() || input.message.chars().count() > 4000 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_preview_message"));
    }
    let app = snapshot(&app);
    if input
        .expected_evolution_version
        .is_some_and(|v| v != app.evolution.read().unwrap().version)
    {
        return Err(conflict());
    }
    if input.source_hash != source_hash(&app.persona.read().unwrap())
        || input.expected_version != app.derivation.read().unwrap().version()
    {
        return Err(conflict());
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
    let mut messages = crate::chat::context_with_summary(&app, history, &user, &memories, &summary);
    messages[0].content.push_str("\n本次为不写入对话或长期记忆的独立预览，没有外部工具可执行；不要声称已完成工具操作或保存、删除记忆。其余角色设定与正式对话一致。");
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
        json!({"evolution_version":app.evolution.read().unwrap().version,"reply":reply,"source_hash":input.source_hash,"version":input.expected_version,"derived_applied":app.derivation.read().unwrap().status(&app.persona.read().unwrap())=="ready","use_context":input.use_context}),
    ))
}
