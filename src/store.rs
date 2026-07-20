//! SQLite 存储：关系数据（对话/记忆/实体/情绪）在普通表，向量存进 sqlite-vec 的 vec0
//! 虚拟表做余弦 KNN；rowid 对应 memories 行。旧库首次启动会把 f16 BLOB 向量迁进 vec0。
//! 追加式（append-only）：记忆只新增，不因时间过期；软删除/取代仍保留可审计留痕。
//! 检索一段 = vec0 KNN 召回候选，二段精排（rerank）在 agent 层完成。

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{Duration, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::Config;

pub fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false)
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

// f16 编码只剩迁移的单测在用（校验 blob_to_vec 依赖的 f16 布局）；保留但允许未使用。
#[allow(dead_code)]
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

/// vec0 需要原始 float32 小端字节；检索与入库都用它把 `Vec<f32>` 变成可绑定的 BLOB。
fn vec_to_blob_f32(vector: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(vector.len() * 4);
    for value in vector {
        blob.extend_from_slice(&value.to_le_bytes());
    }
    blob
}

/// 进程内注册 sqlite-vec（vec0 虚拟表）。auto_extension 只对注册之后新开的连接生效，
/// 故须在任何 `Connection::open` 之前调用；`Once` 保证只注册一次。
fn register_vec_extension() {
    static VEC_INIT: Once = Once::new();
    VEC_INIT.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

/// 建 vec0 向量表（幂等）。`embedding` 是向量列，`user_id`/`subject`/`kind` 是可在 KNN
/// 里过滤的元数据列；rowid 对应 `memories` 的隐式 rowid，距离用余弦（1 − 余弦相似度）。
fn ensure_vec_table(conn: &Connection, dim: usize) -> Result<()> {
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE IF NOT EXISTS vec_memories USING vec0(\n           embedding float[{dim}] distance_metric=cosine,\n           user_id text,\n           subject text,\n           kind text\n         );"
    ))?;
    Ok(())
}

/// 旧库迁移：把 `memories.embedding`（f16 BLOB）里活跃记忆的向量回填进 vec0，成功后移除
/// `embedding` 列（向量从此只存 vec0 一份）。幂等：每次先清空 vec0 再回填；无该列则跳过。
fn migrate_embeddings_to_vec0(conn: &mut Connection, dim: usize) -> Result<()> {
    let has_col: bool = conn.query_row(
        "SELECT count(*) FROM pragma_table_info('memories') WHERE name = 'embedding'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;
    if !has_col {
        return Ok(());
    }
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM vec_memories", [])?;
    let mut stmt = tx.prepare(
        "SELECT rowid, embedding, user_id, subject, kind FROM memories WHERE active = 1",
    )?;
    let rows: Vec<(i64, Vec<u8>, String, String, String)> = stmt
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
        })?
        .collect::<std::result::Result<_, _>>()?;
    drop(stmt);
    let mut migrated = 0usize;
    for (rowid, blob, user_id, subject, kind) in rows {
        let vector = blob_to_vec(&blob);
        if vector.len() != dim {
            tracing::warn!("跳过维度不符的旧向量 rowid={rowid}（{} != {dim}）", vector.len());
            continue;
        }
        tx.execute(
            "INSERT INTO vec_memories(rowid, embedding, user_id, subject, kind) \n             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![rowid, vec_to_blob_f32(&vector), user_id, subject, kind],
        )?;
        migrated += 1;
    }
    tx.commit()?;
    // 回填提交成功后再删列（不可逆）；中途失败则下次启动重跑（先清空再回填，幂等）。
    conn.execute("ALTER TABLE memories DROP COLUMN embedding", [])?;
    tracing::info!("向量已迁移到 vec0：{migrated} 条，memories.embedding 列已移除");
    Ok(())
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

/// SQLite 连接池大小。WAL 下多连接可并发读、写自动串行，个人规模 4 条足矣。
const DB_POOL_SIZE: usize = 4;

/// memories 表拼出 [`MemoryRow`] 所需的列，顺序与 [`MemoryRow::from_row`] 对应。
const MEMORY_COLUMNS: &str = "id, text, kind, level, subject, created_at, last_seen_at";

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
    // 一把全局 Mutex 会让检索（O(n) 暴力扫描）与后台落库互相排队；改用小连接池，
    // 靠 WAL 的多读单写 + busy_timeout 让并发操作尽量并行。
    pool: Arc<Vec<Mutex<Connection>>>,
    next: Arc<AtomicUsize>,
    cfg: Arc<Config>,
}

impl Store {
    pub fn open(cfg: Arc<Config>) -> Result<Self> {
        let path = Path::new(&cfg.db_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        // vec0 扩展须在开连接前注册（对之后新开的每条连接生效）。
        register_vec_extension();
        let mut pool = Vec::with_capacity(DB_POOL_SIZE);
        for index in 0..DB_POOL_SIZE {
            let mut conn = Connection::open(path).with_context(|| {
                format!(
                    "无法打开数据库 {}。若挂载的数据目录属主不是本容器用户，请修正属主 \n                     （如 podman unshare chown）或重建卷。",
                    cfg.db_path
                )
            })?;
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
            conn.pragma_update(None, "foreign_keys", "ON")?;
            // 写并发下等锁而不是立刻返回 SQLITE_BUSY（毫秒）。
            conn.pragma_update(None, "busy_timeout", 5000)?;
            // 页缓存默认约 2MB；个人库读写量小，每连接 512KB 足够（负值单位为 KiB）。
            conn.pragma_update(None, "cache_size", -512)?;
            // schema 建表 + vec0 表 + 旧库向量迁移只需在一条连接上做一次（均幂等）。
            // 追加式存储：不再有开机清理过期记忆这一步。
            if index == 0 {
                conn.execute_batch(SCHEMA)?;
                ensure_vec_table(&conn, cfg.embedding_dimensions)?;
                migrate_embeddings_to_vec0(&mut conn, cfg.embedding_dimensions)?;
            }
            pool.push(Mutex::new(conn));
        }
        Ok(Self {
            pool: Arc::new(pool),
            next: Arc::new(AtomicUsize::new(0)),
            cfg,
        })
    }

    async fn run<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection, &Config) -> Result<T> + Send + 'static,
    {
        let pool = self.pool.clone();
        let cfg = self.cfg.clone();
        // 轮询选一条连接；WAL 下并发操作落到不同连接上即可并行。
        let index = self.next.fetch_add(1, Ordering::Relaxed) % pool.len();
        tokio::task::spawn_blocking(move || {
            let mut guard = pool[index].lock().map_err(|_| anyhow!("存储锁中毒"))?;
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

    /// 一段召回：对该用户全部活跃记忆做暴力余弦，按相似度取 top-`limit` 候选。
    /// 不再叠加新近度/等级/关键词加权——最终排序质量交给 agent 层的二段重排（rerank）。
    /// `limit` 是候选宽度：启用重排时通常传 `rerank_candidates`，未启用时传最终条数。
    pub async fn search_memories(
        &self,
        user_id: String,
        embedding: Vec<f32>,
        limit: Option<usize>,
        min_score: Option<f32>,
    ) -> Result<Vec<MemoryView>> {
        self.run(move |conn, cfg| {
            let limit = limit.unwrap_or(cfg.memory_search_limit);
            let min_score = min_score.unwrap_or(cfg.memory_min_score);
            let now = now_iso();
            let query = vec_to_blob_f32(&embedding);

            // 一段召回 = vec0 KNN：按 user_id 过滤的余弦最近邻（vec0 只存活跃记忆）。
            // distance = 1 − 余弦相似度；相似度低于 min_score 的丢弃，最终排序交给二段 rerank。
            let mut stmt = conn.prepare(&format!(
                "SELECT {MEMORY_COLUMNS}, knn.distance FROM ( \n                   SELECT rowid, distance FROM vec_memories \n                   WHERE embedding MATCH ?1 AND user_id = ?2 AND k = ?3 \n                 ) knn \n                 JOIN memories m ON m.rowid = knn.rowid \n                 WHERE m.active = 1 \n                 ORDER BY knn.distance"
            ))?;
            let scored: Vec<(MemoryRow, f32)> = stmt
                .query_map(params![query, user_id, limit as i64], |row| {
                    let similarity = (1.0 - row.get::<_, f64>(7)?) as f32;
                    Ok((MemoryRow::from_row(row)?, similarity))
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|(_, similarity)| *similarity >= min_score)
                .collect();
            drop(stmt);

            let tx = conn.transaction()?;
            for (rowdata, _) in &scored {
                tx.execute(
                    "UPDATE memories SET access_count = access_count + 1, last_accessed_at = ?1 \n                     WHERE id = ?2",
                    params![now, rowdata.id],
                )?;
            }
            tx.commit()?;

            scored
                .into_iter()
                .map(|(rowdata, similarity)| {
                    let mut view = memory_view_from_row(conn, &rowdata)?;
                    view.score = Some((similarity * 1e6).round() / 1e6);
                    Ok(view)
                })
                .collect()
        })
        .await
    }

    pub async fn recent_memories(&self, user_id: String, limit: usize) -> Result<Vec<MemoryView>> {
        self.run(move |conn, _| {
            let now = now_iso();
            let mut stmt = conn.prepare(&format!(
                "SELECT {MEMORY_COLUMNS} FROM memories WHERE user_id = ?1 AND active = 1 \n                 AND (expires_at IS NULL OR expires_at > ?2) \n                 ORDER BY last_seen_at DESC LIMIT ?3"
            ))?;
            let rows: Vec<MemoryRow> = stmt
                .query_map(
                    params![user_id, now, limit.clamp(1, 100) as i64],
                    MemoryRow::from_row,
                )?
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
            let tx = conn.transaction()?;
            let changed = tx.execute(
                "UPDATE memories SET active = 0, forgotten_at = ?1 \n                 WHERE id = ?2 AND user_id = ?3 AND active = 1",
                params![now_iso(), memory_id, user_id],
            )?;
            if changed > 0 {
                // 软删的记忆从向量索引移除（memories 行仍在，rowid 可查）。
                tx.execute(
                    "DELETE FROM vec_memories WHERE rowid = \n                     (SELECT rowid FROM memories WHERE id = ?1)",
                    params![memory_id],
                )?;
            }
            tx.commit()?;
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
                let tx = conn.transaction()?;
                let changed = tx.execute(
                    "UPDATE memories SET active = 0, superseded_by = ?1, superseded_at = ?2 \n                     WHERE id = ?3 AND user_id = ?4",
                    params![created_id, now_iso(), old_id_for_update, user_id],
                )?;
                if changed > 0 {
                    // 被取代的旧记忆从向量索引移除（memories 行保留可回溯）。
                    tx.execute(
                        "DELETE FROM vec_memories WHERE rowid = \n                         (SELECT rowid FROM memories WHERE id = ?1)",
                        params![old_id_for_update],
                    )?;
                }
                tx.commit()?;
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
            let mut stmt = conn.prepare(&format!(
                "WITH RECURSIVE newer(id) AS ( \n                   SELECT superseded_by FROM memories WHERE id = :mid AND superseded_by IS NOT NULL \n                   UNION \n                   SELECT m.superseded_by FROM memories m JOIN newer n ON m.id = n.id \n                   WHERE m.superseded_by IS NOT NULL \n                 ), older(id) AS ( \n                   SELECT id FROM memories WHERE superseded_by = :mid \n                   UNION \n                   SELECT m.id FROM memories m JOIN older o ON m.superseded_by = o.id \n                 ) \n                 SELECT {MEMORY_COLUMNS}, active, superseded_at \n                 FROM memories \n                 WHERE user_id = :uid \n                   AND (id = :mid OR id IN (SELECT id FROM newer) OR id IN (SELECT id FROM older)) \n                 ORDER BY created_at"
            ))?;
            let rows: Vec<(MemoryRow, bool, Option<String>)> = stmt
                .query_map(
                    rusqlite::named_params! {":mid": memory_id, ":uid": user_id},
                    |row| {
                        Ok((
                            MemoryRow::from_row(row)?,
                            row.get::<_, i64>(7)? != 0,
                            row.get(8)?,
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
}

impl MemoryRow {
    /// 从以 [`MEMORY_COLUMNS`] 顺序开头的行取出各列（后续列由调用方另取）。
    fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(MemoryRow {
            id: row.get(0)?,
            text: row.get(1)?,
            kind: row.get(2)?,
            level: row.get(3)?,
            subject: row.get(4)?,
            created_at: row.get(5)?,
            last_seen_at: row.get(6)?,
        })
    }
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
        &format!("SELECT {MEMORY_COLUMNS} FROM memories WHERE id = ?1"),
        params![id],
        MemoryRow::from_row,
    )?)
}

/// 同一记忆再次出现：更新最近提及时间、取更高的重要度等级、计数。
/// 追加式下不再有过期续期这回事，只维护 last_seen_at / repetitions / level。
fn touch_memory(
    conn: &Connection,
    _cfg: &Config,
    id: &str,
    old_level: i64,
    level: i64,
    now: &str,
) -> Result<MemoryView> {
    let new_level = old_level.max(level);
    conn.execute(
        "UPDATE memories SET last_seen_at = ?1, repetitions = repetitions + 1, \n         level = ?2 WHERE id = ?3",
        params![now, new_level, id],
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

    // 近乎完全相同的表述用极高阈值合并（默认 0.995）：在 vec0 里查同 user/subject/kind
    // 的最近一条，余弦 ≥ 阈值即视为重复、并入旧记忆（避免把“喜欢 X”和“不喜欢 X”误并）。
    let near: Option<(i64, f64)> = conn
        .query_row(
            "SELECT rowid, distance FROM vec_memories \n             WHERE embedding MATCH ?1 AND user_id = ?2 AND subject = ?3 AND kind = ?4 AND k = 1",
            params![vec_to_blob_f32(&new.embedding), new.user_id, subject, new.kind],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((rowid, distance)) = near {
        if 1.0 - distance >= cfg.memory_duplicate_threshold as f64 {
            let (id, old_level): (String, i64) = conn.query_row(
                "SELECT id, level FROM memories WHERE rowid = ?1",
                params![rowid],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
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
    // 追加式：不写 expires_at（列保留兼容旧库，新记忆恒为 NULL = 永不过期）。
    tx.execute(
        "INSERT INTO memories (id, user_id, text, kind, level, subject, \n         fingerprint, source, created_at, last_seen_at) \n         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)",
        params![
            memory_id,
            new.user_id,
            text,
            new.kind,
            level,
            subject,
            fingerprint,
            new.source,
            now,
        ],
    )?;
    // 向量只存 vec0 一份；rowid 对应刚插入的 memories 行。
    tx.execute(
        "INSERT INTO vec_memories(rowid, embedding, user_id, subject, kind) \n         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            tx.last_insert_rowid(),
            vec_to_blob_f32(&new.embedding),
            new.user_id,
            subject,
            new.kind,
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

/// CLI `memory list` 的一行。刻意不含 embedding BLOB，避免打印二进制。
#[derive(Debug, Serialize)]
pub struct MemoryListRow {
    pub id: String,
    pub user_id: String,
    pub created_at: String,
    pub last_seen_at: String,
    pub kind: String,
    pub level: i64,
    pub repetitions: i64,
    pub active: bool,
    pub expires_at: Option<String>,
    pub text: String,
}

/// `memory list` 的过滤条件。
pub struct ListFilter {
    /// 只看某个 user_id；None = 全部用户。
    pub user_id: Option<String>,
    /// 是否包含已失效（被遗忘/被取代）的记忆。
    pub include_inactive: bool,
    pub limit: usize,
}

/// CLI 用：直接查记忆，不建表、不清理、不预热。用 query_only 防写，与运行中的
/// 服务共享同一 WAL 数据库（同机多进程读安全）。
pub fn cli_list_memories(cfg: &Config, filter: &ListFilter) -> Result<Vec<MemoryListRow>> {
    let conn = Connection::open(&cfg.db_path)
        .with_context(|| format!("无法打开数据库 {}", cfg.db_path))?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    // 防御性只读：本连接拒绝任何写入，但仍能正常读 WAL。
    conn.pragma_update(None, "query_only", true)?;

    let mut sql = String::from(
        "SELECT id, user_id, created_at, last_seen_at, kind, level, repetitions, active, expires_at, text \
         FROM memories",
    );
    let mut clauses: Vec<&str> = Vec::new();
    if !filter.include_inactive {
        clauses.push("active = 1");
    }
    if filter.user_id.is_some() {
        clauses.push("user_id = ?1");
    }
    if !clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }
    sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", filter.limit.max(1)));

    let map_row = |row: &rusqlite::Row| -> rusqlite::Result<MemoryListRow> {
        Ok(MemoryListRow {
            id: row.get(0)?,
            user_id: row.get(1)?,
            created_at: row.get(2)?,
            last_seen_at: row.get(3)?,
            kind: row.get(4)?,
            level: row.get(5)?,
            repetitions: row.get(6)?,
            active: row.get::<_, i64>(7)? != 0,
            expires_at: row.get(8)?,
            text: row.get(9)?,
        })
    };

    // 只有按用户过滤时才有 ?1 占位符。Option::iter() 产出 0 或 1 个 &String，
    // 既统一了 0/1 参数、又避免空参数数组的类型推断歧义（&String: ToSql）。
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<MemoryListRow> = stmt
        .query_map(rusqlite::params_from_iter(filter.user_id.iter()), map_row)?
        .collect::<std::result::Result<_, _>>()?;
    Ok(rows)
}

/// CLI `memory show` 的完整明细（不含向量）。
#[derive(Debug, Serialize)]
pub struct MemoryDetail {
    pub id: String,
    pub user_id: String,
    pub subject: String,
    pub kind: String,
    pub level: i64,
    pub source: String,
    pub active: bool,
    pub repetitions: i64,
    pub access_count: i64,
    pub created_at: String,
    pub last_seen_at: String,
    pub last_accessed_at: Option<String>,
    pub expires_at: Option<String>,
    pub forgotten_at: Option<String>,
    pub superseded_by: Option<String>,
    pub superseded_at: Option<String>,
    pub entities: Vec<EntityView>,
    pub text: String,
}

/// show/forget 按 id 前缀解析（像 git 短哈希）。完整 id 精确命中优先；多前缀命中
/// 报歧义（提示多给几位）；无命中返回 None。active_only 时只在活跃记忆里找。
fn resolve_memory_id(conn: &Connection, input: &str, active_only: bool) -> Result<Option<String>> {
    let sql = if active_only {
        "SELECT id FROM memories WHERE (id = ?1 OR id LIKE ?1 || '%') AND active = 1 LIMIT 20"
    } else {
        "SELECT id FROM memories WHERE id = ?1 OR id LIKE ?1 || '%' LIMIT 20"
    };
    let mut stmt = conn.prepare(sql)?;
    let ids: Vec<String> = stmt
        .query_map(params![input], |row| row.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    // 完整 id 精确命中：即使它是别的更长 id 的前缀（UUID 不会）也优先。
    if ids.iter().any(|existing| existing.as_str() == input) {
        return Ok(Some(input.to_string()));
    }
    match ids.len() {
        0 => Ok(None),
        1 => Ok(Some(ids.into_iter().next().unwrap())),
        _ => bail!("id 前缀 {input} 匹配到 {} 条，请多给几位：\n{}", ids.len(), ids.join("\n")),
    }
}

/// CLI 用：按 id（或前缀）取单条记忆的完整明细 + 实体。只读打开。找不到返回 None。
pub fn cli_show_memory(cfg: &Config, id: &str) -> Result<Option<MemoryDetail>> {
    let conn = Connection::open(&cfg.db_path)
        .with_context(|| format!("无法打开数据库 {}", cfg.db_path))?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "query_only", true)?;

    let full_id = match resolve_memory_id(&conn, id, false)? {
        Some(full) => full,
        None => return Ok(None),
    };

    let detail = conn
        .query_row(
            "SELECT id, user_id, subject, kind, level, source, active, repetitions, \
             access_count, created_at, last_seen_at, last_accessed_at, expires_at, \
             forgotten_at, superseded_by, superseded_at, text \
             FROM memories WHERE id = ?1",
            params![full_id],
            |row| {
                Ok(MemoryDetail {
                    id: row.get(0)?,
                    user_id: row.get(1)?,
                    subject: row.get(2)?,
                    kind: row.get(3)?,
                    level: row.get(4)?,
                    source: row.get(5)?,
                    active: row.get::<_, i64>(6)? != 0,
                    repetitions: row.get(7)?,
                    access_count: row.get(8)?,
                    created_at: row.get(9)?,
                    last_seen_at: row.get(10)?,
                    last_accessed_at: row.get(11)?,
                    expires_at: row.get(12)?,
                    forgotten_at: row.get(13)?,
                    superseded_by: row.get(14)?,
                    superseded_at: row.get(15)?,
                    entities: Vec::new(),
                    text: row.get(16)?,
                })
            },
        )
        .optional()?;

    let Some(mut detail) = detail else {
        return Ok(None);
    };

    let mut stmt = conn.prepare(
        "SELECT e.name, e.type FROM memory_entities me \
         JOIN entities e ON e.key = me.entity_key WHERE me.memory_id = ?1",
    )?;
    detail.entities = stmt
        .query_map(params![id], |erow| {
            Ok(EntityView {
                name: erow.get(0)?,
                kind: erow.get(1)?,
            })
        })?
        .collect::<std::result::Result<_, _>>()?;
    Ok(Some(detail))
}

/// CLI `memory forget <id>`：软删除一条记忆（active=0），返回被删的文本；
/// 找不到或已是失效状态返回 None。是写操作，用可写连接（WAL + busy_timeout
/// 与运行中的服务并发安全）。
pub fn cli_forget_memory(cfg: &Config, id: &str, purge: bool) -> Result<Option<String>> {
    register_vec_extension();
    let conn = Connection::open(&cfg.db_path)
        .with_context(|| format!("无法打开数据库 {}", cfg.db_path))?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    // 硬删要靠 FK 级联清掉 memory_entities/memory_links。
    conn.pragma_update(None, "foreign_keys", true)?;
    ensure_vec_table(&conn, cfg.embedding_dimensions)?;

    // 硬删可作用于任意记忆；软删只针对活跃记忆。
    let full_id = match resolve_memory_id(&conn, id, !purge)? {
        Some(full) => full,
        None => return Ok(None),
    };
    let text: String = conn.query_row(
        "SELECT text FROM memories WHERE id = ?1",
        params![full_id],
        |row| row.get(0),
    )?;

    // 先删向量索引（趁 memories 行还在、rowid 可查；硬删后 rowid 会被复用污染新记忆）。
    conn.execute(
        "DELETE FROM vec_memories WHERE rowid = (SELECT rowid FROM memories WHERE id = ?1)",
        params![full_id],
    )?;
    if purge {
        conn.execute("DELETE FROM memories WHERE id = ?1", params![full_id])?;
    } else {
        conn.execute(
            "UPDATE memories SET active = 0, forgotten_at = ?1 WHERE id = ?2",
            params![now_iso(), full_id],
        )?;
    }
    Ok(Some(text))
}

/// CLI `memory stats` 的聚合结果。by_level/by_kind/oldest/newest 只统计活跃记忆。
#[derive(Debug, Serialize)]
pub struct MemoryStats {
    pub total: usize,
    pub active: usize,
    pub inactive: usize,
    pub users: usize,
    pub by_level: Vec<(i64, usize)>,
    pub by_kind: Vec<(String, usize)>,
    pub oldest: Option<String>,
    pub newest: Option<String>,
}

/// CLI 用：单次扫描 memories 在 Rust 侧聚合出统计。只读打开。
pub fn cli_stats(cfg: &Config, user: Option<&str>) -> Result<MemoryStats> {
    use std::collections::{BTreeMap, BTreeSet, HashMap};

    let conn = Connection::open(&cfg.db_path)
        .with_context(|| format!("无法打开数据库 {}", cfg.db_path))?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "query_only", true)?;

    let sql = if user.is_some() {
        "SELECT active, level, kind, created_at, user_id FROM memories WHERE user_id = ?1"
    } else {
        "SELECT active, level, kind, created_at, user_id FROM memories"
    };
    let mut stmt = conn.prepare(sql)?;
    // Option::iter() 产出 0 或 1 个 &&str（&str: ToSql），对应 SQL 里的 0/1 占位符。
    let rows = stmt.query_map(rusqlite::params_from_iter(user.iter()), |row| {
        Ok((
            row.get::<_, i64>(0)? != 0,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;

    let mut total = 0usize;
    let mut active = 0usize;
    let mut users: BTreeSet<String> = BTreeSet::new();
    let mut level_map: BTreeMap<i64, usize> = BTreeMap::new();
    let mut kind_map: HashMap<String, usize> = HashMap::new();
    let mut oldest: Option<String> = None;
    let mut newest: Option<String> = None;

    for row in rows {
        let (is_active, level, kind, created_at, user_id) = row?;
        total += 1;
        users.insert(user_id);
        if is_active {
            active += 1;
            *level_map.entry(level).or_default() += 1;
            *kind_map.entry(kind).or_default() += 1;
            if oldest.as_ref().map_or(true, |o| &created_at < o) {
                oldest = Some(created_at.clone());
            }
            if newest.as_ref().map_or(true, |n| &created_at > n) {
                newest = Some(created_at);
            }
        }
    }

    let by_level: Vec<(i64, usize)> = level_map.into_iter().collect();
    let mut by_kind: Vec<(String, usize)> = kind_map.into_iter().collect();
    by_kind.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    Ok(MemoryStats {
        total,
        active,
        inactive: total - active,
        users: users.len(),
        by_level,
        by_kind,
        oldest,
        newest,
    })
}

/// CLI `memory forget --all`：软删所有活跃记忆（active=0），或 purge 硬删全部。
/// 返回受影响条数。危险操作，调用方（cli）负责 --yes 确认。
pub fn cli_forget_all(cfg: &Config, purge: bool) -> Result<usize> {
    register_vec_extension();
    let conn = Connection::open(&cfg.db_path)
        .with_context(|| format!("无法打开数据库 {}", cfg.db_path))?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    ensure_vec_table(&conn, cfg.embedding_dimensions)?;

    let affected = if purge {
        // 清空向量索引 + 硬删全部记忆（WHERE 1 = ?1 恒真，避免空参数类型推断歧义）。
        conn.execute("DELETE FROM vec_memories", [])?;
        conn.execute("DELETE FROM memories WHERE 1 = ?1", params![1_i64])?
    } else {
        // 软删全部活跃记忆 → 全部移出向量索引（vec0 只存活跃记忆）。
        let n = conn.execute(
            "UPDATE memories SET active = 0, forgotten_at = ?1 WHERE active = 1",
            params![now_iso()],
        )?;
        conn.execute("DELETE FROM vec_memories", [])?;
        n
    };
    Ok(affected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(db_path: &str) -> Arc<Config> {
        let mut cfg = Config::from_env().unwrap();
        cfg.db_path = db_path.to_string();
        // 测试向量是 4 维，vec0 表须按同一维度建。
        cfg.embedding_dimensions = 4;
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
            .search_memories("u1".into(), unit(1.0, 0.2, 0.0, 0.0), None, None)
            .await
            .unwrap();
        assert!(results[0].text.starts_with("用户养了一只"));
        assert!(results[0].score.unwrap() > 0.9);
        let other = store
            .search_memories("u2".into(), unit(1.0, 0.0, 0.0, 0.0), None, None)
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
            .search_memories("u1".into(), unit(1.0, 0.0, 0.0, 0.0), None, None)
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
    async fn migrates_legacy_embedding_to_vec0() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        let path_str = path.to_str().unwrap().to_string();
        // 造一个"旧库"：memories 带 embedding 列 + 一条记忆，没有 vec0。
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE users (id TEXT PRIMARY KEY, created_at TEXT NOT NULL);
                 CREATE TABLE memories (
                   id TEXT PRIMARY KEY, user_id TEXT NOT NULL, text TEXT NOT NULL,
                   kind TEXT NOT NULL, level INTEGER NOT NULL, subject TEXT NOT NULL DEFAULT 'user',
                   embedding BLOB NOT NULL, fingerprint TEXT NOT NULL, source TEXT NOT NULL,
                   active INTEGER NOT NULL DEFAULT 1, repetitions INTEGER NOT NULL DEFAULT 1,
                   access_count INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL,
                   last_seen_at TEXT NOT NULL, last_accessed_at TEXT, expires_at TEXT,
                   forgotten_at TEXT, superseded_by TEXT, superseded_at TEXT
                 );",
            )
            .unwrap();
            conn.execute("INSERT INTO users (id, created_at) VALUES ('u1', 't')", [])
                .unwrap();
            conn.execute(
                "INSERT INTO memories (id, user_id, text, kind, level, subject, embedding, \
                 fingerprint, source, created_at, last_seen_at) \
                 VALUES ('m1', 'u1', '旧记忆', 'fact', 5, 'user', ?1, 'fp', 'test', 't', 't')",
                params![vec_to_blob(&unit(1.0, 0.0, 0.0, 0.0))],
            )
            .unwrap();
        }
        // Store::open 触发迁移：向量回填进 vec0，embedding 列被移除。
        let store = Store::open(test_config(&path_str)).unwrap();
        let results = store
            .search_memories("u1".into(), unit(1.0, 0.0, 0.0, 0.0), None, None)
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].text, "旧记忆");
        // embedding 列应已不存在。
        let has_col = store
            .run(|conn, _| {
                Ok(conn.query_row(
                    "SELECT count(*) FROM pragma_table_info('memories') WHERE name = 'embedding'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(has_col, 0);
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
