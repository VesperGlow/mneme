use crate::{
    App,
    api::{ApiError, ApiResult, valid_id},
    domain::{id, now},
};
use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Deserialize, Serialize)]
pub struct ImageData {
    pub mime: String,
    pub data: String,
    pub width: u32,
    pub height: u32,
}

pub fn routes() -> Router<App> {
    Router::new()
        .route(
            "/v1/sessions/{id}/images",
            post(upload).layer(DefaultBodyLimit::max(8 * 1024 * 1024)),
        )
        .route("/v1/assets/{id}", get(image))
}
async fn upload(
    State(app): State<App>,
    Path(session): Path<String>,
    bytes: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    valid_id(&session)?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?)")
        .bind(&session)
        .fetch_one(&app.store.pool)
        .await
        .map_err(anyhow::Error::from)?;
    if !exists {
        return Err(ApiError(StatusCode::NOT_FOUND, "session_not_found"));
    }
    let client = app
        .tools
        .as_ref()
        .ok_or(ApiError(StatusCode::SERVICE_UNAVAILABLE, "tools_disabled"))?;
    let normalized = client.normalize(bytes.to_vec()).await.map_err(|_| {
        ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_image_or_tools_unavailable",
        )
    })?;
    let asset = save(&app, &session, &normalized, "upload").await?;
    Ok((
        StatusCode::CREATED,
        Json(
            json!({"asset_id":asset,"mime":normalized.mime,"width":normalized.width,"height":normalized.height}),
        ),
    ))
}
async fn image(State(app): State<App>, Path(asset): Path<String>) -> ApiResult<Response> {
    valid_id(&asset)?;
    let row: Option<(String, Vec<u8>)> = sqlx::query_as("SELECT mime,data FROM assets WHERE id=?")
        .bind(asset)
        .fetch_optional(&app.store.pool)
        .await
        .map_err(anyhow::Error::from)?;
    let Some((mime, data)) = row else {
        return Err(ApiError(StatusCode::NOT_FOUND, "image_not_found"));
    };
    let mut response = data.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        mime.parse()
            .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "invalid_image"))?,
    );
    response
        .headers_mut()
        .insert(header::CONTENT_DISPOSITION, "inline".parse().unwrap());
    Ok(response)
}
pub async fn save(app: &App, session: &str, image: &ImageData, kind: &str) -> Result<String> {
    ensure!(
        image.mime == "image/jpeg"
            && image.width > 0
            && image.height > 0
            && image.width <= 2048
            && image.height <= 2048,
        "invalid normalized image"
    );
    let data = STANDARD.decode(&image.data)?;
    ensure!(
        data.len() <= 2 * 1024 * 1024 && data.starts_with(&[0xff, 0xd8, 0xff]),
        "invalid normalized image"
    );
    let mut tx = app.store.pool.begin_with("BEGIN IMMEDIATE").await?;
    let total: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(length(data)),0) FROM assets")
        .fetch_one(&mut *tx)
        .await?;
    ensure!(
        total + data.len() as i64 <= 256 * 1024 * 1024,
        "image storage limit reached"
    );
    let asset = id();
    sqlx::query("INSERT INTO assets(id,session_id,mime,data,width,height,kind,created_at) VALUES(?,?,?,?,?,?,?,?)")
        .bind(&asset).bind(session).bind(&image.mime).bind(data).bind(image.width).bind(image.height).bind(kind).bind(now()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(asset)
}
pub async fn content(app: &App, session: &str, asset: &str) -> Result<Option<Value>> {
    let row: Option<(String, Vec<u8>)> =
        sqlx::query_as("SELECT mime,data FROM assets WHERE id=? AND session_id=?")
            .bind(asset)
            .bind(session)
            .fetch_optional(&app.store.pool)
            .await?;
    Ok(row.map(|(mime, data)| json!({"type":"image_url","image_url":{"url":format!("data:{mime};base64,{}", STANDARD.encode(data)),"detail":"auto"}})))
}
pub async fn attach_current(
    app: &App,
    user: &crate::domain::Message,
    messages: &mut [Value],
) -> Result<()> {
    let assets: Vec<String> = sqlx::query_scalar(
        "SELECT asset_id FROM message_assets WHERE message_id=? ORDER BY position",
    )
    .bind(&user.id)
    .fetch_all(&app.store.pool)
    .await?;
    if assets.is_empty() {
        return Ok(());
    }
    let mut parts = vec![json!({"type":"text","text":user.content})];
    for (index, asset) in assets.into_iter().enumerate() {
        parts.push(
            json!({"type":"text","text":format!("用户上传的第 {} 张图片，标识 {asset}", index+1)}),
        );
        if let Some(part) = content(app, &user.session_id, &asset).await? {
            parts.push(part);
        }
    }
    messages.last_mut().unwrap()["content"] = json!(parts);
    Ok(())
}
pub async fn history_metadata(app: &App, session: &str) -> Result<Value> {
    let rows: Vec<(String,String,String,u32,u32)> = sqlx::query_as("SELECT a.id,ma.message_id,a.kind,a.width,a.height FROM assets a JOIN message_assets ma ON ma.asset_id=a.id WHERE a.session_id=? ORDER BY ma.message_id,ma.position LIMIT 400")
        .bind(session).fetch_all(&app.store.pool).await?;
    Ok(json!(rows.into_iter().map(|(id,message_id,kind,width,height)| json!({"id":id,"message_id":message_id,"kind":kind,"width":width,"height":height})).collect::<Vec<_>>()))
}
