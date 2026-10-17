//! Local token estimates for OpenAI-compatible providers. The provider may use another
//! tokenizer or image accounting; expose that distinction in the context API.
use crate::domain::ChatMessage;
use serde_json::Value;

pub const TOOL_RESERVE: usize = 12_000;
pub const TOKENIZER: &str = "o200k_base";

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
}
