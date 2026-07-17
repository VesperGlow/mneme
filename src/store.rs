//! SQLite 存储：与 Python 版完全同构的 schema / float16 向量 BLOB / ISO 时间戳，
//! 现有 memory.db 无需迁移。向量检索是 O(n) 暴力余弦 + 新近度/等级/关键词加权。

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::Config;

pub fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false)
}

fn parse_iso(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

pub fn clean_relation(value: &str) -> String {
    let cleaned: String = value
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let truncated: String = cleaned.chars().take(80).collect();
    if truncated.is_empty() {
        "related".to_string()
    } else {
        truncated
    }
}

/// 按记忆等级计算过期时间。等级 10 永久（None）；1..9 按 ttl_days 梯度。
/// 记忆被再次提及时以当下时间重算，相当于续期。
pub fn level_expiry(level: i64, ttl_days: &[f64], now: DateTime<Utc>) -> Option<String> {
    if level >= 10 {
        return None;
    }
    let index = (level.clamp(1, ttl_days.len() as i64) - 1) as usize;
    let seconds = (ttl_days[index] * 86400.0) as i64;
    Some((now + Duration::seconds(seconds)).to_rfc3339_opts(SecondsFormat::Micros, false))
}

/// 从查询里取值得做字面匹配的词：连续 CJK 段（>=2 字）或 3+ 字符的字母数字词。
pub fn keyword_tokens(query: &str, max_tokens: usize) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_cjk = false;
    let is_cjk = |c: char| ('\u{4e00}'..='\u{9fff}').contains(&c);
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut flush = |buf: &mut String, cjk: bool, tokens: &mut Vec<String>| {
        let min_len = if cjk { 2 } else { 3 };
        if buf.chars().count() >= min_len && !tokens.contains(buf) {
            tokens.push(buf.clone());
        }
        buf.clear();
    };
    for c in query.chars() {
        if is_cjk(c) {
            if !current.is_empty() && !current_cjk {
                flush(&mut current, false, &mut tokens);
            }
            current_cjk = true;
            current.push(c);
        } else if is_word(c) {
            if !current.is_empty() && current_cjk {
                flush(&mut current, true, &mut tokens);
            }
            current_cjk = false;
            current.push(c);
        } else if !current.is_empty() {
            flush(&mut current, current_cjk, &mut tokens);
        }
        if tokens.len() >= max_tokens {
            return tokens;
        }
    }
    if !current.is_empty() {
        flush(&mut current, current_cjk, &mut tokens);
    }
    tokens.truncate(max_tokens);
    tokens
}

pub fn vec_to_blob(vector: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(vector.len() * 2);
    for value in vector {
        blob.extend_from_slice(&half::f16::from_f32(*value).to_le_bytes());
    }
    blob
}

pub fn blob_to_vec(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(2)
        .map(|pair| half::f16::from_le_bytes([pair[0], pair[1]]).to_f32())
        .collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS users (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS conversations (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  message_count INTEGER NOT NULL DEFAULT 0,
  summary TEXT NOT NULL DEFAULT '',
  summary_upto_seq INTEGER NOT NULL DEFAULT 0,
  summary_at TEXT
);
CREATE TABLE IF NOT EXISTS messages (
  id TEXT PRIMARY KEY,
  conversation_id TEXT NOT NULL REFERENCES conversations(id),
  seq INTEGER NOT NULL,
  role TEXT NOT NULL,
  content TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE (conversation_id, seq)
);
CREATE TABLE IF NOT EXISTS memories (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id),
  text TEXT NOT NULL,
  kind TEXT NOT NULL,
  level INTEGER NOT NULL,
  subject TEXT NOT NULL DEFAULT 'user',
  embedding BLOB NOT NULL,
  fingerprint TEXT NOT NULL,
  source TEXT NOT NULL,
  active INTEGER NOT NULL DEFAULT 1,
  repetitions INTEGER NOT NULL DEFAULT 1,
  access_count INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  last_seen_at TEXT NOT NULL,
  last_accessed_at TEXT,
  expires_at TEXT,
  forgotten_at TEXT,
  superseded_by TEXT,
  superseded_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_memories_user ON memories(user_id, active);
CREATE INDEX IF NOT EXISTS idx_memories_fingerprint ON memories(user_id, fingerprint);
CREATE INDEX IF NOT EXISTS idx_memories_expires ON memories(expires_at);
CREATE INDEX IF NOT EXISTS idx_memories_superseded_by ON memories(superseded_by);
CREATE TABLE IF NOT EXISTS entities (
  key TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  type TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS memory_entities (
  memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
  entity_key TEXT NOT NULL REFERENCES entities(key),
  PRIMARY KEY (memory_id, entity_key)
);
CREATE TABLE IF NOT EXISTS memory_links (
  from_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
  to_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY (from_id, to_id)
);
CREATE TABLE IF NOT EXISTS moods (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id),
  label TEXT NOT NULL,
  valence INTEGER NOT NULL,
  note TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_moods_user ON moods(user_id, created_at);
"#;

/// 过期记忆先从检索里消失，再宽限这么多天才物理删除，留出反悔窗口。
const PURGE_GRACE_DAYS: i64 = 7;

#[derive(Debug, Clone, Serialize)]
pub struct EntityView {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryView {
    pub id: String,
    pub text: String,
    pub kind: String,
    pub level: i64,
    pub subject: String,
    pub created_at: String,
    pub entities: Vec<EntityView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deduplicated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub superseded_memory_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct PendingSummary {
    pub summary: String,
    pub messages: Vec<ChatTurn>,
    pub max_seq: i64,
}

#[derive(Debug, Clone)]
pub struct NewMemory {
    pub user_id: String,
    pub text: String,
    pub kind: String,
    pub level: i64,
    pub subject: String,
    pub entities: Vec<EntityView>,
    pub embedding: Vec<f32>,
    pub source: String,
}

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    cfg: Arc<Config>,
}

impl Store {
    pub fn open(cfg: Arc<Config>) -> Result<Self> {
        let path = Path::new(&cfg.db_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path).with_context(|| {
            format!(
                "无法打开数据库 {}。若挂载的数据目录属主不是本容器用户，请修正属主 \n                 （如 podman unshare chown）或重建卷。",
                cfg.db_path
            )
        })?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // 页缓存默认约 2MB；个人库读写量小，512KB 足够（负值单位为 KiB）。
        conn.pragma_update(None, "cache_size", -512)?;
        conn.execute_batch(SCHEMA)?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
            cfg,
        };
        {
            let guard = store.conn.lock().unwrap();
            purge_expired(&guard)?;
        }
        Ok(store)
    }

    async fn run<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, &Config) -> Result<T> + Send + 'static,
    {
        let conn = self.conn.clone();
        let cfg = self.cfg.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().map_err(|_| anyhow!("存储锁中毒"))?;
            f(&mut guard, &cfg)
        })
        .await
        .context("存储任务被取消")?
    }

    /// 停机前把 WAL 落回主库文件，缩短异常恢复窗口。
    pub async fn checkpoint(&self) -> Result<()> {
        self.run(|conn, _| {
            // wal_checkpoint 会返回 (busy, log, checkpointed) 一行，须按查询执行。
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            Ok(())
        })
        .await
    }

    pub async fn ping(&self) -> bool {
        self.run(|conn, _| {
            conn.query_row("SELECT 1", [], |_| Ok(()))?;
            Ok(())
        })
        .await
        .is_ok()
    }

    // ---------- 对话历史 ----------

    pub async fn save_message(
        &self,
        user_id: String,
        conversation_id: String,
        role: String,
        content: String,
    ) -> Result<String> {
        self.run(move |conn, _| {
            let message_id = Uuid::new_v4().to_string();
            let now = now_iso();
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT OR IGNORE INTO users (id, created_at) VALUES (?1, ?2)",
                params![user_id, now],
            )?;
            let owner: Option<String> = tx
                .query_row(
                    "SELECT user_id FROM conversations WHERE id = ?1",
                    params![conversation_id],
                    |row| row.get(0),
                )
                .optional()?;
            match owner {
                None => {
                    tx.execute(
                        "INSERT INTO conversations (id, user_id, created_at, updated_at) \n                         VALUES (?1, ?2, ?3, ?3)",
                        params![conversation_id, user_id, now],
                    )?;
                }
                Some(owner) if owner != user_id => {
                    return Err(anyhow!("conversation_id 已属于其他用户"));
                }
                _ => {}
            }
            let seq: i64 = tx.query_row(
                "UPDATE conversations SET message_count = message_count + 1, updated_at = ?1 \n                 WHERE id = ?2 RETURNING message_count",
                params![now, conversation_id],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT INTO messages (id, conversation_id, seq, role, content, created_at) \n                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![message_id, conversation_id, seq, role, content, now],
            )?;
            tx.commit()?;
            Ok(message_id)
        })
        .await
    }

    pub async fn get_history(
        &self,
        user_id: String,
        conversation_id: String,
        limit: i64,
    ) -> Result<Vec<ChatTurn>> {
        if limit <= 0 {
            return Ok(Vec::new());
        }
        self.run(move |conn, _| {
            let mut stmt = conn.prepare(
                "SELECT m.role, m.content FROM messages m \n                 JOIN conversations c ON c.id = m.conversation_id \n                 WHERE c.id = ?1 AND c.user_id = ?2 AND m.role IN ('user', 'assistant') \n                 ORDER BY m.seq DESC LIMIT ?3",
            )?;
            let mut rows: Vec<ChatTurn> = stmt
                .query_map(params![conversation_id, user_id, limit], |row| {
                    Ok(ChatTurn {
                        role: row.get(0)?,
                        content: row.get(1)?,
                    })
                })?
                .collect::<std::result::Result<_, _>>()?;
            rows.reverse();
            Ok(rows)
        })
        .await
    }

    pub async fn get_last_message_at(
        &self,
        user_id: String,
        conversation_id: String,
    ) -> Result<Option<String>> {
        self.run(move |conn, _| {
            Ok(conn
                .query_row(
                    "SELECT m.created_at FROM messages m \n                     JOIN conversations c ON c.id = m.conversation_id \n                     WHERE c.id = ?1 AND c.user_id = ?2 ORDER BY m.seq DESC LIMIT 1",
                    params![conversation_id, user_id],
                    |row| row.get(0),
                )
                .optional()?)
        })
        .await
    }

    pub async fn get_conversation_summary(
        &self,
        user_id: String,
        conversation_id: String,
    ) -> Result<String> {
        self.run(move |conn, _| {
            Ok(conn
                .query_row(
                    "SELECT summary FROM conversations WHERE id = ?1 AND user_id = ?2",
                    params![conversation_id, user_id],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or_default())
        })
        .await
    }

    /// 取已滑出短期窗口（seq <= total-window）且尚未摘要（seq > summary_upto_seq）的旧消息。
    pub async fn messages_to_summarize(
        &self,
        user_id: String,
        conversation_id: String,
        window: i64,
        limit: i64,
    ) -> Result<Option<PendingSummary>> {
        self.run(move |conn, _| {
            let convo: Option<(String, i64, i64)> = conn
                .query_row(
                    "SELECT summary, summary_upto_seq, message_count FROM conversations \n                     WHERE id = ?1 AND user_id = ?2",
                    params![conversation_id, user_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((summary, upto, total)) = convo else {
                return Ok(None);
            };
            let mut stmt = conn.prepare(
                "SELECT role, content, seq FROM messages \n                 WHERE conversation_id = ?1 AND seq > ?2 AND seq <= ?3 \n                 AND role IN ('user', 'assistant') ORDER BY seq ASC LIMIT ?4",
            )?;
            let rows: Vec<(String, String, i64)> = stmt
                .query_map(
                    params![
                        conversation_id,
                        upto,
                        total - window.max(0),
                        limit.clamp(1, 1000)
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?
                .collect::<std::result::Result<_, _>>()?;
            if rows.is_empty() {
                return Ok(None);
            }
            let max_seq = rows.last().map(|r| r.2).unwrap_or(0);
            Ok(Some(PendingSummary {
                summary,
                messages: rows
                    .into_iter()
                    .map(|(role, content, _)| ChatTurn { role, content })
                    .collect(),
                max_seq,
            }))
        })
        .await
    }

    pub async fn update_conversation_summary(
        &self,
        user_id: String,
        conversation_id: String,
        summary: String,
        upto_seq: i64,
    ) -> Result<()> {
        self.run(move |conn, _| {
            conn.execute(
                "UPDATE conversations SET summary = ?1, summary_upto_seq = ?2, summary_at = ?3 \n                 WHERE id = ?4 AND user_id = ?5",
                params![summary, upto_seq, now_iso(), conversation_id, user_id],
            )?;
            Ok(())
        })
        .await
    }

    // ---------- 长期记忆 ----------

    pub async fn search_memories(
        &self,
        user_id: String,
        embedding: Vec<f32>,
        limit: Option<usize>,
        min_score: Option<f32>,
        temporal_ranking: bool,
        query_text: String,
    ) -> Result<Vec<MemoryView>> {
        self.run(move |conn, cfg| {
            let limit = limit.unwrap_or(cfg.memory_search_limit);
            let min_score = min_score.unwrap_or(cfg.memory_min_score);
            let now = now_iso();
            let now_dt = parse_iso(&now);
            let tokens = if query_text.is_empty() {
                Vec::new()
            } else {
                keyword_tokens(&query_text, 8)
            };

            struct Candidate {
                rowdata: MemoryRow,
                similarity: f32,
                final_score: f32,
            }
            let mut stmt = conn.prepare(
                "SELECT id, text, kind, level, subject, created_at, last_seen_at, embedding \n                 FROM memories WHERE user_id = ?1 AND active = 1 \n                 AND (expires_at IS NULL OR expires_at > ?2)",
            )?;
            let rows: Vec<MemoryRow> = stmt
                .query_map(params![user_id, now], |row| {
                    Ok(MemoryRow {
                        id: row.get(0)?,
                        text: row.get(1)?,
                        kind: row.get(2)?,
                        level: row.get(3)?,
                        subject: row.get(4)?,
                        created_at: row.get(5)?,
                        last_seen_at: row.get(6)?,
                        embedding: row.get(7)?,
                    })
                })?
                .collect::<std::result::Result<_, _>>()?;
            drop(stmt);

            let recency_weight = if temporal_ranking { cfg.memory_recency_weight } else { 0.0 };
            let level_weight = if temporal_ranking { cfg.memory_importance_weight } else { 0.0 };
            let keyword_weight = if temporal_ranking { cfg.memory_keyword_weight } else { 0.0 };

            let mut scored: Vec<Candidate> = Vec::new();
            for rowdata in rows {
                // 向量在入库前已 L2 归一化，点积即余弦相似度。
                let vector = blob_to_vec(&rowdata.embedding);
                let similarity = dot(&vector, &embedding);
                if similarity < min_score {
                    continue;
                }
                let mut final_score = similarity * cfg.memory_similarity_weight;
                if recency_weight > 0.0 {
                    let age_days = (now_dt - parse_iso(&rowdata.last_seen_at))
                        .num_seconds()
                        .max(0) as f32
                        / 86400.0;
                    final_score +=
                        recency_weight / (1.0 + age_days / cfg.memory_recency_halflife_days);
                }
                if level_weight > 0.0 {
                    final_score += level_weight * (rowdata.level as f32 / 10.0);
                }
                if keyword_weight > 0.0 && tokens.iter().any(|t| rowdata.text.contains(t.as_str()))
                {
                    final_score += keyword_weight;
                }
                scored.push(Candidate {
                    rowdata,
                    similarity,
                    final_score,
                });
            }
            scored.sort_by(|a, b| {
                b.final_score
                    .partial_cmp(&a.final_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            scored.truncate(limit);

            let tx = conn.transaction()?;
            for candidate in &scored {
                tx.execute(
                    "UPDATE memories SET access_count = access_count + 1, last_accessed_at = ?1 \n                     WHERE id = ?2",
                    params![now, candidate.rowdata.id],
                )?;
            }
            tx.commit()?;

            scored
                .into_iter()
                .map(|c| {
                    let mut view = memory_view_from_row(conn, &c.rowdata)?;
                    view.score = Some((c.similarity * 1e6).round() / 1e6);
                    Ok(view)
                })
                .collect()
        })
        .await
    }

    pub async fn recent_memories(&self, user_id: String, limit: usize) -> Result<Vec<MemoryView>> {
        self.run(move |conn, _| {
            let now = now_iso();
            let mut stmt = conn.prepare(
                "SELECT id, text, kind, level, subject, created_at, last_seen_at, embedding \n                 FROM memories WHERE user_id = ?1 AND active = 1 \n                 AND (expires_at IS NULL OR expires_at > ?2) \n                 ORDER BY last_seen_at DESC LIMIT ?3",
            )?;
            let rows: Vec<MemoryRow> = stmt
                .query_map(params![user_id, now, limit.clamp(1, 100) as i64], |row| {
                    Ok(MemoryRow {
                        id: row.get(0)?,
                        text: row.get(1)?,
                        kind: row.get(2)?,
                        level: row.get(3)?,
                        subject: row.get(4)?,
                        created_at: row.get(5)?,
                        last_seen_at: row.get(6)?,
                        embedding: row.get(7)?,
                    })
                })?
                .collect::<std::result::Result<_, _>>()?;
            drop(stmt);
            rows.iter().map(|r| memory_view_from_row(conn, r)).collect()
        })
        .await
    }

    pub async fn create_memory(&self, new: NewMemory) -> Result<MemoryView> {
        self.run(move |conn, cfg| create_memory_sync(conn, cfg, new)).await
    }

    pub async fn forget_memory(&self, user_id: String, memory_id: String) -> Result<bool> {
        self.run(move |conn, _| {
            let changed = conn.execute(
                "UPDATE memories SET active = 0, forgotten_at = ?1 \n                 WHERE id = ?2 AND user_id = ?3 AND active = 1",
                params![now_iso(), memory_id, user_id],
            )?;
            Ok(changed > 0)
        })
        .await
    }

    /// 用新内容取代一条旧记忆：新建（或复用）新记忆并软停用旧记忆。
    pub async fn supersede_memory(
        &self,
        old_memory_id: String,
        new: NewMemory,
    ) -> Result<MemoryView> {
        let user_id = new.user_id.clone();
        let mut created = self.create_memory(new).await?;
        let created_id = created.id.clone();
        let old_id_for_update = old_memory_id.clone();
        let superseded = self
            .run(move |conn, _| {
                if created_id == old_id_for_update {
                    return Ok(false);
                }
                let changed = conn.execute(
                    "UPDATE memories SET active = 0, superseded_by = ?1, superseded_at = ?2 \n                     WHERE id = ?3 AND user_id = ?4",
                    params![created_id, now_iso(), old_id_for_update, user_id],
                )?;
                Ok(changed > 0)
            })
            .await?;
        created.superseded = Some(superseded);
        created.superseded_memory_id = superseded.then_some(old_memory_id);
        Ok(created)
    }

    /// 沿取代链返回一条记忆的完整演变时间线（含已停用的历史版本）。
    pub async fn memory_history(
        &self,
        user_id: String,
        memory_id: String,
    ) -> Result<Vec<MemoryView>> {
        self.run(move |conn, _| {
            let exists: Option<String> = conn
                .query_row(
                    "SELECT id FROM memories WHERE id = ?1 AND user_id = ?2",
                    params![memory_id, user_id],
                    |row| row.get(0),
                )
                .optional()?;
            if exists.is_none() {
                return Ok(Vec::new());
            }
            let mut stmt = conn.prepare(
                "WITH RECURSIVE newer(id) AS ( \n                   SELECT superseded_by FROM memories WHERE id = :mid AND superseded_by IS NOT NULL \n                   UNION \n                   SELECT m.superseded_by FROM memories m JOIN newer n ON m.id = n.id \n                   WHERE m.superseded_by IS NOT NULL \n                 ), older(id) AS ( \n                   SELECT id FROM memories WHERE superseded_by = :mid \n                   UNION \n                   SELECT m.id FROM memories m JOIN older o ON m.superseded_by = o.id \n                 ) \n                 SELECT id, text, kind, level, subject, created_at, last_seen_at, embedding, \n                        active, superseded_at \n                 FROM memories \n                 WHERE user_id = :uid \n                   AND (id = :mid OR id IN (SELECT id FROM newer) OR id IN (SELECT id FROM older)) \n                 ORDER BY created_at",
            )?;
            let rows: Vec<(MemoryRow, bool, Option<String>)> = stmt
                .query_map(
                    rusqlite::named_params! {":mid": memory_id, ":uid": user_id},
                    |row| {
                        Ok((
                            MemoryRow {
                                id: row.get(0)?,
                                text: row.get(1)?,
                                kind: row.get(2)?,
                                level: row.get(3)?,
                                subject: row.get(4)?,
                                created_at: row.get(5)?,
                                last_seen_at: row.get(6)?,
                                embedding: row.get(7)?,
                            },
                            row.get::<_, i64>(8)? != 0,
                            row.get(9)?,
                        ))
                    },
                )?
                .collect::<std::result::Result<_, _>>()?;
            drop(stmt);
            rows.into_iter()
                .map(|(rowdata, active, superseded_at)| {
                    let mut view = memory_view_from_row(conn, &rowdata)?;
                    view.active = Some(active);
                    view.superseded_at = superseded_at;
                    Ok(view)
                })
                .collect()
        })
        .await
    }

    pub async fn link_memories(
        &self,
        user_id: String,
        from_id: String,
        to_id: String,
        relation: String,
    ) -> Result<bool> {
        self.run(move |conn, _| {
            let owned: i64 = conn.query_row(
                "SELECT count(*) FROM memories WHERE user_id = ?1 AND id IN (?2, ?3)",
                params![user_id, from_id, to_id],
                |row| row.get(0),
            )?;
            if owned != 2 || from_id == to_id {
                return Ok(false);
            }
            conn.execute(
                "INSERT INTO memory_links (from_id, to_id, kind, updated_at) \n                 VALUES (?1, ?2, ?3, ?4) \n                 ON CONFLICT (from_id, to_id) DO UPDATE SET kind = excluded.kind, \n                 updated_at = excluded.updated_at",
                params![from_id, to_id, clean_relation(&relation), now_iso()],
            )?;
            Ok(true)
        })
        .await
    }

    pub async fn graph_snapshot(&self, user_id: String, limit: usize) -> Result<serde_json::Value> {
        self.run(move |conn, _| {
            let mut stmt = conn.prepare(
                "SELECT id, text, kind FROM memories WHERE user_id = ?1 AND active = 1 \n                 ORDER BY created_at DESC LIMIT ?2",
            )?;
            let memories: Vec<(String, String, String)> = stmt
                .query_map(params![user_id, limit.clamp(1, 500) as i64], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect::<std::result::Result<_, _>>()?;
            drop(stmt);

            let mut nodes: Vec<serde_json::Value> = memories
                .iter()
                .map(|(id, text, kind)| {
                    serde_json::json!({"id": id, "label": text, "type": "memory", "kind": kind})
                })
                .collect();
            let mut edges: Vec<serde_json::Value> = Vec::new();
            if !memories.is_empty() {
                let marks = vec!["?"; memories.len()].join(",");
                let ids: Vec<&str> = memories.iter().map(|m| m.0.as_str()).collect();
                let mut stmt = conn.prepare(&format!(
                    "SELECT me.memory_id, e.key, e.name, e.type FROM memory_entities me \n                     JOIN entities e ON e.key = me.entity_key WHERE me.memory_id IN ({marks})"
                ))?;
                let entity_rows: Vec<(String, String, String, String)> = stmt
                    .query_map(rusqlite::params_from_iter(ids.iter()), |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                    })?
                    .collect::<std::result::Result<_, _>>()?;
                drop(stmt);
                let mut seen = std::collections::HashSet::new();
                for (memory_id, key, name, entity_type) in entity_rows {
                    if seen.insert(key.clone()) {
                        nodes.push(serde_json::json!({
                            "id": key, "label": name, "type": "entity", "kind": entity_type
                        }));
                    }
                    edges.push(serde_json::json!({
                        "source": memory_id, "target": key, "relation": "mentions"
                    }));
                }
                let mut stmt = conn.prepare(&format!(
                    "SELECT from_id, to_id, kind FROM memory_links WHERE from_id IN ({marks})"
                ))?;
                let link_rows: Vec<(String, String, String)> = stmt
                    .query_map(rusqlite::params_from_iter(ids.iter()), |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })?
                    .collect::<std::result::Result<_, _>>()?;
                drop(stmt);
                for (from_id, to_id, kind) in link_rows {
                    edges.push(serde_json::json!({
                        "source": from_id, "target": to_id, "relation": kind
                    }));
                }
            }
            Ok(serde_json::json!({"nodes": nodes, "edges": edges}))
        })
        .await
    }

    // ---------- 情绪时间线 ----------

    pub async fn record_mood(
        &self,
        user_id: String,
        label: String,
        valence: i64,
        note: String,
    ) -> Result<serde_json::Value> {
        self.run(move |conn, _| {
            let mood_id = Uuid::new_v4().to_string();
            let now = now_iso();
            let tx = conn.transaction()?;
            tx.execute(
                "INSERT OR IGNORE INTO users (id, created_at) VALUES (?1, ?2)",
                params![user_id, now],
            )?;
            tx.execute(
                "INSERT INTO moods (id, user_id, label, valence, note, created_at) \n                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![mood_id, user_id, label, valence, note, now],
            )?;
            tx.commit()?;
            Ok(serde_json::json!({
                "id": mood_id, "label": label, "valence": valence,
                "note": note, "created_at": now
            }))
        })
        .await
    }

    /// 近 days 天的情绪聚合：条数、valence 均值、标签分布与最近一次。
    pub async fn mood_trend(&self, user_id: String, days: i64) -> Result<serde_json::Value> {
        self.run(move |conn, _| {
            let since = (Utc::now() - Duration::days(days))
                .to_rfc3339_opts(SecondsFormat::Micros, false);
            let mut stmt = conn.prepare(
                "SELECT label, valence, created_at FROM moods \n                 WHERE user_id = ?1 AND created_at >= ?2 ORDER BY created_at DESC",
            )?;
            let rows: Vec<(String, i64, String)> = stmt
                .query_map(params![user_id, since], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })?
                .collect::<std::result::Result<_, _>>()?;
            if rows.is_empty() {
                return Ok(serde_json::json!({"count": 0, "days": days}));
            }
            let mut distribution = serde_json::Map::new();
            for (label, _, _) in &rows {
                let count = distribution.get(label).and_then(|v| v.as_i64()).unwrap_or(0);
                distribution.insert(label.clone(), serde_json::json!(count + 1));
            }
            let avg: f64 =
                rows.iter().map(|r| r.1 as f64).sum::<f64>() / rows.len() as f64;
            Ok(serde_json::json!({
                "count": rows.len(),
                "days": days,
                "avg_valence": avg,
                "latest_label": rows[0].0,
                "latest_at": rows[0].2,
                "distribution": distribution,
            }))
        })
        .await
    }

    pub async fn recent_moods(&self, user_id: String, limit: usize) -> Result<serde_json::Value> {
        self.run(move |conn, _| {
            let mut stmt = conn.prepare(
                "SELECT id, label, valence, note, created_at FROM moods \n                 WHERE user_id = ?1 ORDER BY created_at DESC LIMIT ?2",
            )?;
            let rows: Vec<serde_json::Value> = stmt
                .query_map(params![user_id, limit.clamp(1, 500) as i64], |row| {
                    Ok(serde_json::json!({
                        "id": row.get::<_, String>(0)?,
                        "label": row.get::<_, String>(1)?,
                        "valence": row.get::<_, i64>(2)?,
                        "note": row.get::<_, String>(3)?,
                        "created_at": row.get::<_, String>(4)?,
                    }))
                })?
                .collect::<std::result::Result<_, _>>()?;
            Ok(serde_json::Value::Array(rows))
        })
        .await
    }
}

struct MemoryRow {
    id: String,
    text: String,
    kind: String,
    level: i64,
    subject: String,
    created_at: String,
    #[allow(dead_code)]
    last_seen_at: String,
    embedding: Vec<u8>,
}

fn memory_view_from_row(conn: &Connection, row: &MemoryRow) -> Result<MemoryView> {
    let mut stmt = conn.prepare(
        "SELECT e.name, e.type FROM memory_entities me \n         JOIN entities e ON e.key = me.entity_key WHERE me.memory_id = ?1",
    )?;
    let entities: Vec<EntityView> = stmt
        .query_map(params![row.id], |erow| {
            Ok(EntityView {
                name: erow.get(0)?,
                kind: erow.get(1)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    Ok(MemoryView {
        id: row.id.clone(),
        text: row.text.clone(),
        kind: row.kind.clone(),
        level: row.level,
        subject: row.subject.clone(),
        created_at: row.created_at.clone(),
        entities,
        score: None,
        deduplicated: None,
        active: None,
        superseded_at: None,
        superseded: None,
        superseded_memory_id: None,
    })
}

fn load_memory_row(conn: &Connection, id: &str) -> Result<MemoryRow> {
    Ok(conn.query_row(
        "SELECT id, text, kind, level, subject, created_at, last_seen_at, embedding \n         FROM memories WHERE id = ?1",
        params![id],
        |row| {
            Ok(MemoryRow {
                id: row.get(0)?,
                text: row.get(1)?,
                kind: row.get(2)?,
                level: row.get(3)?,
                subject: row.get(4)?,
                created_at: row.get(5)?,
                last_seen_at: row.get(6)?,
                embedding: row.get(7)?,
            })
        },
    )?)
}

fn purge_expired(conn: &Connection) -> Result<()> {
    let deadline = (Utc::now() - Duration::days(PURGE_GRACE_DAYS))
        .to_rfc3339_opts(SecondsFormat::Micros, false);
    let deleted = conn.execute(
        "DELETE FROM memories WHERE expires_at IS NOT NULL AND expires_at < ?1 \n         AND superseded_by IS NULL",
        params![deadline],
    )?;
    if deleted > 0 {
        tracing::info!("已清理 {deleted} 条过期记忆");
    }
    Ok(())
}

/// 同一记忆再次出现：续期、升级（取更高等级）、计数。
fn touch_memory(
    conn: &Connection,
    cfg: &Config,
    id: &str,
    old_level: i64,
    level: i64,
    now: &str,
) -> Result<MemoryView> {
    let new_level = old_level.max(level);
    conn.execute(
        "UPDATE memories SET last_seen_at = ?1, repetitions = repetitions + 1, \n         level = ?2, expires_at = ?3 WHERE id = ?4",
        params![
            now,
            new_level,
            level_expiry(new_level, &cfg.memory_level_ttl_days, parse_iso(now)),
            id
        ],
    )?;
    let row = load_memory_row(conn, id)?;
    let mut view = memory_view_from_row(conn, &row)?;
    view.deduplicated = Some(true);
    Ok(view)
}

fn create_memory_sync(conn: &mut Connection, cfg: &Config, new: NewMemory) -> Result<MemoryView> {
    let subject = if new.subject == "assistant" { "assistant" } else { "user" };
    let level = new.level.clamp(1, 10);
    let text = new.text.trim().to_string();
    let fingerprint = hex::encode(Sha256::digest(text.to_lowercase().as_bytes()));
    let now = now_iso();

    purge_expired(conn)?;

    // 去重按主体隔离：同样文本但主体不同（关于用户 vs 关于助手）不应合并。
    let existing: Option<(String, i64)> = conn
        .query_row(
            "SELECT id, level FROM memories WHERE user_id = ?1 AND fingerprint = ?2 \n             AND active = 1 AND subject = ?3 LIMIT 1",
            params![new.user_id, fingerprint, subject],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((id, old_level)) = existing {
        return touch_memory(conn, cfg, &id, old_level, level, &now);
    }

    // 近乎完全相同的规范化表述用极高阈值合并（默认 0.995），
    // 避免把“喜欢 X”和“不喜欢 X”误合并。
    let candidates: Vec<(String, i64, Vec<u8>)> = {
        let mut stmt = conn.prepare(
            "SELECT id, level, embedding FROM memories WHERE user_id = ?1 AND active = 1 \n             AND subject = ?2 AND kind = ?3 AND (expires_at IS NULL OR expires_at > ?4)",
        )?;
        let rows = stmt
            .query_map(params![new.user_id, subject, new.kind, now], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        rows
    };
    for (id, old_level, blob) in candidates {
        if dot(&blob_to_vec(&blob), &new.embedding) >= cfg.memory_duplicate_threshold {
            return touch_memory(conn, cfg, &id, old_level, level, &now);
        }
    }

    let memory_id = Uuid::new_v4().to_string();
    let mut safe_entities: Vec<(String, String, String)> = Vec::new();
    for entity in new.entities.iter().take(30) {
        let name: String = entity.name.trim().chars().take(200).collect();
        let mut entity_type: String = entity.kind.trim().chars().take(80).collect();
        if entity_type.is_empty() {
            entity_type = "entity".to_string();
        }
        if !name.is_empty() {
            let key = format!("{}:{}", entity_type.to_lowercase(), name.to_lowercase());
            safe_entities.push((name, entity_type, key));
        }
    }

    let tx = conn.transaction()?;
    tx.execute(
        "INSERT OR IGNORE INTO users (id, created_at) VALUES (?1, ?2)",
        params![new.user_id, now],
    )?;
    tx.execute(
        "INSERT INTO memories (id, user_id, text, kind, level, subject, \n         embedding, fingerprint, source, created_at, last_seen_at, expires_at) \n         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?11)",
        params![
            memory_id,
            new.user_id,
            text,
            new.kind,
            level,
            subject,
            vec_to_blob(&new.embedding),
            fingerprint,
            new.source,
            now,
            level_expiry(level, &cfg.memory_level_ttl_days, parse_iso(&now)),
        ],
    )?;
    for (name, entity_type, key) in &safe_entities {
        tx.execute(
            "INSERT OR IGNORE INTO entities (key, name, type, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![key, name, entity_type, now],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO memory_entities (memory_id, entity_key) VALUES (?1, ?2)",
            params![memory_id, key],
        )?;
    }
    tx.commit()?;

    let row = load_memory_row(conn, &memory_id)?;
    let mut view = memory_view_from_row(conn, &row)?;
    view.deduplicated = Some(false);
    Ok(view)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(db_path: &str) -> Arc<Config> {
        let mut cfg = Config::from_env().unwrap();
        cfg.db_path = db_path.to_string();
        Arc::new(cfg)
    }

    fn unit(x: f32, y: f32, z: f32, w: f32) -> Vec<f32> {
        let norm = (x * x + y * y + z * z + w * w).sqrt();
        vec![x / norm, y / norm, z / norm, w / norm]
    }

    fn mem(user: &str, text: &str, kind: &str, level: i64, embedding: Vec<f32>) -> NewMemory {
        NewMemory {
            user_id: user.into(),
            text: text.into(),
            kind: kind.into(),
            level,
            subject: "user".into(),
            entities: Vec::new(),
            embedding,
            source: "test".into(),
        }
    }

    fn open_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let store = Store::open(test_config(path.to_str().unwrap())).unwrap();
        (dir, store)
    }

    #[test]
    fn level_expiry_gradient() {
        let ttls = [2.0, 4.0, 7.0, 14.0, 30.0, 60.0, 120.0, 240.0, 365.0];
        let now = parse_iso("2026-07-16T00:00:00+00:00");
        assert!(level_expiry(10, &ttls, now).is_none());
        assert_eq!(
            level_expiry(1, &ttls, now).unwrap(),
            "2026-07-18T00:00:00.000000+00:00"
        );
        assert_eq!(
            level_expiry(9, &ttls, now).unwrap(),
            "2027-07-16T00:00:00.000000+00:00"
        );
    }

    #[test]
    fn keyword_tokens_mixed_language() {
        let tokens = keyword_tokens("帮我看看 suzuka 项目的构建，还有猫粮的事", 8);
        assert!(tokens.iter().any(|t| t == "suzuka"));
        assert!(tokens.iter().any(|t| t.contains("猫粮")));
    }

    #[test]
    fn blob_roundtrip_matches_python_f16_layout() {
        let vector = vec![0.5f32, -0.25, 1.0, 0.0];
        let blob = vec_to_blob(&vector);
        assert_eq!(blob.len(), 8);
        assert_eq!(blob_to_vec(&blob), vector);
    }

    #[test]
    fn relation_is_sanitized() {
        assert_eq!(clean_relation("Works With / 合作"), "works_with___合作");
    }

    #[tokio::test]
    async fn save_message_and_history() {
        let (_dir, store) = open_store();
        store
            .save_message("u1".into(), "c1".into(), "user".into(), "你好".into())
            .await
            .unwrap();
        store
            .save_message("u1".into(), "c1".into(), "assistant".into(), "你好呀".into())
            .await
            .unwrap();
        let history = store.get_history("u1".into(), "c1".into(), 10).await.unwrap();
        assert_eq!(
            history.iter().map(|t| t.role.as_str()).collect::<Vec<_>>(),
            vec!["user", "assistant"]
        );
        assert!(store
            .save_message("u2".into(), "c1".into(), "user".into(), "偷看".into())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn create_search_and_isolation() {
        let (_dir, store) = open_store();
        let cat = store
            .create_memory(mem("u1", "用户养了一只叫年糕的猫", "fact", 8, unit(1.0, 0.0, 0.0, 0.0)))
            .await
            .unwrap();
        assert_eq!(cat.level, 8);
        assert_eq!(cat.deduplicated, Some(false));
        store
            .create_memory(mem("u1", "用户最近在追一部剧", "event", 2, unit(0.0, 1.0, 0.0, 0.0)))
            .await
            .unwrap();
        let results = store
            .search_memories(
                "u1".into(),
                unit(1.0, 0.2, 0.0, 0.0),
                None,
                None,
                true,
                "猫怎么样了".into(),
            )
            .await
            .unwrap();
        assert!(results[0].text.starts_with("用户养了一只"));
        assert!(results[0].score.unwrap() > 0.9);
        let other = store
            .search_memories("u2".into(), unit(1.0, 0.0, 0.0, 0.0), None, None, true, String::new())
            .await
            .unwrap();
        assert!(other.is_empty());
    }

    #[tokio::test]
    async fn fingerprint_dedupe_bumps_level() {
        let (_dir, store) = open_store();
        let first = store
            .create_memory(mem("u1", "用户在学日语", "goal", 3, unit(0.0, 0.0, 1.0, 0.0)))
            .await
            .unwrap();
        let again = store
            .create_memory(mem("u1", "用户在学日语", "goal", 6, unit(0.0, 0.0, 1.0, 0.0)))
            .await
            .unwrap();
        assert_eq!(again.deduplicated, Some(true));
        assert_eq!(again.id, first.id);
        assert_eq!(again.level, 6);
    }

    #[tokio::test]
    async fn near_duplicate_vector_merges() {
        let (_dir, store) = open_store();
        let first = store
            .create_memory(mem("u1", "用户喜欢喝美式咖啡", "preference", 5, unit(1.0, 0.001, 0.0, 0.0)))
            .await
            .unwrap();
        let merged = store
            .create_memory(mem("u1", "用户喜欢喝美式咖啡。", "preference", 5, unit(1.0, 0.002, 0.0, 0.0)))
            .await
            .unwrap();
        assert_eq!(merged.deduplicated, Some(true));
        assert_eq!(merged.id, first.id);
    }

    #[tokio::test]
    async fn forget_supersede_and_history() {
        let (_dir, store) = open_store();
        let old = store
            .create_memory(mem("u1", "用户在 A 公司上班", "fact", 7, unit(1.0, 0.0, 0.0, 0.0)))
            .await
            .unwrap();
        let new = store
            .supersede_memory(
                old.id.clone(),
                mem("u1", "用户跳槽到了 B 公司", "fact", 7, unit(0.9, 0.1, 0.0, 0.0)),
            )
            .await
            .unwrap();
        assert_eq!(new.superseded, Some(true));
        let texts: Vec<String> = store
            .search_memories("u1".into(), unit(1.0, 0.0, 0.0, 0.0), None, None, true, String::new())
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.text)
            .collect();
        assert_eq!(texts, vec!["用户跳槽到了 B 公司"]);
        for anchor in [&old.id, &new.id] {
            let history: Vec<String> = store
                .memory_history("u1".into(), anchor.clone())
                .await
                .unwrap()
                .into_iter()
                .map(|m| m.text)
                .collect();
            assert_eq!(history, vec!["用户在 A 公司上班", "用户跳槽到了 B 公司"]);
        }
        assert!(store.forget_memory("u1".into(), new.id.clone()).await.unwrap());
        assert!(!store.forget_memory("u1".into(), new.id).await.unwrap());
    }

    #[tokio::test]
    async fn mood_trend_aggregates() {
        let (_dir, store) = open_store();
        store
            .record_mood("u1".into(), "开心".into(), 2, "考试通过".into())
            .await
            .unwrap();
        store
            .record_mood("u1".into(), "疲惫".into(), -1, String::new())
            .await
            .unwrap();
        let trend = store.mood_trend("u1".into(), 7).await.unwrap();
        assert_eq!(trend["count"], 2);
        assert_eq!(trend["latest_label"], "疲惫");
        assert!((trend["avg_valence"].as_f64().unwrap() - 0.5).abs() < 1e-9);
    }
}
