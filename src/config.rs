//! 环境变量配置：变量名保持稳定，现有 .env 无需改动。

use std::env;

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::llm::Think;

fn env_string(name: &str, fallback: &str) -> String {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => fallback.to_string(),
    }
}

fn env_parse<T: std::str::FromStr>(name: &str, fallback: T) -> T {
    env::var(name)
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(fallback)
}

fn env_bool(name: &str, fallback: bool) -> bool {
    match env::var(name) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => fallback,
    }
}

fn clamp<T: PartialOrd>(value: T, min: T, max: T) -> T {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

/// 在字符串里展开 ${NAME} / $NAME 环境变量引用（MCP 配置用）。
pub fn expand_env_refs(value: &str) -> Result<String> {
    let mut result = String::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() {
            let (name, consumed) = if bytes[i + 1] == b'{' {
                let end = value[i + 2..]
                    .find('}')
                    .with_context(|| format!("MCP 配置里有未闭合的 ${{：{value}"))?;
                (&value[i + 2..i + 2 + end], end + 3)
            } else {
                let rest = &value[i + 1..];
                let end = rest
                    .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .unwrap_or(rest.len());
                (&rest[..end], end + 1)
            };
            if name.is_empty() {
                result.push('$');
                i += 1;
                continue;
            }
            let resolved = env::var(name)
                .with_context(|| format!("MCP_SERVERS_JSON 引用了未设置的环境变量 {name}"))?;
            result.push_str(&resolved);
            i += consumed;
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }
    Ok(result)
}

#[derive(Debug, Clone)]
pub struct McpServer {
    pub name: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingStyle {
    Local,
    OpenAi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QqEventMode {
    Webhook,
    WebSocket,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub app_api_key: String,
    pub log_level: String,
    /// 运行日志里消息/记忆内容预览的最大字符数；0 = 完全不在日志里出现内容。
    pub log_preview_chars: usize,
    pub persona_prompt: String,
    pub system_instructions: String,

    pub ai_base_url: String,
    pub ai_api_key: String,
    pub memory_model: String,
    pub chat_model: String,
    /// 记忆模型的独立接入点；为空时回退到 ai_* 共享配置。
    /// 这样对话模型可用 grok、廉价的记忆模型可用 deepseek 等不同供应商。
    pub memory_base_url: String,
    pub memory_api_key: String,
    pub memory_extra_headers: Vec<(String, String)>,
    pub ai_timeout_seconds: f64,
    pub ai_max_retries: u32,
    pub ai_extra_headers: Vec<(String, String)>,
    /// 对话调用的思考深度；配 ai_thinking_map 翻译成具体厂商字段。
    pub chat_think: Think,
    /// 「思考等级 → 要合并进请求体的 JSON 片段」映射，对话/记忆两个接入点各一份。
    /// 记忆侧调用（判定、摘要）恒为 off，一般无需配 memory 映射。
    pub ai_thinking_map: Map<String, Value>,
    pub memory_thinking_map: Map<String, Value>,
    pub chat_max_output_tokens: u32,
    pub memory_max_output_tokens: u32,
    pub max_tool_rounds: u32,

    pub mcp_servers: Vec<McpServer>,
    pub mcp_timeout_seconds: f64,
    pub mcp_result_max_chars: usize,

    /// 内置网页抓取工具（fetch_url）：拉取公开链接抽正文转 Markdown，不渲染 JS。
    pub fetch_url_enabled: bool,
    pub fetch_timeout_seconds: f64,
    pub fetch_max_bytes: usize,
    pub fetch_result_max_chars: usize,

    pub db_path: String,

    pub embedding_api_style: EmbeddingStyle,
    pub embedding_model: String,
    pub embedding_base_url: String,
    pub embedding_api_key: String,
    pub embedding_dimensions: usize,
    pub embedding_context_size: usize,
    pub embedding_query_instruction: String,
    pub embedding_timeout_seconds: f64,
    pub embedding_threads: usize,
    pub embedding_output_min: f32,
    pub embedding_output_max: f32,
    pub hf_token: String,

    /// 二段精排前，一段余弦召回返回给上层的最终条数上限。
    pub memory_search_limit: usize,
    pub memory_min_score: f32,
    pub memory_history_messages: i64,
    pub memory_duplicate_threshold: f32,
    pub memory_judge_skip_trivial: bool,

    /// 重排（rerank）：本地 ONNX 交叉编码器给 (query, 候选) 联合打分做二段精排。
    /// 关闭、模型加载失败或推理出错时，自动回退到一段余弦顺序，服务不受影响。
    pub rerank_enabled: bool,
    pub rerank_model: String,
    /// 从仓库多个 onnx 变体里挑一个的偏好子串（如 "quantized"/"q4"/"fp16"）；
    /// onnx-community 这类仓库把多份量化权重放在 onnx/ 子目录，必须挑一个而不是全下。
    pub rerank_onnx_file: String,
    /// 一段余弦召回的候选宽度：喂给重排器的最多条数（再由重排精排到 memory_search_limit）。
    pub rerank_candidates: usize,
    pub rerank_context_size: usize,
    pub rerank_threads: usize,
    pub rerank_instruction: String,

    pub conversation_summary_enabled: bool,
    pub conversation_summary_batch: usize,
    pub conversation_summary_max_chars: usize,

    /// 图片理解：把 QQ 图片附件 / API images 参数以 image_url 段传给对话模型（模型须支持视觉）。
    pub chat_image_enabled: bool,
    pub chat_image_max_count: usize,
    /// 发送给模型的单张图片大小上限；超限的图会先压缩（缩放 + 重编码 JPEG）。
    pub chat_image_max_bytes: usize,
    /// 从 QQ CDN 下载原图的大小上限（压缩前），防滥用兜底。
    pub chat_image_fetch_max_bytes: usize,
    /// 图片长边像素上限，超过则缩放；视觉模型内部分辨率有限，缩了还省 token。
    pub chat_image_max_edge: u32,

    pub mood_tracking_enabled: bool,
    pub mood_trend_days: i64,
    pub time_awareness_enabled: bool,

    pub app_port: u16,

    /// 收到 SIGTERM/Ctrl-C 后等待在途消息与落库任务完成的上限（秒）。
    /// 注意容器编排的 stop 宽限期（如 podman stop -t / stop_grace_period）要不小于该值。
    pub shutdown_timeout_seconds: u64,

    // QQ 桥接
    pub qq_app_id: String,
    pub qq_app_secret: String,
    pub qq_event_mode: QqEventMode,
    pub qq_listen_addr: String,
    pub qq_webhook_path: String,
    pub qq_ai_timeout_seconds: u64,
    pub qq_openapi_timeout_seconds: u64,
    pub qq_reply_max_runes: usize,
    pub qq_reply_max_parts: usize,
    pub qq_workers: usize,
    pub qq_queue_size: usize,
    pub qq_dedup_ttl_seconds: u64,
    pub qq_max_webhook_bytes: usize,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let mcp_servers = parse_mcp_servers(&env_string("MCP_SERVERS_JSON", "[]"))?;
        let ai_extra_headers = parse_headers(&env_string("AI_EXTRA_HEADERS_JSON", "{}"))
            .context("AI_EXTRA_HEADERS_JSON 必须是 JSON 对象")?;

        // 记忆模型接入点：未单独设置时回退到共享的 ai_* 配置。
        let memory_base_url = {
            let raw = env_string("MEMORY_BASE_URL", "").trim_end_matches('/').to_string();
            if raw.is_empty() {
                env_string("AI_BASE_URL", "").trim_end_matches('/').to_string()
            } else {
                raw
            }
        };
        let memory_api_key = {
            let raw = env_string("MEMORY_API_KEY", "");
            if raw.is_empty() {
                env_string("AI_API_KEY", "")
            } else {
                raw
            }
        };
        let memory_extra_headers = match env::var("MEMORY_EXTRA_HEADERS_JSON") {
            Ok(value) if !value.trim().is_empty() => parse_headers(&value)
                .context("MEMORY_EXTRA_HEADERS_JSON 必须是 JSON 对象")?,
            _ => ai_extra_headers.clone(),
        };

        let chat_think = {
            let raw = env_string("CHAT_THINK", "high");
            Think::parse(&raw)
                .with_context(|| format!("CHAT_THINK 只能是 off/low/medium/high，当前是 {raw}"))?
        };
        let ai_thinking_map = parse_thinking_map(&env_string("AI_THINKING_MAP_JSON", "{}"))
            .context("AI_THINKING_MAP_JSON 必须是 {等级: {字段片段}} 形式的 JSON")?;
        // 未单独设置记忆映射时，与接入点回退逻辑一致：沿用对话侧映射。
        let memory_thinking_map = match env::var("MEMORY_THINKING_MAP_JSON") {
            Ok(value) if !value.trim().is_empty() => parse_thinking_map(&value)
                .context("MEMORY_THINKING_MAP_JSON 必须是 {等级: {字段片段}} 形式的 JSON")?,
            _ => ai_thinking_map.clone(),
        };

        let embedding_api_style = match env_string("EMBEDDING_API_STYLE", "local").as_str() {
            "local" => EmbeddingStyle::Local,
            "openai" => EmbeddingStyle::OpenAi,
            other => bail!("EMBEDDING_API_STYLE 只能是 local 或 openai，当前是 {other}"),
        };
        let qq_event_mode = match env_string("QQ_EVENT_MODE", "webhook").to_lowercase().as_str() {
            "webhook" => QqEventMode::Webhook,
            "websocket" => QqEventMode::WebSocket,
            other => bail!("QQ_EVENT_MODE 必须是 webhook 或 websocket，当前是 {other}"),
        };

        let mut qq_webhook_path = env_string("QQ_WEBHOOK_PATH", "/qqbot");
        if !qq_webhook_path.starts_with('/') {
            qq_webhook_path.insert(0, '/');
        }

        Ok(Self {
            app_api_key: env_string("APP_API_KEY", ""),
            log_level: env_string("LOG_LEVEL", "INFO"),
            log_preview_chars: clamp(env_parse("LOG_CONTENT_PREVIEW_CHARS", 40), 0, 500),
            persona_prompt: env_string("PERSONA_PROMPT", ""),
            system_instructions: env_string("SYSTEM_INSTRUCTIONS", ""),

            ai_base_url: env_string("AI_BASE_URL", "").trim_end_matches('/').to_string(),
            ai_api_key: env_string("AI_API_KEY", ""),
            memory_model: env_string("MEMORY_MODEL", ""),
            chat_model: env_string("CHAT_MODEL", ""),
            memory_base_url,
            memory_api_key,
            memory_extra_headers,
            ai_timeout_seconds: env_parse("AI_TIMEOUT_SECONDS", 120.0),
            ai_max_retries: env_parse("AI_MAX_RETRIES", 2),
            ai_extra_headers,
            chat_think,
            ai_thinking_map,
            memory_thinking_map,
            chat_max_output_tokens: env_parse("CHAT_MAX_OUTPUT_TOKENS", 2048),
            memory_max_output_tokens: env_parse("MEMORY_MAX_OUTPUT_TOKENS", 800),
            max_tool_rounds: env_parse("MAX_TOOL_ROUNDS", 6),

            mcp_servers,
            mcp_timeout_seconds: env_parse("MCP_TIMEOUT_SECONDS", 300.0),
            mcp_result_max_chars: env_parse("MCP_RESULT_MAX_CHARS", 12000),

            fetch_url_enabled: env_bool("FETCH_URL_ENABLED", true),
            fetch_timeout_seconds: env_parse("FETCH_TIMEOUT_SECONDS", 30.0),
            fetch_max_bytes: clamp(env_parse("FETCH_MAX_BYTES", 5_242_880), 65_536, 52_428_800),
            fetch_result_max_chars: clamp(env_parse("FETCH_RESULT_MAX_CHARS", 12000), 500, 60000),

            db_path: env_string("DB_PATH", "/data/memory.db"),

            embedding_api_style,
            embedding_model: env_string(
                "EMBEDDING_MODEL",
                "electroglyph/Qwen3-Embedding-0.6B-onnx-uint8",
            ),
            embedding_base_url: env_string("EMBEDDING_BASE_URL", "")
                .trim_end_matches('/')
                .to_string(),
            embedding_api_key: env_string("EMBEDDING_API_KEY", ""),
            embedding_dimensions: clamp(env_parse("EMBEDDING_DIMENSIONS", 1024), 32, 4096),
            embedding_context_size: clamp(env_parse("EMBEDDING_CONTEXT_SIZE", 512), 64, 32768),
            embedding_query_instruction: env_string(
                "EMBEDDING_QUERY_INSTRUCTION",
                "Given a user's message, retrieve memories that are useful for personalizing the response",
            ),
            embedding_timeout_seconds: env_parse("EMBEDDING_TIMEOUT_SECONDS", 180.0),
            embedding_threads: clamp(env_parse("EMBEDDING_THREADS", 4), 1, 32),
            embedding_output_min: env_parse("EMBEDDING_OUTPUT_MIN", -0.3009),
            embedding_output_max: env_parse("EMBEDDING_OUTPUT_MAX", 0.3952),
            hf_token: env_string("HF_TOKEN", ""),

            memory_search_limit: clamp(env_parse("MEMORY_SEARCH_LIMIT", 8), 1, 50),
            memory_min_score: env_parse("MEMORY_MIN_SCORE", 0.30),
            memory_history_messages: clamp(env_parse("MEMORY_HISTORY_MESSAGES", 16), 0, 100),
            memory_duplicate_threshold: env_parse("MEMORY_DUPLICATE_THRESHOLD", 0.995),
            memory_judge_skip_trivial: env_bool("MEMORY_JUDGE_SKIP_TRIVIAL", true),

            rerank_enabled: env_bool("RERANK_ENABLED", true),
            rerank_model: env_string("RERANK_MODEL", "onnx-community/Qwen3-Reranker-0.6B-ONNX"),
            rerank_onnx_file: env_string("RERANK_ONNX_FILE", "quantized"),
            rerank_candidates: clamp(env_parse("RERANK_CANDIDATES", 50), 1, 500),
            rerank_context_size: clamp(env_parse("RERANK_CONTEXT_SIZE", 512), 64, 32768),
            rerank_threads: clamp(env_parse("RERANK_THREADS", 4), 1, 32),
            rerank_instruction: env_string(
                "RERANK_INSTRUCTION",
                "Given the user's message, judge whether the memory is useful for personalizing the reply",
            ),

            conversation_summary_enabled: env_bool("CONVERSATION_SUMMARY_ENABLED", true),
            conversation_summary_batch: clamp(env_parse("CONVERSATION_SUMMARY_BATCH", 10), 2, 100),
            conversation_summary_max_chars: clamp(
                env_parse("CONVERSATION_SUMMARY_MAX_CHARS", 1000),
                100,
                8000,
            ),

            chat_image_enabled: env_bool("CHAT_IMAGE_ENABLED", true),
            chat_image_max_count: clamp(env_parse("CHAT_IMAGE_MAX_COUNT", 3), 1, 10),
            chat_image_max_bytes: clamp(env_parse("CHAT_IMAGE_MAX_BYTES", 5_242_880), 65_536, 20_971_520),
            chat_image_fetch_max_bytes: clamp(
                env_parse("CHAT_IMAGE_FETCH_MAX_BYTES", 31_457_280),
                1_048_576,
                104_857_600,
            ),
            chat_image_max_edge: clamp(env_parse("CHAT_IMAGE_MAX_EDGE", 2048), 512, 8192),

            mood_tracking_enabled: env_bool("MOOD_TRACKING_ENABLED", true),
            mood_trend_days: clamp(env_parse("MOOD_TREND_DAYS", 7), 1, 90),
            time_awareness_enabled: env_bool("TIME_AWARENESS_ENABLED", true),

            app_port: env_parse("APP_PORT_INTERNAL", 8000),

            shutdown_timeout_seconds: clamp(env_parse("SHUTDOWN_TIMEOUT_SECONDS", 30), 1, 600),

            qq_app_id: env_string("QQ_APP_ID", ""),
            qq_app_secret: env_string("QQ_APP_SECRET", ""),
            qq_event_mode,
            qq_listen_addr: env_string("QQ_LISTEN_ADDR", ":9000"),
            qq_webhook_path,
            qq_ai_timeout_seconds: clamp(env_parse("QQ_AI_TIMEOUT_SECONDS", 180), 5, 600),
            qq_openapi_timeout_seconds: clamp(env_parse("QQ_OPENAPI_TIMEOUT_SECONDS", 15), 5, 60),
            qq_reply_max_runes: clamp(env_parse("QQ_REPLY_MAX_RUNES", 1800), 200, 10000),
            qq_reply_max_parts: clamp(env_parse("QQ_REPLY_MAX_PARTS", 4), 1, 5),
            qq_workers: clamp(env_parse("QQ_WORKERS", 8), 1, 64),
            qq_queue_size: clamp(env_parse("QQ_QUEUE_SIZE", 128), 1, 10000),
            qq_dedup_ttl_seconds: clamp(env_parse("QQ_DEDUP_TTL_SECONDS", 600), 60, 86400),
            qq_max_webhook_bytes: clamp(env_parse("QQ_MAX_WEBHOOK_BYTES", 1048576), 4096, 10485760),
        })
    }

    /// /health 与 /v1/config 暴露的脱敏配置摘要。
    pub fn safe_summary(&self) -> serde_json::Value {
        #[derive(Serialize)]
        struct Summary<'a> {
            ai_base_url: &'a str,
            memory_base_url: &'a str,
            memory_model: &'a str,
            chat_model: &'a str,
            chat_think: &'a str,
            chat_thinking_levels: Vec<&'a str>,
            embedding_api_style: &'a str,
            embedding_model: &'a str,
            embedding_dimensions: usize,
            embedding_context_size: usize,
            db_path: &'a str,
            rerank_enabled: bool,
            rerank_model: &'a str,
            mcp_servers: Vec<&'a str>,
        }
        serde_json::to_value(Summary {
            ai_base_url: &self.ai_base_url,
            memory_base_url: &self.memory_base_url,
            memory_model: &self.memory_model,
            chat_model: &self.chat_model,
            chat_think: self.chat_think.key(),
            chat_thinking_levels: self.ai_thinking_map.keys().map(String::as_str).collect(),
            embedding_api_style: match self.embedding_api_style {
                EmbeddingStyle::Local => "local",
                EmbeddingStyle::OpenAi => "openai",
            },
            embedding_model: &self.embedding_model,
            embedding_dimensions: self.embedding_dimensions,
            embedding_context_size: self.embedding_context_size,
            db_path: &self.db_path,
            rerank_enabled: self.rerank_enabled,
            rerank_model: &self.rerank_model,
            mcp_servers: self.mcp_servers.iter().map(|s| s.name.as_str()).collect(),
        })
        .expect("safe summary 序列化不应失败")
    }
}

fn parse_headers(raw: &str) -> Result<Vec<(String, String)>> {
    let value: serde_json::Value = serde_json::from_str(if raw.is_empty() { "{}" } else { raw })?;
    let object = value.as_object().context("需要 JSON 对象")?;
    Ok(object
        .iter()
        .map(|(k, v)| {
            let text = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
            (k.clone(), text)
        })
        .collect())
}

/// 解析 *_THINKING_MAP_JSON：形如 {"high": {"reasoning_effort": "high"}, ...}。
/// 顶层 key 必须是合法思考等级（off/low/medium/high 或其别名，归一化到标准 key），
/// 对应的值必须是对象（要合并进请求体的字段片段）。
fn parse_thinking_map(raw: &str) -> Result<Map<String, Value>> {
    let value: Value = serde_json::from_str(if raw.trim().is_empty() { "{}" } else { raw })
        .context("必须是合法 JSON")?;
    let object = value.as_object().context("需要 JSON 对象")?;
    let mut map = Map::new();
    for (key, fragment) in object {
        let level = Think::parse(key)
            .with_context(|| format!("未知的思考等级 key：{key}（应为 off/low/medium/high）"))?;
        if !fragment.is_object() {
            bail!("思考等级 {key} 的值必须是对象，例如 {{\"reasoning_effort\": \"high\"}}");
        }
        // 归一化到标准 key，别名（none/minimal/max…）不至于查不到。
        if map.insert(level.key().to_string(), fragment.clone()).is_some() {
            bail!("思考等级 {} 被重复定义（注意别名会归一）", level.key());
        }
    }
    Ok(map)
}

fn parse_mcp_servers(raw: &str) -> Result<Vec<McpServer>> {
    let value: serde_json::Value =
        serde_json::from_str(raw).context("MCP_SERVERS_JSON 必须是合法 JSON")?;
    let items = value.as_array().context("MCP_SERVERS_JSON 必须是 JSON 数组")?;
    let mut servers = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (index, item) in items.iter().enumerate() {
        let object = item
            .as_object()
            .with_context(|| format!("MCP_SERVERS_JSON[{index}] 必须是对象"))?;
        if !object.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true) {
            continue;
        }
        let name = object
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let url_raw = object.get("url").and_then(|v| v.as_str()).unwrap_or("").trim();
        if name.is_empty() || url_raw.is_empty() {
            bail!("MCP_SERVERS_JSON[{index}] 缺少 name 或 url");
        }
        if !seen.insert(name.clone()) {
            bail!("MCP_SERVERS_JSON 出现重复的 name：{name}");
        }
        if let Some(transport) = object.get("transport").and_then(|v| v.as_str()) {
            if transport != "streamable_http" {
                bail!("MCP_SERVERS_JSON[{index}] 的 transport 目前只支持 streamable_http");
            }
        }
        let url = expand_env_refs(url_raw)?;
        let mut headers = Vec::new();
        if let Some(header_map) = object.get("headers").and_then(|v| v.as_object()) {
            for (k, v) in header_map {
                let text = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
                headers.push((k.clone(), expand_env_refs(&text)?));
            }
        }
        let string_list = |key: &str| -> Vec<String> {
            object
                .get(key)
                .and_then(|v| v.as_array())
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default()
        };
        servers.push(McpServer {
            name,
            url,
            headers,
            include: string_list("tools"),
            exclude: string_list("exclude"),
        });
    }
    Ok(servers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_refs_braced_and_bare() {
        std::env::set_var("MNEME_TEST_KEY", "tvly-123");
        assert_eq!(
            expand_env_refs("https://x/?k=${MNEME_TEST_KEY}&v=$MNEME_TEST_KEY/mcp").unwrap(),
            "https://x/?k=tvly-123&v=tvly-123/mcp"
        );
    }

    #[test]
    fn expand_refs_missing_var_fails() {
        assert!(expand_env_refs("${MNEME_TEST_MISSING_VAR}").is_err());
    }

    #[test]
    fn mcp_servers_parse_with_filters() {
        std::env::set_var("MNEME_TEST_KEY2", "fc-abc");
        let servers = parse_mcp_servers(
            r#"[{"name":"firecrawl","url":"https://mcp.firecrawl.dev/${MNEME_TEST_KEY2}/v2/mcp","tools":["firecrawl_scrape"]},{"name":"off","url":"https://x","enabled":false}]"#,
        )
        .unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].url, "https://mcp.firecrawl.dev/fc-abc/v2/mcp");
        assert_eq!(servers[0].include, vec!["firecrawl_scrape"]);
    }
}
