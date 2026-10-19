use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::FromRow;

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn normalized(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace() && !"，。！？、,.!?；;：:‘’“”\"'".contains(*c))
        .flat_map(char::to_lowercase)
        .collect()
}
pub fn content_hash(text: &str) -> String {
    // Keep internal punctuation: 60.5 and 605, or A/B and AB, are different facts.
    let canonical: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    let canonical = canonical.trim_end_matches(['。', '！', '？', '!', '?', '.', '；', ';']);
    format!("{:x}", Sha256::digest(canonical.as_bytes()))
}
pub fn truncate(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub turn_id: String,
    pub role: String,
    pub content: String,
    pub created_at: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Memory {
    pub id: String,
    pub slot: String,
    pub content: String,
    pub content_hash: String,
    pub kind: String,
    pub importance: f64,
    pub confidence: f64,
    pub entities: String,
    #[serde(skip_serializing)]
    pub embedding: String,
    pub status: String,
    pub superseded_by: Option<String>,
    pub revision: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub source_seq: i64,
    pub last_accessed_at: Option<i64>,
    pub access_count: i64,
    pub valid_until: Option<i64>,
    pub origin: String,
    pub deleted_at: Option<i64>,
    pub indexed_revision: i64,
}
impl Memory {
    pub fn strength(&self, at: i64, half_life_days: f64) -> f64 {
        let elapsed = (at - self.updated_at).max(0) as f64 / 86400.0;
        let half_life = half_life_days * (0.5 + self.importance * 2.0);
        self.importance * self.confidence * 2f64.powf(-elapsed / half_life)
    }
    pub fn available(&self, at: i64) -> bool {
        self.deleted_at.is_none()
            && self.status == "active"
            && self.valid_until.is_none_or(|end| end > at)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub key: String,
    pub content: String,
    pub kind: String,
    pub importance: f64,
    pub confidence: f64,
    #[serde(default)]
    pub entities: Vec<String>,
    pub source_quote: String,
    pub valid_until: Option<String>,
    #[serde(default)]
    pub sensitive: bool,
    /// Only accepted for an existing memory in the extraction context with the same slot.
    pub equivalent_to: Option<String>,
    /// Explicit same subject/property identity, independently of its current value.
    #[serde(default)]
    pub attribute_of: Option<String>,
    /// Topic/property identity alone does not establish a contradiction.
    #[serde(default)]
    pub relation: FactRelation,
    #[serde(default)]
    pub supersedes: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FactRelation {
    #[default]
    Replace,
    Confirm,
    Coexist,
}
impl Candidate {
    pub fn storage_key(&self) -> String {
        if self.relation != FactRelation::Coexist {
            return self.key.clone();
        }
        let suffix = format!(".assertion_{}", &content_hash(&self.content)[..12]);
        let base = self.key.split(".assertion_").next().unwrap();
        format!("{}{}", &base[..base.len().min(100 - suffix.len())], suffix)
    }
}

#[derive(Debug, Clone, FromRow)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub payload: String,
    pub attempts: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}
impl From<Message> for ChatMessage {
    fn from(message: Message) -> Self {
        Self {
            role: message.role,
            content: message.content,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Recall {
    pub memory: Memory,
    pub score: f64,
    pub channels: Vec<String>,
}
