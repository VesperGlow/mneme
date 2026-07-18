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

#[derive(Debug, Clone, Default)]
pub struct ChatParams {
    pub temperature: f32,
    pub max_tokens: u32,
    pub tools: Option<Vec<Value>>,
    pub response_format: Option<Value>,
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
            },
            Profile::Memory => Endpoint {
                model: &self.cfg.memory_model,
                base_url: &self.cfg.memory_base_url,
                api_key: &self.cfg.memory_api_key,
                extra_headers: &self.cfg.memory_extra_headers,
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
                            "LLM {model} 完成 耗时{:.1}s{tokens}",
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
    fn parse_tool_calls() {
        let data = json!({"choices": [{"message": {"content": null, "tool_calls": [
            {"id": "1", "function": {"name": "search_memories", "arguments": "{}"}}
        ]}}]});
        let parsed = parse_choice(&data).unwrap();
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.content, "");
    }
}
