use crate::{config::Config, llm::bounded_body};
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::OnceCell;

type RequestCache<T> = Arc<Mutex<HashMap<String, (Instant, Arc<OnceCell<T>>)>>>;

fn cached_request<T>(cache: &RequestCache<T>, key: String) -> Arc<OnceCell<T>> {
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    cache.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(300));
    if let Some((_, cell)) = cache.get(&key) {
        return cell.clone();
    }
    let cell = Arc::new(OnceCell::new());
    if cache.len() >= 128 {
        let oldest = cache
            .iter()
            .filter(|(_, (_, cell))| cell.initialized() || Arc::strong_count(cell) == 1)
            .min_by_key(|(_, (at, _))| *at)
            .map(|(key, _)| key.clone());
        if let Some(oldest) = oldest {
            cache.remove(&oldest);
        }
    }
    // In-flight work is never evicted to make room for an unbounded new entry.
    if cache.len() < 128 {
        cache.insert(key, (Instant::now(), cell.clone()));
    }
    cell
}

#[derive(Clone)]
pub struct Models {
    client: reqwest::Client,
    base: String,
    dimensions: usize,
    embedding_cache: RequestCache<Vec<Vec<f32>>>,
    reranking_cache: RequestCache<Vec<f64>>,
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
                // Inference takes much longer than a loopback TCP handshake.
                // Do not retain idle sockets across ASGI/container keep-alive
                // closures: stale pooled connections stalled real evaluations
                // until the 8s recall / 45s model deadlines.
                .pool_max_idle_per_host(0)
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base: config.models.base_url.trim_end_matches('/').into(),
            dimensions: config.models.dimensions,
            embedding_cache: Default::default(),
            reranking_cache: Default::default(),
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
        if texts.is_empty() {
            return Ok(Vec::new());
        }
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
        // Batch extraction can emit 32 facts, while the CPU service accepts eight.
        // Deduplicate within the operation without retaining private text in a global cache.
        let mut unique = Vec::new();
        let mut positions = std::collections::HashMap::new();
        let mut indices = Vec::with_capacity(texts.len());
        for text in texts {
            let index = *positions.entry(text).or_insert_with(|| {
                unique.push(text.clone());
                unique.len() - 1
            });
            indices.push(index);
        }
        let mut vectors = Vec::with_capacity(unique.len());
        // Sequential bounded chunks preserve the single CPU inference queue's backpressure.
        for chunk in unique.chunks(8) {
            vectors.extend(self.embed_batch(chunk, query).await?);
        }
        Ok(indices.into_iter().map(|i| vectors[i].clone()).collect())
    }
    async fn embed_batch(&self, texts: &[String], query: bool) -> Result<Vec<Vec<f32>>> {
        let key = format!("{:x}", Sha256::digest(serde_json::to_vec(&(query, texts))?));
        let cell = cached_request(&self.embedding_cache, key);
        // OnceCell shares successful results and in-flight work. Failed or
        // cancelled initializations are retryable; only hashed input keys remain.
        Ok(cell
            .get_or_try_init(|| self.embed_batch_uncached(texts, query))
            .await?
            .clone())
    }
    async fn embed_batch_uncached(&self, texts: &[String], query: bool) -> Result<Vec<Vec<f32>>> {
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
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        let key = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(query, documents))?)
        );
        let cell = cached_request(&self.reranking_cache, key);
        Ok(cell
            .get_or_try_init(|| self.rerank_uncached(query, documents))
            .await?
            .clone())
    }
    async fn rerank_uncached(&self, query: &str, documents: &[String]) -> Result<Vec<f64>> {
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
