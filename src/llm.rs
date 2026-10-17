use crate::{config::Config, domain::ChatMessage};
use anyhow::{Result, bail, ensure};
use axum::response::sse::Event;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::mpsc;

/// Compatible providers still need a model ID; discover it only when explicitly automatic.
pub async fn resolve_model(config: &mut Config) -> Result<()> {
    if config.llm.model != "auto" {
        return Ok(());
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = client
        .get(format!(
            "{}/models",
            config.llm.base_url.trim_end_matches('/')
        ))
        .bearer_auth(&config.llm_key)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("model discovery failed; set MNEME_LLM_MODEL explicitly"))?;
    ensure!(
        response.status().is_success(),
        "model catalog unavailable; set MNEME_LLM_MODEL explicitly"
    );
    let value: Value = serde_json::from_slice(&bounded_body(response, 1_048_576).await?)?;
    let model = value["data"]
        .as_array()
        .and_then(|models| {
            models.iter().filter_map(|m| m["id"].as_str()).find(|id| {
                let lower = id.to_lowercase();
                !id.is_empty()
                    && id.len() <= 256
                    && ![
                        "embed",
                        "rerank",
                        "whisper",
                        "tts",
                        "image",
                        "dall-e",
                        "moderation",
                        "transcrib",
                    ]
                    .iter()
                    .any(|kind| lower.contains(kind))
            })
        })
        .ok_or_else(|| anyhow::anyhow!("no chat model found; set MNEME_LLM_MODEL explicitly"))?;
    config.llm.model = model.into();
    tracing::info!(model=%config.llm.model,"automatically selected first chat model from provider catalog; override with MNEME_LLM_MODEL");
    Ok(())
}

#[derive(Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct ToolFunction {
    pub name: String,
    pub arguments: String,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: ToolFunction,
}
pub struct Completion {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub reasoning_content: Option<String>,
    pub tool_markup: bool,
}

// Hold only a possible opening marker across SSE chunks. Raw tool syntax never
// reaches a client and is never interpreted as an executable tool call.
#[derive(Default)]
struct ToolTextGuard {
    pending: String,
    suppressed: bool,
}
impl ToolTextGuard {
    fn push(&mut self, delta: &str) -> String {
        if self.suppressed {
            return String::new();
        }
        self.pending.push_str(delta);
        let mut visible = String::new();
        while !self.pending.is_empty() {
            if !self.pending.starts_with('<') {
                let end = self.pending.find('<').unwrap_or(self.pending.len());
                visible.extend(self.pending.drain(..end));
                continue;
            }
            let prefix: String = self
                .pending
                .chars()
                .filter(|c| !matches!(c, '|' | '｜') && !c.is_whitespace())
                .take(8)
                .collect();
            if prefix.starts_with("<DSML") || prefix.starts_with("</DSML") {
                self.suppressed = true;
                self.pending.clear();
                break;
            }
            if ("<DSML".starts_with(&prefix) || "</DSML".starts_with(&prefix))
                && self.pending.len() <= 256
            {
                break;
            }
            visible.push('<');
            self.pending.remove(0);
        }
        visible
    }
    fn finish(&mut self) -> String {
        if self.pending.contains(['|', '｜']) && self.pending.contains('D') {
            self.suppressed = true;
            self.pending.clear();
        }
        if self.suppressed {
            String::new()
        } else {
            std::mem::take(&mut self.pending)
        }
    }
}

#[derive(Clone)]
pub struct Llm {
    client: reqwest::Client,
    url: String,
    key: String,
    model: String,
    temperature: f32,
    max_tokens: u32,
    pub metrics: std::sync::Arc<crate::telemetry::Metrics>,
}

impl Llm {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(config.llm.timeout_seconds))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            url: format!(
                "{}/chat/completions",
                config.llm.base_url.trim_end_matches('/')
            ),
            key: config.llm_key.clone(),
            model: config.llm.model.clone(),
            temperature: config.llm.temperature,
            max_tokens: config.llm.max_tokens,
            metrics: Default::default(),
        })
    }

    pub async fn complete_json(&self, system: &str, input: Value) -> Result<Value> {
        let start = std::time::Instant::now();
        let result = self.complete_json_inner(system, input).await;
        self.metrics.record(start, result.is_ok());
        if let Err(error) = &result {
            crate::diagnostics::warn("llm.extract", error);
        }
        result
    }
    async fn complete_json_inner(&self, system: &str, input: Value) -> Result<Value> {
        let response = self.client.post(&self.url).bearer_auth(&self.key)
            .json(&json!({"model":self.model,"temperature":0,"max_tokens":2500,"stream":false,
                "messages":[{"role":"system","content":system},{"role":"user","content":input.to_string()}]}))
            .send().await?.error_for_status()?;
        let value: Value = serde_json::from_slice(&bounded_body(response, 1_048_576).await?)?;
        ensure!(
            value["choices"][0]["finish_reason"] == "stop",
            "incomplete extraction response"
        );
        let content = value["choices"][0]["message"]["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing completion"))?;
        // A number of compatible providers wrap otherwise valid JSON in a code fence.
        let text = content
            .trim()
            .strip_prefix("```json")
            .or_else(|| content.trim().strip_prefix("```"))
            .map(|s| s.trim().strip_suffix("```").unwrap_or(s).trim())
            .unwrap_or(content.trim());
        Ok(serde_json::from_str(text)?)
    }

    pub async fn stream(
        &self,
        messages: &[ChatMessage],
        output: &mpsc::Sender<Event>,
    ) -> Result<String> {
        let start = std::time::Instant::now();
        let result = self.stream_inner(messages, output).await;
        self.metrics.record(start, result.is_ok());
        if let Err(error) = &result {
            crate::diagnostics::warn("llm.chat", error);
        }
        result
    }
    pub async fn health(&self) -> &'static str {
        let url = self
            .url
            .strip_suffix("/chat/completions")
            .unwrap_or(&self.url);
        match self
            .client
            .get(format!("{url}/models"))
            .bearer_auth(&self.key)
            .timeout(Duration::from_secs(4))
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                match bounded_body(response, 1_048_576)
                    .await
                    .and_then(|b| Ok(serde_json::from_slice::<Value>(&b)?))
                {
                    Ok(value) if value["data"].is_array() => "healthy",
                    _ => "unknown",
                }
            }
            Ok(response) if matches!(response.status().as_u16(), 404 | 405 | 501) => "unknown",
            _ => "unavailable",
        }
    }
    async fn stream_inner(
        &self,
        messages: &[ChatMessage],
        output: &mpsc::Sender<Event>,
    ) -> Result<String> {
        let messages = messages.iter().map(|m| json!(m)).collect::<Vec<_>>();
        Ok(self
            .round_inner(&messages, None, false, output)
            .await?
            .content)
    }
    pub async fn round(
        &self,
        messages: &[Value],
        tools: Option<&Value>,
        output: &mpsc::Sender<Event>,
    ) -> Result<Completion> {
        let start = std::time::Instant::now();
        let result = self.round_inner(messages, tools, true, output).await;
        self.metrics.record(start, result.is_ok());
        if let Err(error) = &result {
            crate::diagnostics::warn("llm.tools", error);
        }
        result
    }
    async fn round_inner(
        &self,
        messages: &[Value],
        tools: Option<&Value>,
        tool_conversation: bool,
        output: &mpsc::Sender<Event>,
    ) -> Result<Completion> {
        let mut body = json!({"model":self.model,"messages":messages,"temperature":self.temperature,"max_tokens":self.max_tokens,"stream":true});
        if let Some(tools) = tools {
            body["tools"] = tools.clone();
            body["tool_choice"] = json!("auto");
            body["parallel_tool_calls"] = json!(false);
        } else if tool_conversation {
            body["tool_choice"] = json!("none");
        }
        let response = self
            .client
            .post(&self.url)
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let mut events = response.bytes_stream().eventsource();
        let mut content = String::new();
        let mut reasoning = String::new();
        let mut guard = ToolTextGuard::default();
        let mut calls = std::collections::BTreeMap::<usize, ToolCall>::new();
        let mut finish = String::new();
        let mut total_bytes = 0usize;
        while let Some(event) = events.next().await {
            let event = event?;
            total_bytes += event.data.len();
            ensure!(
                total_bytes <= 2 * 1024 * 1024,
                "upstream stream exceeds limit"
            );
            if event.data.trim() == "[DONE]" {
                if tool_conversation {
                    let tail = guard.finish();
                    if !tail.is_empty() {
                        content.push_str(&tail);
                        emit(output, "delta", json!({"content":tail}))?;
                    }
                }
                ensure!(!finish.is_empty(), "unfinished completion");
                if calls.is_empty() {
                    ensure!(
                        (!content.trim().is_empty() || guard.suppressed)
                            && matches!(finish.as_str(), "stop" | "length"),
                        "empty or unfinished completion"
                    );
                } else {
                    ensure!(
                        finish == "tool_calls" && tools.is_some(),
                        "unfinished tool calls"
                    );
                    let mut ids = std::collections::HashSet::new();
                    for call in calls.values() {
                        ensure!(
                            !call.id.is_empty()
                                && call.id.len() <= 128
                                && !call.id.chars().any(char::is_control)
                                && ids.insert(&call.id)
                                && !call.function.name.is_empty()
                                && call.function.name.len() <= 64,
                            "invalid tool call"
                        );
                    }
                }
                return Ok(Completion {
                    content,
                    tool_calls: calls.into_values().collect(),
                    reasoning_content: if reasoning.is_empty() {
                        None
                    } else {
                        Some(reasoning)
                    },
                    tool_markup: guard.suppressed,
                });
            }
            let chunk: Value = serde_json::from_str(&event.data)?;
            ensure!(chunk.get("error").is_none(), "upstream stream error");
            if let Some(reason) = chunk["choices"][0]["finish_reason"].as_str() {
                ensure!(
                    matches!(reason, "stop" | "length" | "tool_calls"),
                    "unsupported finish reason"
                );
                finish = reason.into();
            }
            if let Some(delta) = chunk["choices"][0]["delta"]["content"].as_str() {
                ensure!(
                    content.len() + delta.len() <= 128_000,
                    "completion exceeds byte limit"
                );
                let visible = if tool_conversation {
                    guard.push(delta)
                } else {
                    delta.to_owned()
                };
                if !visible.is_empty() {
                    content.push_str(&visible);
                    emit(output, "delta", json!({"content":visible}))?;
                }
            }
            if tool_conversation
                && let Some(delta) = chunk["choices"][0]["delta"]["reasoning_content"].as_str()
            {
                ensure!(
                    reasoning.len() + delta.len() <= 256_000,
                    "reasoning exceeds byte limit"
                );
                reasoning.push_str(delta);
            }
            if let Some(deltas) = chunk["choices"][0]["delta"]["tool_calls"].as_array() {
                ensure!(tools.is_some(), "unexpected tool call");
                for delta in deltas {
                    let index = delta["index"]
                        .as_u64()
                        .ok_or_else(|| anyhow::anyhow!("missing tool index"))?
                        as usize;
                    ensure!(index < 4, "too many simultaneous tool calls");
                    let call = calls.entry(index).or_insert_with(|| ToolCall {
                        id: String::new(),
                        kind: "function".into(),
                        function: ToolFunction::default(),
                    });
                    if let Some(kind) = delta["type"].as_str() {
                        ensure!(kind == "function", "unsupported tool type");
                    }
                    if let Some(id) = delta["id"].as_str() {
                        call.id.push_str(id);
                    }
                    if let Some(name) = delta["function"]["name"].as_str() {
                        call.function.name.push_str(name);
                    }
                    if let Some(arguments) = delta["function"]["arguments"].as_str() {
                        call.function.arguments.push_str(arguments);
                    }
                    ensure!(
                        call.id.len() <= 128
                            && call.function.name.len() <= 64
                            && call.function.arguments.len() <= 16384,
                        "tool call exceeds limit"
                    );
                }
            }
        }
        bail!("upstream stream ended before DONE")
    }
}

/// A disconnected client does not cancel a durable turn. A slow connected client is bounded.
pub fn emit(output: &mpsc::Sender<Event>, name: &str, data: Value) -> Result<()> {
    let event = Event::default().event(name).data(data.to_string());
    match output.try_send(event) {
        Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_)) => bail!("slow streaming consumer"),
    }
}

pub async fn bounded_body(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        ensure!(body.len() + chunk.len() <= limit, "upstream body too large");
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::ToolTextGuard;

    #[test]
    fn dsml_is_never_visible_even_when_streamed_one_character_at_a_time() {
        let double = include_str!("../tests/fixtures/deepseek_dsml.txt");
        for marker in [
            double.to_owned(),
            double.replace("｜｜", "｜"),
            double.replace("｜｜", "|"),
            "<｜｜DS".into(),
        ] {
            let mut guard = ToolTextGuard::default();
            let mut visible = String::new();
            for character in format!("已完成搜索。{marker}").chars() {
                visible.push_str(&guard.push(&character.to_string()));
            }
            visible.push_str(&guard.finish());
            assert_eq!(visible, "已完成搜索。");
            assert!(guard.suppressed);
        }
    }

    #[test]
    fn ordinary_html_comparisons_and_partial_angles_keep_their_text() {
        let text = "<p>比较 1 < 2，DSML 是名称。</p>代码 Vec<T>，最后一个 <";
        let mut guard = ToolTextGuard::default();
        let mut visible = String::new();
        for character in text.chars() {
            visible.push_str(&guard.push(&character.to_string()));
        }
        visible.push_str(&guard.finish());
        assert_eq!(visible, text);
        assert!(!guard.suppressed);
    }
}
