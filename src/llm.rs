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
    context_tokens: usize,
    json_schema: bool,
    store: Option<crate::store::Store>,
    scope: Option<(Option<String>, String)>,
    stream_usage: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
            context_tokens: config.llm.context_tokens,
            json_schema: config.llm.json_schema,
            store: None,
            scope: None,
            stream_usage: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            metrics: Default::default(),
        })
    }

    pub(crate) fn tracked(mut self, store: crate::store::Store) -> Self {
        self.store = Some(store);
        self
    }
    pub(crate) fn for_turn(&self, turn: &str, purpose: &str) -> Self {
        let mut scoped = self.clone();
        scoped.scope = Some((Some(turn.to_owned()), purpose.to_owned()));
        scoped
    }
    pub(crate) fn for_purpose(&self, purpose: &str) -> Self {
        let mut scoped = self.clone();
        scoped.scope = Some((None, purpose.into()));
        scoped
    }
    async fn start_request(&self, context: &crate::usage::ContextTokens) -> Result<String> {
        if let Some(store) = &self.store {
            store
                .start_model_request(
                    self.scope.as_ref().and_then(|s| s.0.as_deref()),
                    self.scope
                        .as_ref()
                        .map(|s| s.1.as_str())
                        .unwrap_or("memory"),
                    &self.model,
                    context,
                )
                .await
        } else {
            Ok(crate::domain::id())
        }
    }
    async fn save_request(
        &self,
        id: &str,
        usage: &crate::usage::TokenUsage,
        reasoning: &str,
        status: &str,
    ) -> Result<()> {
        if let Some(store) = &self.store {
            store
                .update_model_request(id, usage, reasoning, status)
                .await?;
        }
        Ok(())
    }

    pub async fn complete_json(&self, system: &str, input: Value) -> Result<Value> {
        self.complete_json_with_tokens(system, input, 2500).await
    }
    pub(crate) async fn describe_image(
        &self,
        system: &str,
        image: &crate::assets::ImageData,
    ) -> Result<Value> {
        let messages = json!([
            {"role":"system","content":system},
            {"role":"user","content":[
                {"type":"text","text":"请识别这张表情包。若有多格，按从左到右的首、中、末帧理解动作；只描述实际可见的内容。"},
                {"type":"image_url","image_url":{"url":format!("data:{};base64,{}",image.mime,image.data)}}
            ]}
        ]);
        let context = crate::usage::ContextTokens::from_wire(messages.as_array().unwrap(), None);
        ensure!(
            context.total() + 768 <= self.context_tokens,
            "image request exceeds token budget"
        );
        let start = std::time::Instant::now();
        let result = async {
            let response = self.json_request(&json!({"model":self.model,"temperature":0,"max_tokens":768,"stream":false,"messages":messages})).await?;
            ensure!(response["choices"][0]["finish_reason"] == "stop", "incomplete image description");
            let content = response["choices"][0]["message"]["content"].as_str().ok_or_else(|| anyhow::anyhow!("missing image description"))?;
            let text = content.trim().strip_prefix("```json").or_else(|| content.trim().strip_prefix("```"))
                .map(|s| s.trim().strip_suffix("```").unwrap_or(s).trim()).unwrap_or(content.trim());
            Ok::<Value, anyhow::Error>(serde_json::from_str(text)?)
        }.await;
        self.metrics.record(start, result.is_ok());
        result
    }
    pub(crate) async fn complete_json_with_tokens(
        &self,
        system: &str,
        input: Value,
        max_tokens: u32,
    ) -> Result<Value> {
        self.complete_json_schema(system, input, max_tokens, None)
            .await
    }
    pub(crate) async fn complete_json_schema(
        &self,
        system: &str,
        input: Value,
        max_tokens: u32,
        schema: Option<Value>,
    ) -> Result<Value> {
        let start = std::time::Instant::now();
        let result = self
            .complete_json_inner(system, input, max_tokens, schema)
            .await;
        self.metrics.record(start, result.is_ok());
        if let Err(error) = &result {
            crate::diagnostics::warn("llm.extract", error);
        }
        result
    }
    async fn complete_json_inner(
        &self,
        system: &str,
        input: Value,
        max_tokens: u32,
        schema: Option<Value>,
    ) -> Result<Value> {
        let schema_tokens = schema
            .as_ref()
            .filter(|_| self.json_schema)
            .map(|s| crate::tokens::count(&s.to_string()) + 64)
            .unwrap_or(0);
        ensure!(
            schema_tokens
                + crate::tokens::message(system)
                + crate::tokens::message(&input.to_string())
                + 3
                + max_tokens as usize
                <= self.context_tokens,
            "JSON request exceeds token budget"
        );
        let mut body = json!({"model":self.model,"temperature":0,"max_tokens":max_tokens,"stream":false,
            "messages":[{"role":"system","content":system},{"role":"user","content":input.to_string()}]});
        if self.json_schema
            && let Some(schema) = schema
        {
            body["response_format"] = json!({"type":"json_schema","json_schema":{"name":"memory_extraction","strict":true,"schema":schema}});
        }
        let value = self.json_request(&body).await?;
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
    async fn json_request(&self, body: &Value) -> Result<Value> {
        let mut context =
            crate::usage::ContextTokens::from_wire(body["messages"].as_array().unwrap(), None);
        context.tools += body
            .get("response_format")
            .map(|s| crate::tokens::count(&s.to_string()) + 64)
            .unwrap_or(0);
        let id = self.start_request(&context).await?;
        let result = async {
            let response = self
                .client
                .post(&self.url)
                .bearer_auth(&self.key)
                .json(body)
                .send()
                .await?
                .error_for_status()?;
            Ok::<Value, anyhow::Error>(serde_json::from_slice(
                &bounded_body(response, 1_048_576).await?,
            )?)
        }
        .await;
        let mut usage = crate::usage::TokenUsage::default();
        if let Ok(value) = &result {
            usage.update(value);
        }
        self.save_request(
            &id,
            &usage,
            "",
            if result.is_ok() { "complete" } else { "failed" },
        )
        .await?;
        result
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
        ensure!(
            3 + messages
                .iter()
                .map(crate::tokens::wire_message)
                .sum::<usize>()
                + tools
                    .map(|t| crate::tokens::count(&t.to_string()) + 20)
                    .unwrap_or(0)
                + self.max_tokens as usize
                <= self.context_tokens,
            "model request exceeds token budget"
        );
        let context = crate::usage::ContextTokens::from_wire(messages, tools);
        let id = self.start_request(&context).await?;
        emit(
            output,
            "context",
            json!({"request_id":id,"context":context,"estimated_input_tokens":context.total(),"tokens_estimated":true}),
        )?;
        let mut usage = crate::usage::TokenUsage::default();
        let mut reasoning = String::new();
        let result = self
            .read_round(
                messages,
                tools,
                tool_conversation,
                output,
                &id,
                &mut usage,
                &mut reasoning,
            )
            .await;
        self.save_request(
            &id,
            &usage,
            &reasoning,
            if result.is_ok() { "complete" } else { "failed" },
        )
        .await?;
        emit(
            output,
            "usage",
            json!({"request_id":id,"usage":usage,"status":if result.is_ok(){"complete"}else{"failed"}}),
        )?;
        result
    }
    #[allow(clippy::too_many_arguments)]
    async fn read_round(
        &self,
        messages: &[Value],
        tools: Option<&Value>,
        tool_conversation: bool,
        output: &mpsc::Sender<Event>,
        id: &str,
        usage: &mut crate::usage::TokenUsage,
        reasoning: &mut String,
    ) -> Result<Completion> {
        let mut body = json!({"model":self.model,"messages":messages,"temperature":self.temperature,"max_tokens":self.max_tokens,"stream":true,"stream_options":{"include_usage":true}});
        if !self.stream_usage.load(std::sync::atomic::Ordering::Relaxed) {
            body.as_object_mut().unwrap().remove("stream_options");
        }
        if let Some(tools) = tools {
            body["tools"] = tools.clone();
            body["tool_choice"] = json!("auto");
            body["parallel_tool_calls"] = json!(false);
        } else if tool_conversation {
            body["tool_choice"] = json!("none");
        }
        let mut response = self
            .client
            .post(&self.url)
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await?;
        // Some compatible endpoints reject this optional parameter. Retry only
        // explicit validation failures about stream usage, never an accepted call.
        if matches!(response.status().as_u16(), 400 | 422) && body.get("stream_options").is_some() {
            let status = response.status();
            let detail =
                String::from_utf8_lossy(&bounded_body(response, 65536).await?).to_lowercase();
            ensure!(
                detail.contains("stream_options") || detail.contains("include_usage"),
                "upstream HTTP {status}"
            );
            self.stream_usage
                .store(false, std::sync::atomic::Ordering::Relaxed);
            body.as_object_mut().unwrap().remove("stream_options");
            response = self
                .client
                .post(&self.url)
                .bearer_auth(&self.key)
                .json(&body)
                .send()
                .await?;
        }
        let response = response.error_for_status()?;
        let mut events = response.bytes_stream().eventsource();
        let mut content = String::new();
        let mut guard = ToolTextGuard::default();
        let mut media_guard = crate::stickers::MediaTextGuard::default();
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
                        emit(output, "delta", json!({"content":media_guard.push(&tail)}))?;
                    }
                }
                let tail = media_guard.finish();
                if !tail.is_empty() {
                    emit(output, "delta", json!({"content":tail}))?;
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
                        Some(reasoning.clone())
                    },
                    tool_markup: guard.suppressed,
                });
            }
            let chunk: Value = serde_json::from_str(&event.data)?;
            ensure!(chunk.get("error").is_none(), "upstream stream error");
            if usage.update(&chunk) {
                // Persist even if the stream later fails or the turn is cancelled.
                self.save_request(id, usage, reasoning, "running").await?;
                emit(
                    output,
                    "usage",
                    json!({"request_id":id,"usage":usage,"status":"running"}),
                )?;
            }
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
                    let visible = media_guard.push(&visible);
                    if !visible.is_empty() {
                        emit(output, "delta", json!({"content":visible}))?;
                    }
                }
            }
            for delta in reasoning_deltas(&chunk["choices"][0]["delta"]) {
                ensure!(
                    reasoning.len() + delta.len() <= 256_000,
                    "reasoning exceeds byte limit"
                );
                reasoning.push_str(delta);
                emit(
                    output,
                    "reasoning",
                    json!({"request_id":id,"content":delta}),
                )?;
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

// Compatible providers expose plain reasoning under different names. Only render
// returned text/summary blocks; signatures and encrypted blocks are not display text.
fn reasoning_deltas(delta: &Value) -> Vec<&str> {
    for key in ["reasoning_content", "reasoning", "thinking"] {
        if let Some(text) = delta[key].as_str() {
            return vec![text];
        }
    }
    delta["reasoning_details"]
        .as_array()
        .map(|details| {
            details
                .iter()
                .filter_map(|part| match part["type"].as_str() {
                    Some("reasoning.text") => part["text"].as_str(),
                    Some("reasoning.summary") => part["summary"].as_str(),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
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
    fn reasoning_aliases_render_only_returned_plain_text() {
        use super::*;
        for key in ["reasoning_content", "reasoning", "thinking"] {
            assert_eq!(reasoning_deltas(&json!({key:"实际内容"})), vec!["实际内容"]);
        }
        assert_eq!(
            reasoning_deltas(&json!({"reasoning_details":[
                {"type":"reasoning.encrypted","data":"secret"},
                {"type":"reasoning.summary","summary":"实际摘要"},
                {"type":"reasoning.text","text":"实际内容"}
            ]})),
            vec!["实际摘要", "实际内容"]
        );
        assert!(reasoning_deltas(&json!({"content":"ordinary answer"})).is_empty());
    }

    #[tokio::test]
    async fn token_guard_includes_reply_reserve_json_and_tool_schemas_before_network() {
        use super::*;
        let mut config: Config = toml::from_str(include_str!("../config/default.toml")).unwrap();
        config.llm.base_url = "http://127.0.0.1:1".into();
        config.llm.context_tokens = 3000;
        let llm = Llm::new(&config).unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let messages = vec![ChatMessage {
            role: "user".into(),
            content: "字".repeat(2000),
        }];
        assert!(
            llm.stream(&messages, &tx)
                .await
                .unwrap_err()
                .to_string()
                .contains("token budget")
        );
        assert!(
            llm.complete_json("extract", json!({"source":"字".repeat(1000)}))
                .await
                .unwrap_err()
                .to_string()
                .contains("token budget")
        );
        let result = llm
            .round(
                &[json!({"role":"user","content":"hi"})],
                Some(&json!({"description":"字".repeat(3000)})),
                &tx,
            )
            .await;
        assert!(result.err().unwrap().to_string().contains("token budget"));
    }

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
