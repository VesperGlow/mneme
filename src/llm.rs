//! OpenAI-compatible /chat/completions 客户端：带重试、额外请求头、
//! tools 与 response_format 支持。

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::config::Config;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct LlmError {
    pub message: String,
    /// 上游返回的 HTTP 状态码；网络层错误为 None。
    pub status: Option<u16>,
}

impl LlmError {
    fn new(message: impl Into<String>, status: Option<u16>) -> Self {
        Self {
            message: message.into(),
            status,
        }
    }
}

/// 单次调用的 token 用量（上游未返回 usage 时为 0）。
#[derive(Debug, Clone, Copy, Default)]
pub struct TokenUsage {
    pub input: i64,
    pub output: i64,
}

impl std::ops::AddAssign for TokenUsage {
    fn add_assign(&mut self, rhs: Self) {
        self.input += rhs.input;
        self.output += rhs.output;
    }
}

#[derive(Debug, Clone, Default)]
pub struct LlmResponse {
    pub content: String,
    pub tool_calls: Vec<Value>,
    pub usage: TokenUsage,
}

/// 抽象的「思考深度」等级。领域层只认这四档语义值，各家厂商用什么字段表达
/// （reasoning_effort / enable_thinking / thinking.budget_tokens / …）完全由配置里的
/// 「等级 → payload 片段」映射决定，见 Config::*_thinking_map 与 chat() 里的 deep-merge。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Think {
    #[default]
    Off,
    Low,
    Medium,
    High,
}

impl Think {
    /// 在 *_THINKING_MAP_JSON 里查片段用的 key。
    pub fn key(self) -> &'static str {
        match self {
            Think::Off => "off",
            Think::Low => "low",
            Think::Medium => "medium",
            Think::High => "high",
        }
    }

    /// 解析 env 值；接受常见别名。未知值返回 None，由调用方决定是否报错。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().as_str() {
            "off" | "none" | "no" | "false" | "0" => Some(Think::Off),
            "low" | "minimal" | "min" => Some(Think::Low),
            "medium" | "med" | "mid" => Some(Think::Medium),
            "high" | "max" => Some(Think::High),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChatParams {
    pub temperature: f32,
    pub max_tokens: u32,
    pub tools: Option<Vec<Value>>,
    pub response_format: Option<Value>,
    /// 思考深度；配合当前 Profile 的 thinking_map 翻译成具体厂商字段。
    pub think: Think,
}

/// 选择使用哪个模型接入点：对话（grok 等）或记忆（deepseek 等）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Chat,
    Memory,
}

/// 某个 Profile 对应的模型名与接入点（base_url / api_key / 额外请求头）。
struct Endpoint<'a> {
    model: &'a str,
    base_url: &'a str,
    api_key: &'a str,
    extra_headers: &'a [(String, String)],
    /// 「思考等级 → 要合并进 payload 的 JSON 片段」。厂商差异全部落在这里。
    thinking_map: &'a serde_json::Map<String, Value>,
}

pub struct LlmClient {
    cfg: Arc<Config>,
    http: reqwest::Client,
}

impl LlmClient {
    pub fn new(cfg: Arc<Config>) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs_f64(cfg.ai_timeout_seconds))
                .build()?,
            cfg,
        })
    }

    fn endpoint(&self, profile: Profile) -> Endpoint<'_> {
        match profile {
            Profile::Chat => Endpoint {
                model: &self.cfg.chat_model,
                base_url: &self.cfg.ai_base_url,
                api_key: &self.cfg.ai_api_key,
                extra_headers: &self.cfg.ai_extra_headers,
                thinking_map: &self.cfg.ai_thinking_map,
            },
            Profile::Memory => Endpoint {
                model: &self.cfg.memory_model,
                base_url: &self.cfg.memory_base_url,
                api_key: &self.cfg.memory_api_key,
                extra_headers: &self.cfg.memory_extra_headers,
                thinking_map: &self.cfg.memory_thinking_map,
            },
        }
    }

    fn url(base: &str) -> Result<String, LlmError> {
        if base.is_empty() {
            return Err(LlmError::new("尚未配置 AI_BASE_URL", None));
        }
        Ok(if base.ends_with("/chat/completions") {
            base.to_string()
        } else {
            format!("{base}/chat/completions")
        })
    }

    pub async fn chat(
        &self,
        profile: Profile,
        messages: &[Value],
        params: ChatParams,
    ) -> Result<LlmResponse, LlmError> {
        let endpoint = self.endpoint(profile);
        let model = endpoint.model;
        if model.is_empty() {
            return Err(LlmError::new("尚未配置模型名称", None));
        }
        let mut payload = json!({
            "model": model,
            "messages": messages,
            "temperature": params.temperature,
            "max_tokens": params.max_tokens,
        });
        if let Some(tools) = &params.tools {
            payload["tools"] = json!(tools);
            payload["tool_choice"] = json!("auto");
        }
        if let Some(format) = &params.response_format {
            payload["response_format"] = format.clone();
        }
        // 思考深度：把当前等级对应的厂商片段深合并进 payload。片段里值为 null 的键会被
        // 删除（用于去掉某些推理模型不接受的字段，如 o 系的 temperature、grok 的 stop）。
        let think_applied = match endpoint.thinking_map.get(params.think.key()) {
            Some(fragment) => {
                deep_merge(&mut payload, fragment);
                true
            }
            None => false,
        };
        // 请求了非 off 等级却没配对应片段：思考字段没发出去，会被厂商默认行为静默降级。
        let think_label = if !think_applied && params.think != Think::Off {
            format!("{}(未配)", params.think.key())
        } else {
            params.think.key().to_string()
        };
        let url = Self::url(endpoint.base_url)?;
        let started = std::time::Instant::now();

        let mut last_error = String::new();
        for attempt in 0..=self.cfg.ai_max_retries {
            let mut request = self.http.post(&url).json(&payload);
            for (key, value) in endpoint.extra_headers {
                request = request.header(key, value);
            }
            if !endpoint.api_key.is_empty() {
                request = request.bearer_auth(endpoint.api_key);
            }
            match request.send().await {
                Ok(response) => {
                    let status = response.status().as_u16();
                    if status >= 400 {
                        let body = response.text().await.unwrap_or_default();
                        let body: String = body.chars().take(2000).collect();
                        // 只对典型的可恢复状态码重试。
                        if !matches!(status, 408 | 429 | 500 | 502 | 503 | 504) {
                            return Err(LlmError::new(
                                format!("AI 接口返回 HTTP {status}：{body}"),
                                Some(status),
                            ));
                        }
                        tracing::warn!(
                            "LLM {model} 第{}次请求失败：HTTP {status}",
                            attempt + 1
                        );
                        last_error = format!("HTTP {status}：{body}");
                    } else {
                        let data: Value = response.json().await.map_err(|e| {
                            LlmError::new(format!("AI 接口响应不是 JSON：{e}"), None)
                        })?;
                        let usage = TokenUsage {
                            input: data["usage"]["prompt_tokens"].as_i64().unwrap_or(0),
                            output: data["usage"]["completion_tokens"].as_i64().unwrap_or(0),
                        };
                        let tokens = if usage.input > 0 || usage.output > 0 {
                            format!(" tokens={}+{}", usage.input, usage.output)
                        } else {
                            String::new()
                        };
                        tracing::info!(
                            "LLM {model} think={think_label} 完成 耗时{:.1}s{tokens}",
                            started.elapsed().as_secs_f32()
                        );
                        let mut parsed = parse_choice(&data)?;
                        parsed.usage = usage;
                        return Ok(parsed);
                    }
                }
                Err(e) => {
                    tracing::warn!("LLM {model} 第{}次请求失败：{e}", attempt + 1);
                    last_error = e.to_string();
                }
            }
            if attempt < self.cfg.ai_max_retries {
                let backoff = (1u64 << attempt).min(4);
                tokio::time::sleep(Duration::from_secs(backoff)).await;
            }
        }
        Err(LlmError::new(format!("AI 接口请求失败：{last_error}"), None))
    }
}

/// 把 `src` 递归合并进 `dst`：对象逐键合并，其余类型整体覆盖。
/// 约定：`src` 里值为 null 的键表示「从 dst 删除该键」，而不是写入 null。
fn deep_merge(dst: &mut Value, src: &Value) {
    match (dst, src) {
        (Value::Object(d), Value::Object(s)) => {
            for (key, value) in s {
                if value.is_null() {
                    d.remove(key);
                } else {
                    deep_merge(d.entry(key.clone()).or_insert(Value::Null), value);
                }
            }
        }
        (slot, value) => *slot = value.clone(),
    }
}

fn parse_choice(data: &Value) -> Result<LlmResponse, LlmError> {
    let message = data["choices"]
        .as_array()
        .and_then(|choices| choices.first())
        .map(|choice| &choice["message"])
        .ok_or_else(|| {
            let text: String = data.to_string().chars().take(1000).collect();
            LlmError::new(format!("AI 接口未返回 choices：{text}"), None)
        })?;
    // content 可能是字符串，也可能是多段 {type,text} 数组。
    let content = match &message["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| {
                part["text"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| part.as_str().unwrap_or_default().to_string())
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    };
    let tool_calls = message["tool_calls"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    Ok(LlmResponse {
        content,
        tool_calls,
        usage: TokenUsage::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_string_and_parts_content() {
        let data = json!({"choices": [{"message": {"content": "你好"}}]});
        assert_eq!(parse_choice(&data).unwrap().content, "你好");
        let data = json!({"choices": [{"message": {"content": [
            {"type": "text", "text": "你"}, {"type": "text", "text": "好"}
        ]}}]});
        assert_eq!(parse_choice(&data).unwrap().content, "你好");
    }

    #[test]
    fn deep_merge_adds_and_overwrites() {
        let mut base = json!({"model": "m", "temperature": 0.3});
        deep_merge(&mut base, &json!({"reasoning_effort": "high", "temperature": 0.9}));
        assert_eq!(base["reasoning_effort"], json!("high"));
        assert_eq!(base["temperature"], json!(0.9));
        assert_eq!(base["model"], json!("m"));
    }

    #[test]
    fn deep_merge_null_deletes_key() {
        let mut base = json!({"temperature": 0.3, "stop": ["x"]});
        deep_merge(&mut base, &json!({"reasoning_effort": "high", "temperature": null}));
        assert!(base.get("temperature").is_none());
        assert_eq!(base["reasoning_effort"], json!("high"));
        assert_eq!(base["stop"], json!(["x"]));
    }

    #[test]
    fn deep_merge_is_recursive() {
        let mut base = json!({"thinking": {"type": "enabled"}});
        deep_merge(&mut base, &json!({"thinking": {"budget_tokens": 8000}}));
        assert_eq!(base["thinking"]["type"], json!("enabled"));
        assert_eq!(base["thinking"]["budget_tokens"], json!(8000));
    }

    #[test]
    fn think_parse_aliases() {
        assert_eq!(Think::parse("none"), Some(Think::Off));
        assert_eq!(Think::parse("HIGH"), Some(Think::High));
        assert_eq!(Think::parse(" med "), Some(Think::Medium));
        assert_eq!(Think::parse("banana"), None);
    }

    #[test]
    fn parse_tool_calls() {
        let data = json!({"choices": [{"message": {"content": null, "tool_calls": [
            {"id": "1", "function": {"name": "search_memories", "arguments": "{}"}}
        ]}}]});
        let parsed = parse_choice(&data).unwrap();
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.content, "");
    }
}
