//! Explicit, reviewable forgetting. Original messages remain auditable.
use crate::{
    App,
    api::{ApiError, ApiResult},
    console_store::{MemoryFilter, WriteResult},
    domain::*,
};
use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde::Deserialize;
use serde_json::{Value, json};

pub fn routes() -> Router<App> {
    Router::new()
        .route("/v1/memories/forget/preview", post(preview))
        .route("/v1/memories/forget/confirm", post(confirm))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preview {
    query: String,
}

async fn preview(State(app): State<App>, Json(input): Json<Preview>) -> ApiResult<Json<Value>> {
    Ok(Json(preview_for(&app, &input.query).await?))
}

pub(crate) async fn preview_for(app: &App, query: &str) -> ApiResult<Value> {
    let query = query.trim();
    if query.is_empty() || query.chars().count() > 500 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_query"));
    }
    let lexical = app
        .store
        .memory_page(&MemoryFilter {
            q: Some(query.into()),
            status: Some("active".into()),
            limit: Some(12),
            ..Default::default()
        })
        .await?;
    let mut memories = lexical.memories;
    let recalled = crate::memory::recall(app, query).await?;
    for item in recalled.memories {
        if memories.len() < 12 && !memories.iter().any(|m| m.id == item.memory.id) {
            memories.push(item.memory);
        }
    }
    let mut tx = app
        .store
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(anyhow::Error::from)?;
    sqlx::query("DELETE FROM forget_requests WHERE expires_at<=?")
        .bind(now())
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    // Re-read the revision after retrieval; stale previews must never authorize a newer value.
    let mut candidates = Vec::new();
    for m in memories {
        let current: Option<Memory> = sqlx::query_as("SELECT * FROM memories WHERE id=? AND revision=? AND status='active' AND deleted_at IS NULL")
            .bind(&m.id).bind(m.revision).fetch_optional(&mut *tx).await.map_err(anyhow::Error::from)?;
        if let Some(m) = current.filter(|m| m.available(now())) {
            let sources: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM memory_sources WHERE memory_id=?")
                    .bind(&m.id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(anyhow::Error::from)?;
            candidates.push(json!({"id":m.id,"revision":m.revision,"content":m.content,"kind":m.kind,"source_count":sources}));
        }
    }
    let (through, messages): (i64, i64) =
        sqlx::query_as("SELECT COALESCE(MAX(seq),0),COUNT(*) FROM messages")
            .fetch_one(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
    let token = id();
    let expires_at = now() + 300;
    sqlx::query(
        "INSERT INTO forget_requests(id,candidates,through_seq,expires_at) VALUES(?,?,?,?)",
    )
    .bind(&token)
    .bind(json!(candidates).to_string())
    .bind(through)
    .bind(expires_at)
    .execute(&mut *tx)
    .await
    .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    Ok(
        json!({"token":token,"candidates":candidates,"expires_at":expires_at,"context_messages":messages,"degraded":recalled.degraded}),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Confirmation {
    pub token: String,
    pub memory_ids: Vec<String>,
    pub scope: String,
    pub confirmation: String,
}

async fn confirm(
    State(app): State<App>,
    Json(input): Json<Confirmation>,
) -> ApiResult<Json<Value>> {
    Ok(Json(confirm_for(&app, input).await?))
}

pub(crate) async fn confirm_for(app: &App, input: Confirmation) -> ApiResult<Value> {
    if input.confirmation != "FORGET"
        || !matches!(input.scope.as_str(), "memory" | "context")
        || input.memory_ids.is_empty()
        || input.memory_ids.len() > 12
        || input
            .memory_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != input.memory_ids.len()
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_forget_confirmation",
        ));
    }
    crate::api::valid_id(&input.token)?;
    let mut tx = app
        .store
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(anyhow::Error::from)?;
    let plan: Option<(String, i64, i64, Option<String>)> = sqlx::query_as(
        "SELECT candidates,through_seq,expires_at,result FROM forget_requests WHERE id=?",
    )
    .bind(&input.token)
    .fetch_optional(&mut *tx)
    .await
    .map_err(anyhow::Error::from)?;
    let Some((candidates, through, expires_at, result)) = plan else {
        return Err(ApiError(StatusCode::NOT_FOUND, "forget_preview_not_found"));
    };
    if let Some(result) = result {
        let result: Value = serde_json::from_str(&result).map_err(anyhow::Error::from)?;
        if result["scope"] != input.scope || result["memory_ids"] != json!(input.memory_ids) {
            return Err(ApiError(
                StatusCode::CONFLICT,
                "forget_confirmation_changed",
            ));
        }
        return Ok(result);
    }
    if expires_at <= now() {
        return Err(ApiError(StatusCode::CONFLICT, "forget_preview_expired"));
    }
    let busy: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE active_turn IS NOT NULL)")
            .fetch_one(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
    if busy {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "stop_generation_before_forgetting",
        ));
    }
    let candidates: Vec<Value> = serde_json::from_str(&candidates).map_err(anyhow::Error::from)?;
    if input.scope == "context" {
        let latest: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq),0) FROM messages")
            .fetch_one(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
        if latest != through {
            return Err(ApiError(StatusCode::CONFLICT, "forget_context_changed"));
        }
    }
    for id in &input.memory_ids {
        let candidate = candidates
            .iter()
            .find(|m| m["id"] == *id)
            .ok_or(ApiError(StatusCode::BAD_REQUEST, "memory_not_in_preview"))?;
        match crate::console_store::delete_memory_in(&mut tx, id, candidate["revision"].as_i64())
            .await?
        {
            WriteResult::Saved(_) => {}
            _ => return Err(ApiError(StatusCode::CONFLICT, "memory_changed_reload")),
        }
    }
    if input.scope == "context" {
        // Start subsequent compression after this boundary. Nothing is removed
        // from the audit transcript, and unrelated long-term facts remain usable.
        sqlx::query("INSERT INTO conversation_context(session_id,summary,through_seq,updated_at) SELECT id,'',?,? FROM sessions WHERE true ON CONFLICT(session_id) DO UPDATE SET summary='',through_seq=MAX(through_seq,excluded.through_seq),updated_at=excluded.updated_at")
            .bind(through).bind(now()).execute(&mut *tx).await.map_err(anyhow::Error::from)?;
        sqlx::query("UPDATE memories SET last_accessed_at=NULL")
            .execute(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
    }
    let result = json!({"memory_ids":input.memory_ids,"scope":input.scope,"status":"forgotten","context_cleared":input.scope=="context","originals_retained":true,"graph_cleanup":"queued"});
    sqlx::query("UPDATE forget_requests SET result=? WHERE id=?")
        .bind(result.to_string())
        .bind(input.token)
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    Ok(result)
}

/// Only explicit leading commands open a preview; arbitrary mentions are normal chat.
pub(crate) fn forget_query(text: &str) -> Option<&str> {
    let text = text.trim();
    ["/忘记 ", "请忘记", "请忘掉", "忘记", "忘掉"]
        .iter()
        .find_map(|prefix| text.strip_prefix(prefix))
        .map(|query| query.trim().trim_end_matches(['。', '！', '!']))
        .filter(|query| !query.is_empty() && query.chars().count() <= 500)
}

pub(crate) async fn qq_command(app: &App, text: &str) -> Option<String> {
    if let Some(args) = text.trim().strip_prefix("/确认忘记 ") {
        let result = async {
            let parts: Vec<_> = args.split_whitespace().collect();
            if !(2..=3).contains(&parts.len()) || parts.get(2).is_some_and(|p| *p != "上下文") {
                return Err(ApiError(
                    StatusCode::BAD_REQUEST,
                    "invalid_forget_confirmation",
                ));
            }
            let candidates: Option<String> =
                sqlx::query_scalar("SELECT candidates FROM forget_requests WHERE id=?")
                    .bind(parts[0])
                    .fetch_optional(&app.store.pool)
                    .await
                    .map_err(anyhow::Error::from)?;
            let candidates: Vec<Value> = serde_json::from_str(
                &candidates.ok_or(ApiError(StatusCode::NOT_FOUND, "forget_preview_not_found"))?,
            )
            .map_err(anyhow::Error::from)?;
            let mut ids = Vec::new();
            for index in parts[1].split([',', '，']) {
                let index = index
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| i.checked_sub(1))
                    .ok_or(ApiError(
                        StatusCode::BAD_REQUEST,
                        "invalid_forget_confirmation",
                    ))?;
                ids.push(
                    candidates
                        .get(index)
                        .and_then(|m| m["id"].as_str())
                        .ok_or(ApiError(StatusCode::BAD_REQUEST, "memory_not_in_preview"))?
                        .to_owned(),
                );
            }
            confirm_for(
                app,
                Confirmation {
                    token: parts[0].into(),
                    memory_ids: ids,
                    scope: if parts.len() == 3 {
                        "context"
                    } else {
                        "memory"
                    }
                    .into(),
                    confirmation: "FORGET".into(),
                },
            )
            .await
        }
        .await;
        return Some(match result {
            Ok(value) if value["context_cleared"] == true => "已移除所选长期记忆，并清空摘要、停止使用此前对话作为聊天历史。原始记录和备份仍保留，其他长期记忆不受影响。".into(),
            Ok(_) => "已移除所选长期记忆。此前对话和摘要仍可能包含相关内容，原始记录和备份保留。".into(),
            Err(error) => command_error(error),
        });
    }
    let query = forget_query(text)?;
    Some(match preview_for(app, query).await {
        Ok(value) if value["candidates"].as_array().is_none_or(|a| a.is_empty()) => {
            "没有找到匹配的长期记忆，未执行删除。原始对话可在 Console 查看。".into()
        }
        Ok(value) => {
            let lines: Vec<_> = value["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    format!(
                        "{}. {}",
                        i + 1,
                        truncate(m["content"].as_str().unwrap_or(""), 160)
                    )
                })
                .collect();
            format!(
                "找到以下候选，尚未删除，请逐项核对：\n{}\n\n5分钟内回复 /确认忘记 {} 1,2（替换为要删除的序号）。\n仅移除所选长期记忆，历史和摘要仍可能包含相关内容。若同时停止使用此前全部对话并清空摘要，在命令末尾加“ 上下文”。两种范围均保留原始记录和备份。",
                lines.join("\n"),
                value["token"].as_str().unwrap()
            )
        }
        Err(error) => command_error(error),
    })
}

fn command_error(error: ApiError) -> String {
    match error.1 {
        "stop_generation_before_forgetting" => "当前正在生成回复，请先发送 /停止，再确认遗忘。",
        "memory_changed_reload" | "forget_context_changed" => {
            "记录或对话已变化，未执行本次遗忘。请重新发送 /忘记 关键词并核对。"
        }
        "forget_preview_expired" | "forget_preview_not_found" => {
            "遗忘预览已过期或不存在，请重新发送 /忘记 关键词。"
        }
        _ => "未执行遗忘，请检查确认命令，或在 Console 中核对后操作。",
    }
    .into()
}
