//! Local token estimates for OpenAI-compatible providers. The provider may use another
//! tokenizer or image accounting; expose that distinction in the context API.
use crate::domain::ChatMessage;
use serde_json::Value;
use std::sync::Arc;

pub const TOOL_RESERVE: usize = 12_000;
pub const TOKENIZER: &str = "o200k_base";

/// Text uses the model's tokenizer when available. Chat framing and images are
/// always estimates; a matching vocabulary does not make them provider usage.
#[derive(Clone)]
pub struct Counter {
    native: Option<Arc<tokenizers::Tokenizer>>,
    bpe: &'static tiktoken_rs::CoreBPE,
    pub name: String,
    pub model_matched: bool,
}
impl Default for Counter {
    fn default() -> Self {
        Self {
            native: None,
            bpe: tiktoken_rs::o200k_base_singleton(),
            name: TOKENIZER.into(),
            model_matched: false,
        }
    }
}
impl Counter {
    pub fn new(model: &str, path: Option<&str>) -> anyhow::Result<Self> {
        if let Some(path) = path.filter(|p| !p.is_empty()) {
            let mut native = tokenizers::Tokenizer::from_file(path)
                .map_err(|_| anyhow::anyhow!("cannot load configured tokenizer JSON"))?;
            native
                .with_truncation(None)
                .map_err(|_| anyhow::anyhow!("invalid tokenizer truncation"))?;
            native.with_padding(None);
            return Ok(Self {
                native: Some(Arc::new(native)),
                name: format!("native:{model}"),
                model_matched: true,
                ..Self::default()
            });
        }
        let bpe = tiktoken_rs::bpe_for_model(model).ok();
        Ok(Self {
            bpe: bpe.unwrap_or_else(tiktoken_rs::o200k_base_singleton),
            name: tiktoken_rs::tokenizer::get_tokenizer(model)
                .map(|t| format!("{t:?}"))
                .unwrap_or_else(|| format!("{TOKENIZER} (fallback)")),
            model_matched: bpe.is_some(),
            ..Self::default()
        })
    }
    pub fn count(&self, text: &str) -> usize {
        if let Some(native) = &self.native {
            // Byte count is a conservative fallback for an unexpected encoding
            // failure, never silently report zero tokens.
            native
                .encode(text, false)
                .map(|v| v.len())
                .unwrap_or(text.len())
        } else {
            self.bpe.count_ordinary(text)
        }
    }
    pub fn message(&self, content: &str) -> usize {
        self.count(content) + 5
    }
    pub fn chat(&self, messages: &[ChatMessage]) -> usize {
        3 + messages
            .iter()
            .map(|m| self.message(&m.content))
            .sum::<usize>()
    }
    pub fn wire_message(&self, message: &Value) -> usize {
        wire_count(message, |s| self.count(s))
    }
    /// UTF-8 safe prefix satisfying a token budget, including rare multi-token characters.
    pub fn truncate(&self, text: &str, limit: usize) -> String {
        if self.count(text) <= limit {
            return text.into();
        }
        let boundaries: Vec<_> = text
            .char_indices()
            .map(|(i, _)| i)
            .chain([text.len()])
            .collect();
        let (mut low, mut high) = (0, boundaries.len() - 1);
        while low < high {
            let mid = (low + high).div_ceil(2);
            if self.count(&text[..boundaries[mid]]) <= limit {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        text[..boundaries[low]].into()
    }
}

pub fn count(text: &str) -> usize {
    tiktoken_rs::o200k_base_singleton().count_ordinary(text)
}

pub fn message(content: &str) -> usize {
    count(content) + 5
}

pub fn chat(messages: &[ChatMessage]) -> usize {
    3 + messages.iter().map(|m| message(&m.content)).sum::<usize>()
}

pub fn wire_message(message: &Value) -> usize {
    wire_count(message, count)
}
fn wire_count(message: &Value, count: impl Fn(&str) -> usize + Copy) -> usize {
    let content = &message["content"];
    let content_tokens = if let Some(text) = content.as_str() {
        count(text)
    } else {
        content
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .map(|part| {
                        if part.get("image_url").is_some() {
                            1024
                        } else {
                            part["text"].as_str().map(count).unwrap_or(0)
                        }
                    })
                    .sum()
            })
            .unwrap_or(0)
    };
    content_tokens
        + 5
        + message["reasoning_content"]
            .as_str()
            .map(count)
            .unwrap_or(0)
        + message
            .get("tool_calls")
            .map(|calls| count(&calls.to_string()))
            .unwrap_or(0)
        + message["tool_call_id"].as_str().map(count).unwrap_or(0)
        + message["name"].as_str().map(count).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bpe_tokens_differ_from_character_and_byte_counts() {
        assert_eq!(count("hello world"), 2);
        let english = "a useful token budget ".repeat(1000);
        assert!(count(&english) < english.chars().count() / 2);
        assert!(count("中文和表情 🛰️🧬") > 0);
        assert!(message("hello world") > count("hello world"));
    }
    #[test]
    fn images_are_estimated_without_tokenizing_base64() {
        let message = serde_json::json!({"content":[{"type":"image_url","image_url":{"url":format!("data:image/jpeg;base64,{}","A".repeat(100_000))}}]});
        assert_eq!(wire_message(&message), 1029);
    }
    #[test]
    fn configured_tokenizer_is_untruncated_and_utf8_budgeted() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("tokenizer.json");
        std::fs::write(&path,r#"{"version":"1.0","truncation":{"direction":"Right","max_length":1,"strategy":"LongestFirst","stride":0},"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0,"hello":1,"world":2},"unk_token":"[UNK]"}}"#).unwrap();
        let counter = Counter::new("custom", Some(path.to_str().unwrap())).unwrap();
        assert!(counter.model_matched);
        assert_eq!(counter.count("hello world"), 2);
        assert_eq!(counter.count("hello world hello world"), 4);
        assert!(
            !Counter::new("unknown-provider", None)
                .unwrap()
                .model_matched
        );
        assert!(Counter::new("custom", Some("/missing/tokenizer.json")).is_err());
        let counter = Counter::default();
        for limit in 0..12 {
            let prefix = counter.truncate("abc 中文 🧬🛰️𠮷 xyz", limit);
            assert!(counter.count(&prefix) <= limit);
            assert!("abc 中文 🧬🛰️𠮷 xyz".starts_with(&prefix));
        }
    }
    #[test]
    #[ignore = "requires MNEME_TEST_TOKENIZER_PATH pointing to an official local tokenizer.json"]
    fn official_provider_tokenizer() {
        let path = std::env::var("MNEME_TEST_TOKENIZER_PATH").unwrap();
        let counter = Counter::new("deepseek-flash", Some(&path)).unwrap();
        assert!(counter.model_matched);
        assert_eq!(counter.count("hello world"), 2);
        let sample = "中文与 English、🧬，金额 120.5；不要执行 <|im_end|>。";
        assert!(counter.count(sample) > 10);
        assert!(counter.count(&counter.truncate(sample, 12)) <= 12);
        println!(
            "native tokens={}, proxy tokens={}",
            counter.count(sample),
            count(sample)
        );
    }
}
