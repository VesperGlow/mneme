//! 配置：只保留部署真正必须的少数环境变量，其余全部是本文件里的常量。
//!
//! 模型侧固定为 DeepSeek 官方 API（<https://api.deepseek.com>），思考等级固定 max，
//! 不再有 base_url / 模型名 / 思考映射之类的可配项——换供应商本来就要改代码。

use std::env;

use anyhow::{bail, Context, Result};
use serde::Serialize;

// ---------- DeepSeek 接入点（固定） ----------

pub const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";
/// 对话模型：思考模式恒开、等级 max（见 llm.rs 的 payload 组装）。
pub const CHAT_MODEL: &str = "deepseek-v4-pro";
/// 记忆模型：精选/巩固/摘要这类结构化短任务，用便宜的 flash 且关掉思考。
pub const MEMORY_MODEL: &str = "deepseek-v4-flash";
/// 固定思考等级。DeepSeek 只有 high / max 两档，这里恒取 max。
pub const REASONING_EFFORT: &str = "max";

pub const AI_TIMEOUT_SECONDS: f64 = 300.0;
pub const AI_MAX_RETRIES: u32 = 2;
/// 对话输出上限。思考模式下 CoT 也算输出，留足额度免得答案被截断。
pub const CHAT_MAX_OUTPUT_TOKENS: u32 = 8192;
pub const MEMORY_MAX_OUTPUT_TOKENS: u32 = 800;
pub const MAX_TOOL_ROUNDS: u32 = 6;

// ---------- 服务与日志 ----------

/// 容器内监听端口，对外映射由 compose 决定。
pub const API_PORT: u16 = 8000;
/// 运行日志里消息/记忆内容预览的最大字符数。
pub const LOG_PREVIEW_CHARS: usize = 40;
/// 请求体上限（纯文本对话，1MB 绰绰有余）。
pub const API_BODY_LIMIT: usize = 1_048_576;
/// 收到 SIGTERM/Ctrl-C 后等待在途消息与落库任务完成的上限（秒）。
/// 容器编排的 stop 宽限期要不小于该值。
pub const SHUTDOWN_TIMEOUT_SECONDS: u64 = 30;

// ---------- 工具 ----------

pub const MCP_TIMEOUT_SECONDS: f64 = 300.0;
pub const MCP_RESULT_MAX_CHARS: usize = 12000;
pub const FETCH_TIMEOUT_SECONDS: f64 = 30.0;
pub const FETCH_MAX_BYTES: usize = 5_242_880;
pub const FETCH_RESULT_MAX_CHARS: usize = 12000;

// ---------- 记忆策略 ----------

/// 检索最终注入上下文的记忆条数上限。
pub const MEMORY_SEARCH_LIMIT: usize = 8;
pub const MEMORY_HISTORY_MESSAGES: i64 = 16;
/// 近似去重阈值：字符三元组 Jaccard ≥ 此值即并入旧记忆。
/// 0.9 ≈「只差标点或一两个语气词」；「喜欢 X」与「不喜欢 X」只有 0.3 左右，不会被误合并。
pub const MEMORY_DUPLICATE_THRESHOLD: f32 = 0.9;
/// 精选候选池上限：按 last_seen_at 倒序取这么多条活跃记忆交给记忆模型挑。
pub const MEMORY_SELECT_POOL_MAX: usize = 400;
/// 候选清单里每条记忆的截断长度（字符）。
pub const MEMORY_SELECT_TEXT_MAX_CHARS: usize = 200;
/// 精选时查询文本的截断长度（字符）。
pub const MEMORY_SELECT_QUERY_MAX_CHARS: usize = 2000;
/// 精选调用的输出上限：只输出一个编号数组。
pub const MEMORY_SELECT_MAX_OUTPUT_TOKENS: u32 = 300;
/// 触发一次巩固所需的「已滑出窗口、尚未巩固」的最少消息数。
pub const MEMORY_CONSOLIDATE_BATCH: usize = 6;
/// 会话最后活动早于「现在 - 该秒数」才算空闲、可被尾巴 flush。
pub const MEMORY_FLUSH_IDLE_SECONDS: u64 = 900;
/// 尾巴 flush 的扫描周期（秒）。
pub const MEMORY_FLUSH_INTERVAL_SECONDS: u64 = 300;
/// 累计这么多条「已滑出窗口且未摘要」的消息才更新一次滚动摘要。
pub const CONVERSATION_SUMMARY_BATCH: usize = 10;
pub const CONVERSATION_SUMMARY_MAX_CHARS: usize = 1000;
pub const MOOD_TREND_DAYS: i64 = 7;

// ---------- QQ 桥接 ----------

pub const QQ_LISTEN_ADDR: &str = "0.0.0.0:9000";
pub const QQ_WEBHOOK_PATH: &str = "/qqbot";
pub const QQ_AI_TIMEOUT_SECONDS: u64 = 300;
pub const QQ_OPENAPI_TIMEOUT_SECONDS: u64 = 15;
/// 长回复自动分片；QQ 对同一消息的被动回复最多 5 次，这里保守用 4 次。
pub const QQ_REPLY_MAX_RUNES: usize = 1800;
pub const QQ_REPLY_MAX_PARTS: usize = 4;
pub const QQ_WORKERS: usize = 8;
pub const QQ_QUEUE_SIZE: usize = 128;
pub const QQ_DEDUP_TTL_SECONDS: u64 = 600;
pub const QQ_MAX_WEBHOOK_BYTES: usize = 1_048_576;

fn env_string(name: &str, fallback: &str) -> String {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => fallback.to_string(),
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
pub enum QqEventMode {
    Webhook,
    WebSocket,
}

/// 环境变量能配的全部内容。想调别的行为请改上面的常量。
#[derive(Debug, Clone)]
pub struct Config {
    /// 对外 API 鉴权；留空则不鉴权。
    pub app_api_key: String,
    pub log_level: String,
    /// 全局人设（只写性格/口吻）；留空用内置默认人设。
    pub persona_prompt: String,
    /// 系统指令层（输出格式/记忆工具/安全），优先级高于人设；
    /// 留空用 `agent::DEFAULT_SYSTEM_INSTRUCTIONS`。多行用字面量 \n 分隔。
    pub system_instructions: String,
    pub deepseek_api_key: String,
    pub db_path: String,
    pub mcp_servers: Vec<McpServer>,
    pub qq_app_id: String,
    pub qq_app_secret: String,
    pub qq_event_mode: QqEventMode,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let qq_event_mode = match env_string("QQ_EVENT_MODE", "webhook").to_lowercase().as_str() {
            "webhook" => QqEventMode::Webhook,
            "websocket" => QqEventMode::WebSocket,
            other => bail!("QQ_EVENT_MODE 必须是 webhook 或 websocket，当前是 {other}"),
        };
        Ok(Self {
            app_api_key: env_string("APP_API_KEY", ""),
            log_level: env_string("LOG_LEVEL", "INFO"),
            persona_prompt: env_string("PERSONA_PROMPT", ""),
            system_instructions: env_string("SYSTEM_INSTRUCTIONS", ""),
            deepseek_api_key: env_string("DEEPSEEK_API_KEY", ""),
            db_path: env_string("DB_PATH", "/data/memory.db"),
            mcp_servers: parse_mcp_servers(&env_string("MCP_SERVERS_JSON", "[]"))?,
            qq_app_id: env_string("QQ_APP_ID", ""),
            qq_app_secret: env_string("QQ_APP_SECRET", ""),
            qq_event_mode,
        })
    }

    /// /health 与 /v1/config 暴露的脱敏配置摘要。
    pub fn safe_summary(&self) -> serde_json::Value {
        #[derive(Serialize)]
        struct Summary<'a> {
            provider: &'a str,
            base_url: &'a str,
            chat_model: &'a str,
            memory_model: &'a str,
            reasoning_effort: &'a str,
            db_path: &'a str,
            mcp_servers: Vec<&'a str>,
        }
        serde_json::to_value(Summary {
            provider: "deepseek",
            base_url: DEEPSEEK_BASE_URL,
            chat_model: CHAT_MODEL,
            memory_model: MEMORY_MODEL,
            reasoning_effort: REASONING_EFFORT,
            db_path: &self.db_path,
            mcp_servers: self.mcp_servers.iter().map(|s| s.name.as_str()).collect(),
        })
        .expect("safe summary 序列化不应失败")
    }
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
