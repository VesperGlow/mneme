use crate::{config::Config, domain::ChatMessage};
use anyhow::{Result, bail, ensure};
use axum::response::sse::Event;
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::mpsc;

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
        let response = self.client.post(&self.url).bearer_auth(&self.key)
            .json(&json!({"model":self.model,"messages":messages,"temperature":self.temperature,"max_tokens":self.max_tokens,"stream":true}))
            .send().await?.error_for_status()?;
        let mut events = response.bytes_stream().eventsource();
        let mut content = String::new();
        let mut finished = false;
        while let Some(event) = events.next().await {
            let event = event?;
            if event.data.trim() == "[DONE]" {
                ensure!(
                    finished && !content.trim().is_empty(),
                    "empty or unfinished completion"
                );
                return Ok(content);
            }
            let chunk: Value = serde_json::from_str(&event.data)?;
            ensure!(chunk.get("error").is_none(), "upstream stream error");
            if let Some(reason) = chunk["choices"][0]["finish_reason"].as_str() {
                ensure!(
                    matches!(reason, "stop" | "length"),
                    "unsupported finish reason"
                );
                finished = true;
            }
            if let Some(delta) = chunk["choices"][0]["delta"]["content"].as_str() {
                ensure!(
                    content.len() + delta.len() <= 128_000,
                    "completion exceeds byte limit"
                );
                content.push_str(delta);
                emit(output, "delta", json!({"content":delta}))?;
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
