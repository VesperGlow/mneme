//! 远程 MCP 工具（streamable-http）：手写的最小 JSON-RPC 客户端。
//! 采用"每次调用新建连接"策略——initialize → 请求 → 丢弃会话，
//! 避免长连接会话管理的复杂度，对个人项目的调用频率完全够用。

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use crate::config::{Config, McpServer};

/// 内置工具一律无前缀；MCP 工具统一加这个前缀，避免与内置工具或彼此重名。
pub const MCP_TOOL_PREFIX: &str = "mcp__";

const PROTOCOL_VERSION: &str = "2025-06-18";

/// MCP 的 key 常嵌在 URL（Tavily 在查询串、Firecrawl 在路径），出错时异常文本会带出来。
/// 写日志或回传错误前先脱敏，避免密钥落到日志、模型上下文或 API 响应里。
pub fn redact_secret(text: &str) -> String {
    let patterns = [
        (r"(?i)([?&][\w-]*(?:key|token|secret|password)=)[^&\s\x22']+", "$1***"),
        (r"\b((?:fc|tvly|sk)-)[A-Za-z0-9._-]{6,}", "$1***"),
        (r"(?i)(bearer\s+)[A-Za-z0-9._\-]{6,}", "$1***"),
        (r"(https?://)[^/@\s]+@", "$1***@"),
    ];
    let mut result = text.to_string();
    for (pattern, replacement) in patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            result = re.replace_all(&result, replacement).into_owned();
        }
    }
    result
}

fn accepts(server: &McpServer, tool_name: &str) -> bool {
    if !server.include.is_empty() && !server.include.iter().any(|t| t == tool_name) {
        return false;
    }
    !server.exclude.iter().any(|t| t == tool_name)
}

/// 一次性 JSON-RPC 交换：initialize → initialized 通知 → 发送请求 → 返回 result。
struct McpConnection<'a> {
    http: &'a reqwest::Client,
    server: &'a McpServer,
    session_id: Option<String>,
    next_id: i64,
}

impl<'a> McpConnection<'a> {
    async fn open(http: &'a reqwest::Client, server: &'a McpServer) -> Result<McpConnection<'a>> {
        let mut conn = Self {
            http,
            server,
            session_id: None,
            next_id: 1,
        };
        let init = conn
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "mneme", "version": env!("CARGO_PKG_VERSION")},
                }),
            )
            .await
            .context("MCP initialize 失败")?;
        let _ = init; // 协商结果目前只需要会话头，已在 request 里捕获。
        conn.notify("notifications/initialized").await?;
        Ok(conn)
    }

    fn builder(&self, body: &Value) -> reqwest::RequestBuilder {
        let mut request = self
            .http
            .post(&self.server.url)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .json(body);
        if let Some(session) = &self.session_id {
            request = request.header("Mcp-Session-Id", session);
        }
        for (key, value) in &self.server.headers {
            request = request.header(key, value);
        }
        request
    }

    async fn notify(&mut self, method: &str) -> Result<()> {
        let body = json!({"jsonrpc": "2.0", "method": method});
        let response = self.builder(&body).send().await?;
        if !response.status().is_success() {
            bail!("MCP 通知 {method} 返回 HTTP {}", response.status());
        }
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let response = self.builder(&body).send().await?;
        if let Some(session) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session_id = Some(session.to_string());
        }
        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let text = response.text().await?;
        if !status.is_success() {
            bail!("MCP {method} 返回 HTTP {status}：{}", &text.chars().take(500).collect::<String>());
        }

        let message = if content_type.contains("text/event-stream") {
            find_response_in_sse(&text, id)?
        } else {
            serde_json::from_str(&text).context("MCP 响应不是 JSON")?
        };
        if let Some(error) = message.get("error") {
            bail!("MCP {method} 出错：{error}");
        }
        message
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("MCP {method} 响应缺少 result"))
    }
}

/// 从 SSE 流文本里找出 id 匹配的 JSON-RPC 响应（流里可能夹杂进度通知）。
fn find_response_in_sse(body: &str, id: i64) -> Result<Value> {
    let mut data_lines: Vec<&str> = Vec::new();
    let mut dispatch = |lines: &mut Vec<&str>| -> Option<Value> {
        if lines.is_empty() {
            return None;
        }
        let payload = lines.join("\n");
        lines.clear();
        serde_json::from_str::<Value>(&payload).ok()
    };
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start());
        } else if line.is_empty() {
            if let Some(message) = dispatch(&mut data_lines) {
                if message.get("id").and_then(|v| v.as_i64()) == Some(id) {
                    return Ok(message);
                }
            }
        }
    }
    if let Some(message) = dispatch(&mut data_lines) {
        if message.get("id").and_then(|v| v.as_i64()) == Some(id) {
            return Ok(message);
        }
    }
    bail!("SSE 流里没有找到 id={id} 的响应")
}

pub struct McpManager {
    cfg: Arc<Config>,
    /// 限定名（mcp__server__tool） -> (服务器下标, 原始工具名)
    index: Vec<(String, usize, String)>,
    openai_tools: Vec<Value>,
    list_http: reqwest::Client,
    call_http: reqwest::Client,
}

impl McpManager {
    pub fn new(cfg: Arc<Config>) -> Result<Self> {
        Ok(Self {
            list_http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?,
            call_http: reqwest::Client::builder()
                .timeout(Duration::from_secs_f64(crate::config::MCP_TIMEOUT_SECONDS))
                .build()?,
            cfg,
            index: Vec::new(),
            openai_tools: Vec::new(),
        })
    }

    pub fn enabled(&self) -> bool {
        !self.cfg.mcp_servers.is_empty()
    }

    pub fn openai_tools(&self) -> &[Value] {
        &self.openai_tools
    }

    pub fn owns(&self, name: &str) -> bool {
        self.index.iter().any(|(qualified, _, _)| qualified == name)
    }

    /// 启动时抓取各服务器的工具清单。单个服务器失败只记日志并跳过（降级）。
    pub async fn start(&mut self) -> Result<()> {
        for (server_index, server) in self.cfg.mcp_servers.iter().enumerate() {
            let tools = match self.list_tools(server).await {
                Ok(tools) => tools,
                Err(error) => {
                    tracing::warn!(
                        "加载 MCP 服务器 {} 的工具失败，已跳过：{}",
                        server.name,
                        redact_secret(&format!("{error:#}"))
                    );
                    continue;
                }
            };
            let available: Vec<&str> = tools
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .collect();
            for missing in server.include.iter().filter(|name| !available.contains(&name.as_str())) {
                tracing::warn!("MCP 服务器 {} 的 tools 白名单里的 {missing} 不存在", server.name);
            }
            let mut registered = 0;
            for tool in &tools {
                let Some(tool_name) = tool["name"].as_str() else { continue };
                if !accepts(server, tool_name) {
                    continue;
                }
                let qualified = format!("{MCP_TOOL_PREFIX}{}__{tool_name}", server.name);
                let description: String = tool["description"]
                    .as_str()
                    .unwrap_or("")
                    .chars()
                    .take(1024)
                    .collect();
                let schema = normalize_schema(&tool["inputSchema"]);
                self.openai_tools.push(json!({
                    "type": "function",
                    "function": {"name": qualified, "description": description, "parameters": schema},
                }));
                self.index
                    .push((qualified, server_index, tool_name.to_string()));
                registered += 1;
            }
            tracing::info!(
                "MCP 服务器 {} 共 {} 个工具，注册 {registered} 个",
                server.name,
                tools.len()
            );
        }
        Ok(())
    }

    async fn list_tools(&self, server: &McpServer) -> Result<Vec<Value>> {
        let mut conn = McpConnection::open(&self.list_http, server).await?;
        let result = conn.request("tools/list", json!({})).await?;
        Ok(result["tools"].as_array().cloned().unwrap_or_default())
    }

    pub async fn call(&self, name: &str, arguments: &Value) -> Result<Value> {
        let (_, server_index, tool_name) = self
            .index
            .iter()
            .find(|(qualified, _, _)| qualified == name)
            .ok_or_else(|| anyhow!("未知的 MCP 工具：{name}"))?;
        let server = &self.cfg.mcp_servers[*server_index];
        let result = async {
            let mut conn = McpConnection::open(&self.call_http, server).await?;
            conn.request(
                "tools/call",
                json!({"name": tool_name, "arguments": arguments}),
            )
            .await
        }
        .await
        // 脱敏后再抛出：上层会把它写进 tool_events、回传给模型与 API 响应。
        .map_err(|error| anyhow!("调用 MCP 工具 {name} 失败：{}", redact_secret(&format!("{error:#}"))))?;
        Ok(serialize_result(&result, crate::config::MCP_RESULT_MAX_CHARS))
    }
}

fn normalize_schema(schema: &Value) -> Value {
    if schema.get("type").and_then(|v| v.as_str()) == Some("object") {
        schema.clone()
    } else {
        // 部分 MCP 工具不带 inputSchema 或给的不是 object，补一个空对象 schema。
        json!({"type": "object", "properties": {}})
    }
}

fn serialize_result(result: &Value, max_chars: usize) -> Value {
    let mut texts: Vec<String> = Vec::new();
    for block in result["content"].as_array().unwrap_or(&Vec::new()) {
        if let Some(text) = block["text"].as_str() {
            texts.push(text.to_string());
        } else {
            let block_type = block["type"].as_str().unwrap_or("content");
            texts.push(format!("[{block_type} 内容已省略]"));
        }
    }
    let mut combined = texts.join("\n");
    if combined.chars().count() > max_chars {
        combined = combined.chars().take(max_chars).collect::<String>() + "…（结果过长已截断）";
    }
    let mut payload = json!({
        "is_error": result["isError"].as_bool().unwrap_or(false),
        "text": combined,
    });
    if let Some(structured) = result.get("structuredContent") {
        if !structured.is_null() {
            payload["structured"] = structured.clone();
        }
    }
    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_keys_in_urls_and_tokens() {
        let text = "https://mcp.tavily.com/mcp/?tavilyApiKey=tvly-abcdef123456 Bearer sk-secretsecret";
        let redacted = redact_secret(text);
        assert!(!redacted.contains("tvly-abcdef123456"));
        assert!(!redacted.contains("sk-secretsecret"));
    }

    #[test]
    fn finds_response_among_sse_events() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n\
                    data: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"ok\":true}}\n\n";
        let message = find_response_in_sse(body, 2).unwrap();
        assert_eq!(message["result"]["ok"], true);
    }

    #[test]
    fn truncates_long_results() {
        let result = json!({"content": [{"type": "text", "text": "啊".repeat(50)}], "isError": false});
        let serialized = serialize_result(&result, 10);
        let text = serialized["text"].as_str().unwrap();
        assert!(text.starts_with("啊啊啊啊啊啊啊啊啊啊…"));
    }
}
