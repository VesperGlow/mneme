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
    pub context_chars: usize,
    pub history_turns: i64,
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
    pub allow_sensitive: bool,
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
    pub fn load(path: &Path) -> Result<Self> {
        if Path::new(".env").exists() {
            dotenvy::from_path(".env").map_err(|_| anyhow::anyhow!("invalid .env file"))?;
        }
        let mut config: Self =
            toml::from_str(&std::fs::read_to_string(path)?).context("invalid configuration")?;
        config.api_token = env::var("MNEME_API_TOKEN").context("MNEME_API_TOKEN is required")?;
        config.llm_key = env::var("MNEME_LLM_API_KEY").context("MNEME_LLM_API_KEY is required")?;
        config.neo4j_password =
            env::var("MNEME_NEO4J_PASSWORD").context("MNEME_NEO4J_PASSWORD is required")?;
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
                && self.llm.history_turns <= 100
                && self.llm.context_chars >= 2000
                && self.llm.context_chars <= 100000,
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
            self.llm.context_chars >= persona_chars + self.memory.context_chars + 7000,
            "context_chars must fit the persona, memory JSON, and a 4000-character user message"
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
