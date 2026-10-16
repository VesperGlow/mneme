use crate::{config::Config, llm::bounded_body};
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

#[derive(Clone)]
pub struct Models {
    client: reqwest::Client,
    base: String,
    dimensions: usize,
    pub embed_metrics: std::sync::Arc<crate::telemetry::Metrics>,
    pub rerank_metrics: std::sync::Arc<crate::telemetry::Metrics>,
}

#[derive(Deserialize)]
struct Embeddings {
    embeddings: Vec<Vec<f32>>,
}
#[derive(Deserialize)]
struct Scores {
    scores: Vec<f64>,
}

impl Models {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(3))
                .timeout(Duration::from_secs(config.models.timeout_seconds))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base: config.models.base_url.trim_end_matches('/').into(),
            dimensions: config.models.dimensions,
            embed_metrics: Default::default(),
            rerank_metrics: Default::default(),
        })
    }
    pub async fn health(&self) -> bool {
        self.client
            .get(format!("{}/health", self.base))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }
    pub async fn embed(&self, texts: &[String], query: bool) -> Result<Vec<Vec<f32>>> {
        let start = std::time::Instant::now();
        let result = self.embed_inner(texts, query).await;
        self.embed_metrics.record(start, result.is_ok());
        result
    }
    pub async fn info(&self) -> Result<serde_json::Value> {
        let response = self
            .client
            .get(format!("{}/health", self.base))
            .timeout(Duration::from_secs(3))
            .send()
            .await?
            .error_for_status()?;
        Ok(serde_json::from_slice(
            &bounded_body(response, 65536).await?,
        )?)
    }
    async fn embed_inner(&self, texts: &[String], query: bool) -> Result<Vec<Vec<f32>>> {
        let response = self
            .client
            .post(format!("{}/embed", self.base))
            .json(&json!({"texts":texts,"query":query}))
            .send()
            .await?
            .error_for_status()?;
        let value: Embeddings = serde_json::from_slice(&bounded_body(response, 2_097_152).await?)?;
        ensure!(
            value.embeddings.len() == texts.len(),
            "embedding batch size mismatch"
        );
        for vector in &value.embeddings {
            ensure!(
                vector.len() == self.dimensions && vector.iter().all(|x| x.is_finite()),
                "invalid embedding"
            );
            let norm: f32 = vector.iter().map(|x| x * x).sum();
            ensure!((norm - 1.0).abs() < 0.02, "embedding must be normalized");
        }
        Ok(value.embeddings)
    }
    pub async fn rerank(&self, query: &str, documents: &[String]) -> Result<Vec<f64>> {
        let start = std::time::Instant::now();
        let result = self.rerank_inner(query, documents).await;
        self.rerank_metrics.record(start, result.is_ok());
        result
    }
    async fn rerank_inner(&self, query: &str, documents: &[String]) -> Result<Vec<f64>> {
        let response = self
            .client
            .post(format!("{}/rerank", self.base))
            .json(&json!({"query":query,"documents":documents}))
            .send()
            .await?
            .error_for_status()?;
        let value: Scores = serde_json::from_slice(&bounded_body(response, 65_536).await?)?;
        ensure!(
            value.scores.len() == documents.len()
                && value
                    .scores
                    .iter()
                    .all(|s| s.is_finite() && (0.0..=1.0).contains(s)),
            "invalid reranking scores"
        );
        Ok(value.scores)
    }
}
