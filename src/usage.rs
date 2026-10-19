//! Provider usage is a per-request snapshot, never a stream of additive counters.
use crate::{domain::*, store::Store};
use anyhow::Result;
use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    /// Includes reasoning tokens. Reasoning is a subset, not an additional charge.
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    #[serde(skip)]
    raw: UsageFields,
}
#[derive(Clone, Debug, Default, PartialEq)]
struct UsageFields {
    input: Option<u64>,
    output: Option<u64>,
    total: Option<u64>,
    cache_read: u64,
    cache_write: u64,
    tool_input: u64,
    anthropic: bool,
    gemini: bool,
}

fn number(value: &Value, paths: &[&str]) -> Option<u64> {
    paths
        .iter()
        .find_map(|path| value.pointer(path).and_then(Value::as_u64))
}

impl TokenUsage {
    /// Merge fields before deriving totals: some relays send input and output in
    /// separate cumulative snapshots. Null/absent fields must not erase known data.
    pub fn update(&mut self, chunk: &Value) -> bool {
        let value = chunk
            .get("usage")
            .filter(|v| v.is_object())
            .or_else(|| chunk.get("usageMetadata").filter(|v| v.is_object()))
            .or_else(|| chunk.pointer("/message/usage").filter(|v| v.is_object()));
        let Some(value) = value else { return false };
        let previous = self.clone();
        if value.get("prompt_tokens").is_some() {
            self.raw.anthropic = false;
        } else if value.get("input_tokens").is_some() {
            self.raw.anthropic = true;
        }
        if value.get("promptTokenCount").is_some()
            || value.get("candidatesTokenCount").is_some()
            || value.get("thoughtsTokenCount").is_some()
            || value.get("toolUsePromptTokenCount").is_some()
        {
            self.raw.gemini = true;
        }
        if let Some(input) = number(
            value,
            &["/prompt_tokens", "/input_tokens", "/promptTokenCount"],
        ) {
            self.raw.input = Some(input);
        }
        if let Some(output) = number(
            value,
            &[
                "/completion_tokens",
                "/output_tokens",
                "/candidatesTokenCount",
            ],
        ) {
            self.raw.output = Some(output);
        }
        if let Some(read) = number(value, &["/cache_read_input_tokens"]) {
            self.raw.cache_read = read;
        }
        if let Some(write) = number(value, &["/cache_creation_input_tokens"]) {
            self.raw.cache_write = write;
        }
        if let Some(tools) = number(value, &["/toolUsePromptTokenCount"]) {
            self.raw.tool_input = tools;
        }
        if let Some(reasoning) = number(
            value,
            &[
                "/completion_tokens_details/reasoning_tokens",
                "/output_tokens_details/reasoning_tokens",
                "/output_tokens_details/thinking_tokens",
                "/reasoning_tokens",
                "/thoughtsTokenCount",
            ],
        ) {
            self.reasoning_tokens = Some(reasoning);
        }
        self.input_tokens = self.raw.input.map(|n| {
            if self.raw.anthropic {
                n.saturating_add(self.raw.cache_read)
                    .saturating_add(self.raw.cache_write)
            } else if self.raw.gemini {
                n.saturating_add(self.raw.tool_input)
            } else {
                n
            }
        });
        self.output_tokens = self.raw.output.map(|n| {
            if self.raw.gemini {
                n.saturating_add(self.reasoning_tokens.unwrap_or(0))
            } else {
                n
            }
        });
        if let Some(cached) = number(
            value,
            &[
                "/prompt_tokens_details/cached_tokens",
                "/input_tokens_details/cached_tokens",
                "/prompt_cache_hit_tokens",
                "/cache_read_input_tokens",
                "/cachedContentTokenCount",
            ],
        ) {
            self.cached_input_tokens = Some(cached);
        }
        // Explicit provider totals take precedence over arithmetic derived from
        // API fields; keep them when later frames omit the total.
        if let Some(total) = number(value, &["/total_tokens", "/totalTokenCount"]) {
            self.raw.total = Some(total);
        } else if self.input_tokens != previous.input_tokens
            || self.output_tokens != previous.output_tokens
        {
            // An earlier cumulative total is stale once its component counters
            // change. A later partial frame may omit the new total.
            self.raw.total = None;
        }
        self.total_tokens = self.raw.total.or_else(|| {
            self.input_tokens
                .zip(self.output_tokens)
                .map(|(i, o)| i.saturating_add(o))
        });
        *self != previous
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct ContextTokens {
    pub system: usize,
    pub summary: usize,
    pub memory: usize,
    pub history: usize,
    pub current: usize,
    pub tools: usize,
    pub overhead: usize,
}
impl ContextTokens {
    pub fn total(&self) -> usize {
        self.system
            + self.summary
            + self.memory
            + self.history
            + self.current
            + self.tools
            + self.overhead
    }
    pub fn from_wire(messages: &[Value], tools: Option<&Value>) -> Self {
        let mut result = Self {
            overhead: 3,
            ..Self::default()
        };
        let current = messages.iter().rposition(|m| {
            m["role"] == "user"
                && m["name"] != "tool_images"
                && !m["content"].as_str().is_some_and(|s| {
                    s.starts_with("<untrusted_") || s.contains("<untrusted_tool_results>")
                })
        });
        let mut block = "";
        for (index, message) in messages.iter().enumerate() {
            let text = message["content"].as_str().unwrap_or("");
            let size = crate::tokens::wire_message(message);
            if message["role"] == "system" {
                result.system += size;
            } else if text.starts_with("<untrusted_conversation_summary>") {
                result.summary += size;
                block = "summary";
            } else if text.starts_with("<untrusted_long_term_memory>") {
                result.memory += size;
                block = "memory";
            } else if message["role"] == "assistant" && !block.is_empty() {
                if block == "summary" {
                    result.summary += size;
                } else {
                    result.memory += size;
                }
                block = "";
            } else if message["role"] == "tool"
                || message.get("tool_calls").is_some()
                || message["name"] == "tool_images"
                || text.contains("<untrusted_tool_results>")
            {
                result.tools += size;
            } else if Some(index) == current {
                result.current += size;
            } else {
                result.history += size;
            }
        }
        result.tools += tools
            .map(|t| crate::tokens::count(&t.to_string()) + 20)
            .unwrap_or(0);
        result
    }
}

#[derive(Serialize)]
pub struct RequestUsage {
    pub id: String,
    pub turn_id: Option<String>,
    pub purpose: String,
    pub model: String,
    pub context: ContextTokens,
    pub usage: TokenUsage,
    pub reasoning: String,
    pub status: String,
}
impl<'r> sqlx::FromRow<'r, sqlx::sqlite::SqliteRow> for RequestUsage {
    fn from_row(row: &'r sqlx::sqlite::SqliteRow) -> std::result::Result<Self, sqlx::Error> {
        use sqlx::Row;
        let decode = |column: &str| -> std::result::Result<Value, sqlx::Error> {
            serde_json::from_str(row.try_get::<&str, _>(column)?).map_err(|source| {
                sqlx::Error::ColumnDecode {
                    index: column.into(),
                    source: Box::new(source),
                }
            })
        };
        Ok(Self {
            id: row.try_get("id")?,
            turn_id: row.try_get("turn_id")?,
            purpose: row.try_get("purpose")?,
            model: row.try_get("model")?,
            context: serde_json::from_value(decode("context")?)
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            usage: serde_json::from_value(decode("usage")?)
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            reasoning: row.try_get("reasoning")?,
            status: row.try_get("status")?,
        })
    }
}

/// Nullable sums remain null when every provider omitted a field. Coverage is
/// exposed separately so partial totals never masquerade as complete accounting.
pub fn aggregate(requests: &[RequestUsage]) -> Value {
    let mut total = UsageTotal::default();
    for request in requests {
        total.add(&request.usage);
        total.incomplete_requests += usize::from(request.status != "complete");
    }
    serde_json::to_value(total).expect("token counters serialize")
}

impl Store {
    pub(crate) async fn start_model_request(
        &self,
        turn: Option<&str>,
        purpose: &str,
        model: &str,
        context: &ContextTokens,
    ) -> Result<String> {
        let session: String = if let Some(turn) = turn {
            sqlx::query_scalar("SELECT session_id FROM turns WHERE id=?")
                .bind(turn)
                .fetch_one(&self.pool)
                .await?
        } else {
            self.conversation().await?
        };
        let request = id();
        sqlx::query("INSERT INTO model_requests(id,session_id,turn_id,purpose,model,context,created_at) VALUES(?,?,?,?,?,?,?)")
            .bind(&request).bind(session).bind(turn).bind(purpose).bind(model).bind(serde_json::to_string(context)?).bind(now()).execute(&self.pool).await?;
        Ok(request)
    }
    pub(crate) async fn update_model_request(
        &self,
        request: &str,
        usage: &TokenUsage,
        reasoning: &str,
        status: &str,
    ) -> Result<()> {
        sqlx::query("UPDATE model_requests SET usage=?,reasoning=?,status=? WHERE id=?")
            .bind(serde_json::to_string(usage)?)
            .bind(reasoning)
            .bind(status)
            .bind(request)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    pub async fn turn_model_requests(&self, turn: &str) -> Result<Vec<RequestUsage>> {
        Ok(sqlx::query_as("SELECT id,turn_id,purpose,model,context,usage,reasoning,status FROM model_requests WHERE turn_id=? ORDER BY seq")
            .bind(turn).fetch_all(&self.pool).await?)
    }
    pub(crate) async fn request_history<'a>(
        &self,
        turns: impl Iterator<Item = &'a str>,
    ) -> Result<Vec<RequestUsage>> {
        let turns: std::collections::BTreeSet<_> = turns.collect();
        if turns.is_empty() {
            return Ok(Vec::new());
        }
        let mut query = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
            "SELECT id,turn_id,purpose,model,context,usage,reasoning,status FROM model_requests WHERE turn_id IN (",
        );
        let mut ids = query.separated(",");
        for turn in turns {
            ids.push_bind(turn);
        }
        ids.push_unseparated(") ORDER BY seq");
        Ok(query.build_query_as().fetch_all(&self.pool).await?)
    }
    pub(crate) async fn last_model_request(&self, session: &str) -> Result<Option<RequestUsage>> {
        Ok(sqlx::query_as("SELECT id,turn_id,purpose,model,context,usage,'' AS reasoning,status FROM model_requests WHERE session_id=? AND purpose='chat' ORDER BY seq DESC LIMIT 1")
            .bind(session).fetch_optional(&self.pool).await?)
    }
    pub(crate) async fn session_usage(&self, session: &str) -> Result<Value> {
        // Read counters only, never load the session's stored thinking text here.
        let mut rows = sqlx::query_as::<_, (String, String, String)>(
            "SELECT purpose,usage,status FROM model_requests WHERE session_id=?",
        )
        .bind(session)
        .fetch(&self.pool);
        let mut all = UsageTotal::default();
        let mut purposes = std::collections::BTreeMap::<String, UsageTotal>::new();
        while let Some((purpose, usage, status)) = rows.try_next().await? {
            let usage: TokenUsage = serde_json::from_str(&usage)?;
            all.add(&usage);
            all.incomplete_requests += usize::from(status != "complete");
            let group = purposes.entry(purpose).or_default();
            group.add(&usage);
            group.incomplete_requests += usize::from(status != "complete");
        }
        let mut value = serde_json::to_value(all)?;
        value["by_purpose"] = serde_json::to_value(purposes)?;
        Ok(value)
    }
}

#[derive(Default, Serialize)]
struct Coverage {
    input_tokens: usize,
    output_tokens: usize,
    reasoning_tokens: usize,
    total_tokens: usize,
}
#[derive(Default, Serialize)]
struct UsageTotal {
    requests: usize,
    incomplete_requests: usize,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    total_tokens: Option<u64>,
    reported: Coverage,
}
impl UsageTotal {
    fn add(&mut self, usage: &TokenUsage) {
        self.requests += 1;
        for (target, count, value) in [
            (
                &mut self.input_tokens,
                &mut self.reported.input_tokens,
                usage.input_tokens,
            ),
            (
                &mut self.output_tokens,
                &mut self.reported.output_tokens,
                usage.output_tokens,
            ),
            (
                &mut self.reasoning_tokens,
                &mut self.reported.reasoning_tokens,
                usage.reasoning_tokens,
            ),
            (
                &mut self.total_tokens,
                &mut self.reported.total_tokens,
                usage.total_tokens,
            ),
        ] {
            if let Some(n) = value {
                *target = Some(target.unwrap_or(0).saturating_add(n));
                *count += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn openai_snapshots_do_not_add_reasoning_or_cached_input_twice() {
        let mut usage = TokenUsage::default();
        assert!(!usage.update(&json!({"usage":null})));
        let snapshot = json!({"usage":{"prompt_tokens":100,"completion_tokens":40,
            "completion_tokens_details":{"reasoning_tokens":30},"prompt_tokens_details":{"cached_tokens":60},"total_tokens":140}});
        assert!(usage.update(&snapshot));
        assert!(!usage.update(&snapshot));
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(40));
        assert_eq!(usage.reasoning_tokens, Some(30));
        assert_eq!(usage.total_tokens, Some(140));
        assert_eq!(usage.cached_input_tokens, Some(60));
        assert!(!usage.update(&json!({"usage":{"prompt_tokens":null}})));
    }

    #[test]
    fn partial_snapshots_and_missing_fields_stay_distinct_from_zero() {
        let mut usage = TokenUsage::default();
        usage.update(&json!({"usage":{"input_tokens":120,"output_tokens":0,"total_tokens":120}}));
        assert_eq!(usage.total_tokens, Some(120));
        usage.update(
            &json!({"usage":{"output_tokens":22,"output_tokens_details":{"reasoning_tokens":0}}}),
        );
        assert_eq!(usage.input_tokens, Some(120));
        assert_eq!(usage.total_tokens, Some(142));
        assert_eq!(usage.reasoning_tokens, Some(0));
        let mut missing = TokenUsage::default();
        missing.update(&json!({"usage":{"total_tokens":42}}));
        assert_eq!(missing.total_tokens, Some(42));
        assert_eq!(missing.input_tokens, None);
        assert_eq!(missing.reasoning_tokens, None);
    }

    #[test]
    fn anthropic_cache_counters_and_cumulative_output_are_normalized() {
        let mut usage = TokenUsage::default();
        usage.update(
            &json!({"message":{"usage":{"input_tokens":10,"output_tokens":1,
            "cache_read_input_tokens":60,"cache_creation_input_tokens":30}}}),
        );
        usage.update(&json!({"usage":{"output_tokens":35}}));
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(35));
        assert_eq!(usage.total_tokens, Some(135));
        assert_eq!(usage.reasoning_tokens, None);
    }

    #[test]
    fn gemini_thinking_is_added_to_generated_output_once_even_in_partial_snapshots() {
        let mut usage = TokenUsage::default();
        let first = json!({"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":20,"cachedContentTokenCount":40}});
        usage.update(&first);
        let final_frame = json!({"usageMetadata":{"thoughtsTokenCount":30,"toolUsePromptTokenCount":10,"totalTokenCount":160}});
        usage.update(&final_frame);
        assert_eq!(usage.input_tokens, Some(110));
        assert_eq!(usage.output_tokens, Some(50));
        assert_eq!(usage.reasoning_tokens, Some(30));
        assert_eq!(usage.total_tokens, Some(160));
        assert!(!usage.update(&final_frame));
    }

    #[test]
    fn context_distribution_matches_the_wire_including_memory_tools_and_images() {
        let messages = vec![
            json!({"role":"system","content":"system instructions"}),
            json!({"role":"user","content":"<untrusted_conversation_summary>摘要</untrusted_conversation_summary>"}),
            json!({"role":"assistant","content":"摘要确认"}),
            json!({"role":"user","content":"<untrusted_long_term_memory>记忆</untrusted_long_term_memory>"}),
            json!({"role":"assistant","content":"记忆确认"}),
            json!({"role":"user","content":"history"}),
            json!({"role":"assistant","content":"answer"}),
            json!({"role":"user","content":"current"}),
            json!({"role":"assistant","content":null,"reasoning_content":"actually replayed reasoning", "tool_calls":[{"id":"call","function":{"name":"browser","arguments":"{}"}}]}),
            json!({"role":"tool","content":"tool result","tool_call_id":"call"}),
            json!({"role":"user","name":"tool_images","content":[{"type":"image_url","image_url":{"url":"data:image/jpeg;base64,AAAA"}}]}),
        ];
        let definitions = crate::tools::definitions();
        let context = ContextTokens::from_wire(&messages, Some(&definitions));
        assert_eq!(
            context.total(),
            3 + messages
                .iter()
                .map(crate::tokens::wire_message)
                .sum::<usize>()
                + crate::tokens::count(&definitions.to_string())
                + 20
        );
        assert!(
            context.summary > 0
                && context.memory > 0
                && context.system > 0
                && context.history > 0
                && context.tools > 1024
        );
        assert_eq!(context.current, crate::tokens::message("current"));
    }

    #[test]
    fn totals_reveal_incomplete_coverage_instead_of_reporting_missing_as_zero() {
        let mut total = UsageTotal::default();
        total.add(&TokenUsage::default());
        assert_eq!(total.total_tokens, None);
        let mut usage = TokenUsage::default();
        usage.update(&json!({"usage":{"prompt_tokens":100,"completion_tokens":40,"completion_tokens_details":{"reasoning_tokens":30}}}));
        total.add(&usage);
        total.add(&usage);
        assert_eq!(total.requests, 3);
        assert_eq!(total.total_tokens, Some(280));
        assert_eq!(total.reasoning_tokens, Some(60));
        assert_eq!(total.reported.total_tokens, 2);
    }
}
