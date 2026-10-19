use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{env, path::Path};

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub listen: String,
    pub database: String,
    pub timezone: String,
    pub llm: LlmConfig,
    pub models: ModelsConfig,
    pub neo4j: Neo4jConfig,
    pub memory: MemoryConfig,
    pub persona: Persona,
    #[serde(default)]
    pub console: ConsoleConfig,
    #[serde(default)]
    pub tools: ToolsConfig,
    #[serde(default)]
    pub qq: QqConfig,
    #[serde(skip)]
    pub api_token: String,
    #[serde(skip)]
    pub llm_key: String,
    #[serde(skip)]
    pub neo4j_password: String,
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolsConfig {
    pub enabled: bool,
    pub base_url: String,
    pub max_calls: usize,
    pub timeout_seconds: u64,
    pub turn_timeout_seconds: u64,
    #[serde(skip)]
    pub token: String,
}
impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: "http://127.0.0.1:8091".into(),
            max_calls: 200,
            timeout_seconds: 30,
            turn_timeout_seconds: 1800,
            token: String::new(),
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QqConfig {
    pub listen: String,
    pub api_base: String,
    pub token_url: String,
    #[serde(skip)]
    pub app_id: String,
    #[serde(skip)]
    pub app_secret: String,
}
impl Default for QqConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:8081".into(),
            api_base: "https://api.bot.qq.com".into(),
            token_url: "https://api.bot.qq.com/app/getAppAccessToken".into(),
            app_id: String::new(),
            app_secret: String::new(),
        }
    }
}
impl QqConfig {
    pub fn enabled(&self) -> bool {
        !self.app_id.is_empty()
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {
    pub base_url: String,
    pub model: String,
    pub temperature: f32,
    pub max_tokens: u32,
    pub timeout_seconds: u64,
    /// Enable only for providers supporting response_format json_schema.
    #[serde(default)]
    pub json_schema: bool,
    #[serde(default = "default_context_tokens")]
    pub context_tokens: usize,
    /// Accepted for old TOML files; generation is now bounded by context_tokens.
    #[serde(default, rename = "context_chars")]
    pub legacy_context_chars: Option<usize>,
    /// Legacy configuration retained for compatibility; compaction uses token budgets.
    pub history_turns: i64,
    #[serde(default = "default_summary_chars")]
    pub summary_chars: usize,
}
fn default_summary_chars() -> usize {
    2000
}
fn default_context_tokens() -> usize {
    100_000
}

#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConsoleConfig {
    pub session_days: u64,
    pub secure_cookie: bool,
    #[serde(skip)]
    pub password: String,
}
impl Default for ConsoleConfig {
    fn default() -> Self {
        Self {
            session_days: 30,
            secure_cookie: false,
            password: String::new(),
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelsConfig {
    pub base_url: String,
    pub dimensions: usize,
    pub timeout_seconds: u64,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Neo4jConfig {
    pub base_url: String,
    pub database: String,
    pub username: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {
    #[serde(default = "default_extract_chars")]
    pub extract_every_chars: usize,
    pub candidate_limit: usize,
    pub recall_limit: usize,
    pub context_chars: usize,
    pub minimum_importance: f64,
    pub minimum_confidence: f64,
    pub half_life_days: f64,
    pub forget_below: f64,
    pub max_attempts: i64,
    pub poll_seconds: u64,
    pub retrieval_timeout_seconds: u64,
    pub rerank_min_score: f64,
    /// Discard the weak tail relative to the strongest reranker match. Set 0 to disable.
    #[serde(default = "default_rerank_relative_min")]
    pub rerank_relative_min: f64,
    /// Independent cosine evidence prevents a generic reranker score from
    /// turning unrelated ANN neighbours into personal context.
    #[serde(default = "default_vector_min_score")]
    pub vector_min_score: f64,
    /// Reject semantically unrelated source quotes before persisting candidates.
    #[serde(default = "default_evidence_min_score")]
    pub evidence_min_score: f64,
    pub allow_sensitive: bool,
}
fn default_extract_chars() -> usize {
    10_000
}
fn default_evidence_min_score() -> f64 {
    0.5
}
fn default_rerank_relative_min() -> f64 {
    0.8
}
fn default_vector_min_score() -> f64 {
    0.4
}
#[derive(Clone, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Persona {
    pub name: String,
    pub description: String,
    pub style: String,
    pub boundaries: String,
    #[serde(default)]
    pub output_format: String,
}

impl Config {
    fn parse_document(text: &str) -> Result<Self> {
        let mut config: Self = toml::from_str(text).context("invalid configuration")?;
        let document: toml::Value = toml::from_str(text)?;
        // Old installations kept the original 16-turn default in their mounted file.
        // Upgrade that default together with the character-to-token migration; an
        // explicit new token configuration or environment override remains authoritative.
        if config.llm.legacy_context_chars.is_some()
            && document["llm"].get("context_tokens").is_none()
            && config.llm.history_turns == 16
        {
            config.llm.history_turns = 1000;
        }
        Ok(config)
    }

    pub fn load(path: &Path) -> Result<Self> {
        if Path::new(".env").exists() {
            dotenvy::from_path(".env").map_err(|_| anyhow::anyhow!("invalid .env file"))?;
        }
        let text = std::fs::read_to_string(path)?;
        let mut config = Self::parse_document(&text)?;
        config.api_token = env::var("MNEME_API_TOKEN").context("MNEME_API_TOKEN is required")?;
        config.llm_key = env::var("MNEME_LLM_API_KEY").context("MNEME_LLM_API_KEY is required")?;
        config.neo4j_password =
            env::var("MNEME_NEO4J_PASSWORD").context("MNEME_NEO4J_PASSWORD is required")?;
        config.console.password = env::var("MNEME_CONSOLE_PASSWORD").unwrap_or_default();
        if let Ok(value) = env::var("MNEME_CONSOLE_SESSION_DAYS") {
            config.console.session_days = value
                .parse()
                .context("invalid MNEME_CONSOLE_SESSION_DAYS")?;
        }
        if let Ok(value) = env::var("MNEME_CONSOLE_SECURE_COOKIE") {
            config.console.secure_cookie = value
                .parse()
                .context("invalid MNEME_CONSOLE_SECURE_COOKIE")?;
        }
        if let Ok(value) = env::var("MNEME_TOOLS_ENABLED") {
            config.tools.enabled = value.parse().context("invalid MNEME_TOOLS_ENABLED")?;
        }
        if let Ok(value) = env::var("MNEME_TOOLS_URL") {
            config.tools.base_url = value;
        }
        if let Ok(value) = env::var("MNEME_TOOLS_MAX_CALLS") {
            config.tools.max_calls = value.parse().context("invalid MNEME_TOOLS_MAX_CALLS")?;
        }
        if let Ok(value) = env::var("MNEME_TOOLS_TURN_TIMEOUT_SECONDS") {
            config.tools.turn_timeout_seconds = value
                .parse()
                .context("invalid MNEME_TOOLS_TURN_TIMEOUT_SECONDS")?;
        }
        config.tools.token = env::var("MNEME_TOOLS_TOKEN").unwrap_or_default();
        if let Ok(value) = env::var("MNEME_LISTEN") {
            config.listen = value;
        }
        if let Ok(value) = env::var("MNEME_DATABASE") {
            config.database = value;
        }
        if let Ok(value) = env::var("MNEME_LLM_BASE_URL") {
            config.llm.base_url = value;
        }
        let base = config.llm.base_url.trim_end_matches('/');
        config.llm.base_url = base
            .strip_suffix("/chat/completions")
            .unwrap_or(base)
            .to_owned();
        let mut llm_url = reqwest::Url::parse(&config.llm.base_url).context("invalid LLM URL")?;
        if llm_url.path() == "/" {
            llm_url.set_path("/v1");
            config.llm.base_url = llm_url.to_string();
        }
        if let Ok(value) = env::var("MNEME_LLM_MODEL")
            && !value.trim().is_empty()
        {
            config.llm.model = value;
        }
        if let Ok(value) = env::var("MNEME_LLM_CONTEXT_TOKENS") {
            config.llm.context_tokens =
                value.parse().context("invalid MNEME_LLM_CONTEXT_TOKENS")?;
        }
        if let Ok(value) = env::var("MNEME_LLM_HISTORY_TURNS") {
            config.llm.history_turns = value.parse().context("invalid MNEME_LLM_HISTORY_TURNS")?;
        }
        if let Ok(value) = env::var("MNEME_MEMORY_EXTRACT_CHARS") {
            config.memory.extract_every_chars = value
                .parse()
                .context("invalid MNEME_MEMORY_EXTRACT_CHARS")?;
        }
        if config.llm.model.starts_with("replace-") {
            config.llm.model = "auto".into();
        }
        config.qq.app_id = env::var("MNEME_QQ_APP_ID").unwrap_or_default();
        config.qq.app_secret = env::var("MNEME_QQ_APP_SECRET").unwrap_or_default();
        if let Ok(value) = env::var("MNEME_QQ_LISTEN") {
            config.qq.listen = value;
        }
        if env::var("MNEME_QQ_SANDBOX").as_deref() == Ok("true") {
            config.qq.api_base = "https://sandbox.api.sgroup.qq.com".into();
        }
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=100_000).contains(&self.memory.extract_every_chars),
            "invalid memory extraction interval"
        );
        ensure!(
            (1..=365).contains(&self.console.session_days),
            "console session_days must be 1..365"
        );
        ensure!(
            self.console.password.is_empty()
                || ((10..=128).contains(&self.console.password.chars().count())
                    && !self.console.password.trim().is_empty()),
            "console password must be 10..128 characters"
        );
        ensure!(
            (500..=4000).contains(&self.llm.summary_chars),
            "summary_chars must be 500..4000"
        );
        if self.tools.enabled {
            ensure!(
                (1..=200).contains(&self.tools.max_calls)
                    && (1..=60).contains(&self.tools.timeout_seconds)
                    && (10..=3600).contains(&self.tools.turn_timeout_seconds),
                "invalid tool limits"
            );
            ensure!(
                self.tools.token.len() >= 32
                    && self.tools.token.is_ascii()
                    && !self
                        .tools
                        .token
                        .bytes()
                        .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
                    && self.tools.token != self.api_token,
                "tools require a separate random token of at least 32 characters"
            );
            let url = reqwest::Url::parse(&self.tools.base_url)?;
            ensure!(
                matches!(url.scheme(), "http" | "https")
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.query().is_none()
                    && url.fragment().is_none(),
                "invalid tools URL"
            );
            ensure!(
                url.scheme() == "https"
                    || matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")),
                "remote tools require HTTPS"
            );
        }
        ensure!(
            self.qq.app_id.is_empty() == self.qq.app_secret.is_empty(),
            "QQ AppID and AppSecret must be configured together"
        );
        if self.qq.enabled() {
            ensure!(
                self.qq.app_id.len() <= 32 && self.qq.app_id.bytes().all(|b| b.is_ascii_digit()),
                "invalid QQ AppID"
            );
            ensure!(
                (8..=256).contains(&self.qq.app_secret.len())
                    && self.qq.app_secret.is_ascii()
                    && !self
                        .qq
                        .app_secret
                        .bytes()
                        .any(|b| b.is_ascii_whitespace() || b.is_ascii_control()),
                "invalid QQ AppSecret"
            );
            self.qq
                .listen
                .parse::<std::net::SocketAddr>()
                .context("invalid QQ listen address")?;
            for url in [&self.qq.api_base, &self.qq.token_url] {
                let parsed = reqwest::Url::parse(url)?;
                ensure!(
                    parsed.scheme() == "https"
                        || (parsed.scheme() == "http"
                            && matches!(
                                parsed.host_str(),
                                Some("127.0.0.1" | "localhost" | "[::1]")
                            )),
                    "QQ URLs require HTTPS"
                );
                ensure!(
                    parsed.username().is_empty()
                        && parsed.password().is_none()
                        && parsed.query().is_none()
                        && parsed.fragment().is_none(),
                    "invalid QQ service URL"
                );
            }
        }
        ensure!(
            self.api_token.len() >= 32
                && self.api_token.is_ascii()
                && !self
                    .api_token
                    .bytes()
                    .any(|c| c.is_ascii_whitespace() || c.is_ascii_control())
                && !self.api_token.starts_with("replace-"),
            "API token must be at least 32 random characters"
        );
        ensure!(
            !self.llm_key.is_empty() && !self.neo4j_password.is_empty(),
            "empty service secret"
        );
        for url in [
            &self.llm.base_url,
            &self.models.base_url,
            &self.neo4j.base_url,
        ] {
            let parsed = reqwest::Url::parse(url)?;
            ensure!(
                matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some(),
                "invalid service URL"
            );
            ensure!(
                parsed.password().is_none()
                    && parsed.username().is_empty()
                    && parsed.query().is_none(),
                "secrets and query strings are not allowed in service URLs"
            );
        }
        self.timezone
            .parse::<chrono::FixedOffset>()
            .context("timezone must be an offset, e.g. +08:00")?;
        ensure!(
            self.models.dimensions == 1024,
            "v1 uses Qwen3 1024-dimensional embeddings; model changes require reindexing"
        );
        ensure!(
            self.memory.candidate_limit > 0 && self.memory.candidate_limit <= 64,
            "candidate_limit must be 1..64"
        );
        ensure!(
            self.memory.recall_limit > 0 && self.memory.recall_limit <= self.memory.candidate_limit,
            "invalid recall_limit"
        );
        ensure!(
            self.memory.half_life_days.is_finite() && self.memory.half_life_days > 0.0,
            "invalid half life"
        );
        for value in [
            self.memory.minimum_importance,
            self.memory.minimum_confidence,
            self.memory.forget_below,
            self.memory.rerank_min_score,
            self.memory.vector_min_score,
            self.memory.evidence_min_score,
            self.memory.rerank_relative_min,
        ] {
            ensure!(
                (0.0..=1.0).contains(&value),
                "memory thresholds must be 0..1"
            );
        }
        ensure!(
            self.memory.max_attempts > 0
                && self.memory.poll_seconds > 0
                && self.memory.retrieval_timeout_seconds > 0,
            "invalid worker settings"
        );
        ensure!(
            self.llm.history_turns > 0
                && self.llm.history_turns <= 10_000
                && self.llm.context_tokens >= 2000
                && self.llm.context_tokens <= 1_000_000,
            "invalid context limits"
        );
        ensure!(
            self.llm.max_tokens > 0
                && self.llm.timeout_seconds > 0
                && self.models.timeout_seconds > 0,
            "invalid model limits"
        );
        ensure!(
            self.memory.context_chars > 0 && self.memory.context_chars <= 16000,
            "invalid memory context limit"
        );
        let persona_chars = self.persona.name.chars().count()
            + self.persona.description.chars().count()
            + self.persona.style.chars().count()
            + self.persona.boundaries.chars().count()
            + self.persona.output_format.chars().count();
        ensure!(
            persona_chars <= 6000 && !self.persona.name.trim().is_empty(),
            "invalid persona size or name"
        );
        ensure!(
            self.llm.context_tokens
                >= crate::tokens::count(&serde_json::to_string(&self.persona)?)
                    + self.llm.max_tokens as usize
                    + 1000,
            "context_tokens must fit the persona, reply reserve, and message overhead"
        );
        ensure!(
            self.llm.temperature.is_finite() && (0.0..=2.0).contains(&self.llm.temperature),
            "invalid temperature"
        );
        ensure!(
            self.neo4j
                .database
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "invalid Neo4j database"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Config;
    #[test]
    fn old_defaults_upgrade_but_explicit_token_and_turn_settings_are_preserved() {
        let legacy = include_str!("../config/default.toml")
            .replace("context_tokens = 100000", "context_chars = 16000")
            .replace("history_turns = 1000", "history_turns = 16");
        let upgraded = Config::parse_document(&legacy).unwrap();
        assert_eq!(upgraded.llm.context_tokens, 100_000);
        assert_eq!(upgraded.llm.history_turns, 1000);
        let custom =
            Config::parse_document(&legacy.replace("history_turns = 16", "history_turns = 50"))
                .unwrap();
        assert_eq!(custom.llm.history_turns, 50);
        let explicit = Config::parse_document(&legacy.replace(
            "context_chars = 16000",
            "context_chars = 16000\ncontext_tokens = 200000",
        ))
        .unwrap();
        assert_eq!(explicit.llm.context_tokens, 200_000);
        assert_eq!(explicit.llm.history_turns, 16);
    }
}
