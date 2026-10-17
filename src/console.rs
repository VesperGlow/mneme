use crate::{
    App,
    api::{ApiError, ApiResult, valid_id},
    console_store::{MemoryFilter, MemoryWrite, WriteResult},
    domain::{Memory, now},
    memory,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

pub fn public_routes() -> Router<App> {
    Router::new()
        .route("/", get(|| async { Redirect::to("/console/") }))
        .route("/console", get(|| async { Redirect::to("/console/") }))
        .route(
            "/console/",
            get(|| async { Html(include_str!("../web/index.html")) }),
        )
        .route(
            "/console/app.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../web/app.js"),
                )
            }),
        )
        .route(
            "/console/style.css",
            get(|| async { asset("text/css; charset=utf-8", include_str!("../web/style.css")) }),
        )
        .route(
            "/console/icon.svg",
            get(|| async { asset("image/svg+xml", include_str!("../web/icon.svg")) }),
        )
}
fn asset(kind: &'static str, body: &'static str) -> Response {
    ([(header::CONTENT_TYPE, kind)], body).into_response()
}
pub fn protected_routes() -> Router<App> {
    Router::new()
        .route(
            "/v1/console/auth",
            get(|| async { Json(json!({"authenticated":true})) }),
        )
        .route("/v1/console/stats", get(stats))
        .route("/v1/console/messages", get(transcript))
        .route("/v1/console/retrieval", post(retrieval))
        .route("/v1/console/system", get(system))
}

async fn transcript(State(app): State<App>) -> ApiResult<Json<Value>> {
    let Json(conversation) = crate::api::conversation(State(app.clone())).await?;
    let messages = app.store.console_history().await?;
    let mut assets = Vec::<Value>::new();
    if !messages.is_empty() {
        let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
            "SELECT a.id,ma.message_id,a.kind,a.width,a.height FROM assets a JOIN message_assets ma ON ma.asset_id=a.id WHERE ma.message_id IN (",
        );
        let mut ids = query.separated(",");
        for message in &messages {
            ids.push_bind(&message.message.id);
        }
        ids.push_unseparated(") ORDER BY ma.message_id,ma.position");
        let rows: Vec<(String, String, String, u32, u32)> = query
            .build_query_as()
            .fetch_all(&app.store.pool)
            .await
            .map_err(anyhow::Error::from)?;
        assets = rows.into_iter().map(|(id,message_id,kind,width,height)|json!({"id":id,"message_id":message_id,"kind":kind,"width":width,"height":height})).collect();
    }
    let turns: std::collections::HashSet<_> = messages
        .iter()
        .map(|m| m.message.turn_id.as_str())
        .collect();
    let calls = crate::tools::logs(&app, None).await?;
    let calls: Vec<_> = calls
        .as_array()
        .unwrap()
        .iter()
        .filter(|call| {
            call["turn_id"]
                .as_str()
                .is_some_and(|id| turns.contains(id))
        })
        .collect();
    Ok(Json(
        json!({"messages":messages,"assets":assets,"tool_calls":calls,"conversation":conversation}),
    ))
}

pub(crate) fn validate_filter(f: &MemoryFilter) -> ApiResult<()> {
    if f.q.as_ref().is_some_and(|q| q.chars().count() > 500)
        || f.kind.as_deref().is_some_and(|k| {
            !matches!(
                k,
                "" | "fact" | "preference" | "event" | "relationship" | "emotion"
            )
        })
        || f.status.as_deref().is_some_and(|s| {
            !matches!(
                s,
                "" | "all"
                    | "active"
                    | "invalid"
                    | "expired"
                    | "forgotten"
                    | "superseded"
                    | "deleted"
            )
        })
        || f.sort.as_deref().is_some_and(|s| {
            !matches!(
                s,
                "updated" | "created" | "oldest" | "importance" | "confidence"
            )
        })
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_memory_filter"));
    }
    Ok(())
}
pub(crate) fn write_result(result: WriteResult) -> ApiResult<Memory> {
    match result {
        WriteResult::Saved(memory) => Ok(*memory),
        WriteResult::Missing => Err(ApiError(StatusCode::NOT_FOUND, "memory_not_found")),
        WriteResult::Conflict => Err(ApiError(StatusCode::CONFLICT, "memory_changed_reload")),
        WriteResult::Duplicate => Err(ApiError(StatusCode::CONFLICT, "duplicate_memory")),
    }
}
pub(crate) async fn create_memory(
    State(app): State<App>,
    Json(input): Json<MemoryWrite>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if !input.valid() || input.expected_revision.is_some() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_memory"));
    }
    let memory = write_result(app.store.manual_memory(None, &input).await?)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"memory":memory,"index_update":"queued"})),
    ))
}
pub(crate) async fn edit_memory(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(input): Json<MemoryWrite>,
) -> ApiResult<Json<Value>> {
    valid_id(&id)?;
    if !input.valid() || input.expected_revision.is_none() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_memory"));
    }
    let memory = write_result(app.store.manual_memory(Some(&id), &input).await?)?;
    Ok(Json(json!({"memory":memory,"index_update":"queued"})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeleteQuery {
    pub expected_revision: Option<i64>,
}

async fn stats(State(app): State<App>) -> ApiResult<Json<Value>> {
    let mut stats = app.store.console_stats().await?;
    stats["jobs"] = app.store.job_stats().await?;
    Ok(Json(stats))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DebugQuery {
    query: String,
}
async fn retrieval(
    State(app): State<App>,
    Json(input): Json<DebugQuery>,
) -> ApiResult<Json<Value>> {
    if input.query.trim().is_empty() || input.query.chars().count() > 4000 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_query"));
    }
    let result = memory::debug_recall(&app, &input.query).await?;
    Ok(Json(
        json!({"results":result.memories,"trace":result.trace,"degraded":result.degraded,"rerank_min_score":app.config.memory.rerank_min_score}),
    ))
}
async fn system(State(app): State<App>) -> ApiResult<Json<Value>> {
    let mut cache = app.status_cache.lock().await;
    if let Some((at, value)) = &*cache
        && at.elapsed() < Duration::from_secs(10)
    {
        let mut value = value.clone();
        value["cached"] = json!(true);
        value["uptime_seconds"] = json!(app.started_at.elapsed().as_secs());
        return Ok(Json(value));
    }
    let (llm, graph, models) = tokio::join!(
        async {
            let at = Instant::now();
            let state = app.llm.health().await;
            (state, at.elapsed().as_secs_f64() * 1000.0)
        },
        async {
            let at = Instant::now();
            let healthy = matches!(
                tokio::time::timeout(Duration::from_secs(4), app.graph.health()).await,
                Ok(true)
            );
            (healthy, at.elapsed().as_secs_f64() * 1000.0)
        },
        async {
            let at = Instant::now();
            let info = app.models.info().await;
            (info.ok(), at.elapsed().as_secs_f64() * 1000.0)
        }
    );
    let model_ready = models.0.as_ref().is_some_and(|v| v["ready"] == true);
    let mut database_bytes = 0u64;
    for suffix in ["", "-wal", "-shm"] {
        if let Ok(meta) = tokio::fs::metadata(format!("{}{suffix}", app.config.database)).await {
            database_bytes += meta.len();
        }
    }
    let value = json!({"checked_at":now(),"cached":false,"uptime_seconds":app.started_at.elapsed().as_secs(),"version":env!("CARGO_PKG_VERSION"),"database_bytes":database_bytes,"jobs":app.store.job_stats().await?,
        "services":{
            "llm":{"state":llm.0,"latency_ms":llm.1,"model":app.config.llm.model,"metrics":app.llm.metrics.snapshot(),"probe":"models_catalog"},
            "neo4j":{"state":if graph.0{"healthy"}else{"unavailable"},"latency_ms":graph.1,"metrics":app.graph.metrics.snapshot()},
            "embedding":{"state":if model_ready{"healthy"}else{"unavailable"},"latency_ms":models.1,"model":"Qwen/Qwen3-Embedding-0.6B","metrics":app.models.embed_metrics.snapshot()},
            "reranker":{"state":if model_ready{"healthy"}else{"unavailable"},"latency_ms":models.1,"model":"Alibaba-NLP/gte-multilingual-reranker-base","metrics":app.models.rerank_metrics.snapshot()}
        },"cpu_threads":3,"dimensions":app.config.models.dimensions,"model_queue":models.0.as_ref().and_then(|v|v["queued"].as_u64()),"sqlite":"healthy"});
    *cache = Some((Instant::now(), value.clone()));
    Ok(Json(value))
}
