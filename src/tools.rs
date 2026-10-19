use crate::{
    App,
    api::{ApiError, ApiResult},
    assets::ImageData,
    config::ToolsConfig,
    domain::{id, now, truncate},
    llm::{bounded_body, emit},
};
use anyhow::{Result, ensure};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::sse::Event,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    url: String,
    token: String,
    pub metrics: std::sync::Arc<crate::telemetry::Metrics>,
    reset_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
}
impl Client {
    pub fn new(config: &ToolsConfig) -> Result<Self> {
        let builder = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(config.timeout_seconds))
            .redirect(reqwest::redirect::Policy::none());
        Ok(Self {
            http: builder.build()?,
            url: config.base_url.trim_end_matches('/').to_owned(),
            token: config.token.clone(),
            metrics: Default::default(),
            reset_lock: Default::default(),
        })
    }
    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let mut request = self
            .http
            .request(method, format!("{}{path}", self.url))
            .bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await?;
        let status = response.status();
        let data: Value = serde_json::from_slice(&bounded_body(response, 4 * 1024 * 1024).await?)?;
        if !status.is_success() {
            let code = data["error"]
                .as_str()
                .filter(|s| s.len() <= 80 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
                .unwrap_or("tools_unavailable");
            anyhow::bail!("{code}");
        }
        Ok(data)
    }
    pub async fn health(&self) -> Result<Value> {
        self.request(reqwest::Method::GET, "/health", None).await
    }
    pub async fn reset(&self) -> Result<()> {
        self.request(reqwest::Method::POST, "/v1/reset", Some(json!({})))
            .await?;
        Ok(())
    }
    pub async fn close_session(&self, session: &str) -> Result<()> {
        self.request(
            reqwest::Method::DELETE,
            &format!("/v1/sessions/{session}"),
            None,
        )
        .await?;
        Ok(())
    }
    pub async fn fetch_image(&self, url: &str) -> Result<ImageData> {
        Ok(serde_json::from_value(
            self.request(
                reqwest::Method::POST,
                "/v1/images/fetch",
                Some(json!({"url":url})),
            )
            .await?,
        )?)
    }
    pub async fn normalize(&self, bytes: Vec<u8>) -> Result<ImageData> {
        self.normalize_kind(bytes, false).await
    }
    pub async fn normalize_kind(&self, bytes: Vec<u8>, sticker: bool) -> Result<ImageData> {
        let response = self
            .http
            .post(format!(
                "{}/v1/{}/normalize",
                self.url,
                if sticker { "stickers" } else { "images" }
            ))
            .bearer_auth(&self.token)
            .header("content-type", "application/octet-stream")
            .body(bytes)
            .send()
            .await?
            .error_for_status()?;
        Ok(serde_json::from_slice(
            &bounded_body(response, 4 * 1024 * 1024).await?,
        )?)
    }
    async fn execute(&self, session: &str, arguments: &Value) -> Result<Value> {
        self.request(
            reqwest::Method::POST,
            "/v1/execute",
            Some(json!({"session_id":session,"arguments":arguments})),
        )
        .await
    }
}

pub fn definitions() -> Value {
    let mut catalog: Value =
        serde_json::from_str(include_str!("../services/tools/catalog.json")).unwrap();
    catalog["tools"].as_array_mut().unwrap().push(json!({"type":"function","function":{"name":"view_image","description":"查看当前会话中已上传或浏览器截取的图片。由当前多模态模型直接理解图片，asset_id 必须来自本会话图片标识或工具结果。","parameters":{"type":"object","additionalProperties":false,"required":["asset_id"],"properties":{"asset_id":{"type":"string"}}}}}));
    catalog["tools"].take()
}

pub async fn execute(
    app: &App,
    user: &crate::domain::Message,
    call: &crate::llm::ToolCall,
    output: &mpsc::Sender<Event>,
) -> Result<(Value, Option<Value>)> {
    let log_id = id();
    let arguments = serde_json::from_str::<Value>(&call.function.arguments).unwrap_or(Value::Null);
    let action = if call.function.name == "browser" {
        arguments["action"].as_str().unwrap_or("invalid")
    } else {
        "view_image"
    };
    let summary = match action {
        "search" => json!({"query":truncate(arguments["query"].as_str().unwrap_or(""),500)}),
        "navigate" => json!({"url":safe_url(arguments["url"].as_str().unwrap_or(""))}),
        "type" => json!({"element":arguments["element"],"text":"[输入内容不记录]"}),
        _ => json!({"element":arguments["element"],"asset_id":arguments["asset_id"]}),
    };
    sqlx::query("INSERT INTO tool_calls(id,session_id,turn_id,call_id,name,action,arguments,status,created_at) VALUES(?,?,?,?,?,?,?,'running',?)")
        .bind(&log_id).bind(&user.session_id).bind(&user.turn_id).bind(&call.id).bind(&call.function.name).bind(action).bind(summary.to_string()).bind(now()).execute(&app.store.pool).await?;
    let _ = emit(
        output,
        "tool",
        json!({"id":log_id,"name":call.function.name,"action":action,"status":"running"}),
    );
    let start = Instant::now();
    let result = async {
        ensure!(!arguments.is_null(), "invalid_arguments");
        let client = app
            .tools
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("tools_disabled"))?;
        let reset_guard = client.reset_lock.lock().await;
        let pending: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM metadata WHERE key='tools.reset_pending')",
        )
        .fetch_one(&app.store.pool)
        .await?;
        if pending {
            client.reset().await?;
            sqlx::query("DELETE FROM metadata WHERE key='tools.reset_pending'")
                .execute(&app.store.pool)
                .await?;
        }
        drop(reset_guard);
        if call.function.name == "view_image" {
            let asset = arguments["asset_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("invalid_arguments"))?;
            let part = crate::assets::content(app, &user.session_id, asset)
                .await?
                .ok_or_else(|| anyhow::anyhow!("image_not_found"))?;
            return Ok((
                json!({"asset_id":asset,"image":"图片已附加，请由当前模型查看。"}),
                Some(part),
            ));
        }
        ensure!(call.function.name == "browser", "unknown_tool");
        let response = client.execute(&user.session_id, &arguments).await?;
        let mut result = response["result"].clone();
        let mut image = None;
        if let Some(value) = result.as_object_mut().and_then(|o| o.remove("image")) {
            let normalized: ImageData = serde_json::from_value(value)?;
            let asset =
                crate::assets::save(app, &user.session_id, &normalized, "screenshot").await?;
            image = crate::assets::content(app, &user.session_id, &asset).await?;
            result["asset_id"] = json!(asset);
        }
        Ok::<_, anyhow::Error>((result, image))
    }
    .await;
    let elapsed = start.elapsed().as_millis().min(i64::MAX as u128) as i64;
    if let Some(client) = &app.tools {
        client.metrics.record(start, result.is_ok());
    }
    let (value, image, error) = match result {
        Ok((value, image)) => (value, image, None),
        Err(error) => {
            let message = error.to_string();
            let code = if message.len() <= 80
                && message.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
            {
                message
            } else {
                "tools_unavailable".into()
            };
            (json!({"error":code}), None, Some(code))
        }
    };
    // Store bounded text and links; image bytes live in assets, never in logs or tool messages.
    let value = bounded_result(value, 14000);
    let status = if error.is_some() {
        "failed"
    } else {
        "completed"
    };
    sqlx::query(
        "UPDATE tool_calls SET status=?,result=?,error=?,elapsed_ms=?,completed_at=? WHERE id=?",
    )
    .bind(status)
    .bind(value.to_string())
    .bind(&error)
    .bind(elapsed)
    .bind(now())
    .bind(&log_id)
    .execute(&app.store.pool)
    .await?;
    let _ = emit(
        output,
        "tool",
        json!({"id":log_id,"name":call.function.name,"action":action,"status":status,"elapsed_ms":elapsed,"error":error,"result":value}),
    );
    Ok((value, image))
}
fn safe_url(input: &str) -> String {
    reqwest::Url::parse(input)
        .ok()
        .map(|mut url| {
            url.set_query(None);
            url.set_fragment(None);
            let _ = url.set_username("");
            let _ = url.set_password(None);
            url.to_string()
        })
        .unwrap_or_default()
}
pub fn bounded_result(mut value: Value, limit: usize) -> Value {
    if let Some(text) = value.get_mut("text") {
        *text = json!(truncate(text.as_str().unwrap_or(""), 3000));
    }
    for (field, max) in [("links", 12), ("elements", 30), ("results", 6)] {
        if let Some(items) = value.get_mut(field).and_then(Value::as_array_mut) {
            items.truncate(max);
            for item in items {
                if let Some(snippet) = item.get_mut("snippet") {
                    *snippet = json!(truncate(snippet.as_str().unwrap_or(""), 300));
                }
            }
        }
    }
    while value.to_string().chars().count() > limit {
        let mut reduced = false;
        for field in ["elements", "links", "results"] {
            if let Some(items) = value.get_mut(field).and_then(Value::as_array_mut)
                && !items.is_empty()
            {
                items.pop();
                reduced = true;
                break;
            }
        }
        if !reduced {
            if let Some(text) = value.get_mut("text") {
                let string = text.as_str().unwrap_or("");
                if string.chars().count() > 200 {
                    *text = json!(truncate(string, string.chars().count() / 2));
                    continue;
                }
            }
            break;
        }
    }
    if value.to_string().chars().count() > limit {
        let url = value["url"]
            .as_str()
            .filter(|url| url.chars().count() < limit / 3)
            .unwrap_or("");
        let text = truncate(value["text"].as_str().unwrap_or(""), limit / 3);
        return json!({"url":url,"text":text,"asset_id":value["asset_id"],"error":value["error"],"truncated":true});
    }
    value
}
pub fn routes() -> Router<App> {
    Router::new()
        .route("/v1/console/tools", get(status))
        .route("/v1/console/tools/calls", get(calls))
        .route("/v1/console/tools/close", post(close))
}
async fn status(State(app): State<App>) -> Json<Value> {
    let Some(client) = &app.tools else {
        return Json(json!({"enabled":false}));
    };
    match tokio::time::timeout(Duration::from_secs(4), client.health()).await {
        Ok(Ok(health)) => {
            let state = if health["ready"] == true {
                "healthy"
            } else {
                "unavailable"
            };
            Json(
                json!({"enabled":true,"state":state,"service":health,"metrics":client.metrics.snapshot(),"max_calls":app.config.tools.max_calls}),
            )
        }
        _ => Json(
            json!({"enabled":true,"state":"unavailable","metrics":client.metrics.snapshot(),"max_calls":app.config.tools.max_calls}),
        ),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Filter {
    session_id: Option<String>,
}
async fn calls(State(app): State<App>, Query(filter): Query<Filter>) -> ApiResult<Json<Value>> {
    if let Some(session) = &filter.session_id {
        crate::api::valid_id(session)?;
    }
    Ok(Json(
        json!({"calls":logs(&app,filter.session_id.as_deref()).await?}),
    ))
}
pub async fn logs(app: &App, session: Option<&str>) -> Result<Value> {
    type LogRow = (
        String,
        String,
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
        i64,
    );
    let rows:Vec<LogRow>=sqlx::query_as("SELECT id,session_id,turn_id,name,action,status,result,error,elapsed_ms,created_at FROM tool_calls WHERE (? IS NULL OR session_id=?) ORDER BY created_at DESC,rowid DESC LIMIT 100")
        .bind(session).bind(session).fetch_all(&app.store.pool).await?;
    Ok(json!(rows.into_iter().map(|(id,session_id,turn_id,name,action,status,result,error,elapsed_ms,created_at)|json!({"id":id,"session_id":session_id,"turn_id":turn_id,"name":name,"action":action,"status":status,"result":result.and_then(|r|serde_json::from_str::<Value>(&r).ok()),"error":error,"elapsed_ms":elapsed_ms,"created_at":created_at})).collect::<Vec<_>>()))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Close {
    session_id: String,
}
async fn close(State(app): State<App>, Json(input): Json<Close>) -> ApiResult<Json<Value>> {
    crate::api::valid_id(&input.session_id)?;
    let client = app
        .tools
        .as_ref()
        .ok_or(ApiError(StatusCode::SERVICE_UNAVAILABLE, "tools_disabled"))?;
    client
        .close_session(&input.session_id)
        .await
        .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "tools_unavailable"))?;
    Ok(Json(json!({"closed":true})))
}
