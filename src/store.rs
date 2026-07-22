//! SQLite 存储：对话/记忆/实体/情绪全在普通关系表里，没有向量、没有虚拟表。
//! 追加式（append-only）：记忆只新增，不因时间过期；软删除/取代仍保留可审计留痕。
//! 检索不在这一层做语义判断——本模块只负责按新近度端出候选池（[`Store::memory_pool`]）
//! 和按 id 取回正文（[`Store::memories_by_ids`]），选哪几条由 agent 层交给记忆模型决定。

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{Duration, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
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

/// 把文本切成字符三元组（中文按字、英文按字符，不需要分词器）。
/// 先滤掉空白与标点：近似去重要抓的正是「只差一个句号/逗号」这类改写，标点参与比较
/// 反而会把它们判成不同。短于 3 个字符的文本退化成它自己，保证永远至少产出一个 gram。
fn trigrams(text: &str) -> std::collections::HashSet<String> {
    let chars: Vec<char> = text
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect();
    if chars.len() < 3 {
        return std::iter::once(chars.into_iter().collect()).collect();
    }
    chars.windows(3).map(|w| w.iter().collect()).collect()
}

/// 字符三元组 Jaccard 相似度（0..=1）。用于「近乎完全相同的表述」的合并判定——
/// 取代了原先基于向量余弦的近似去重。注意它衡量的是**字面**重合而非语义：这正是
/// 这里想要的，真正的语义判重由记忆模型在巩固时 reconcile 完成。
pub fn trigram_similarity(a: &str, b: &str) -> f32 {
    let (ga, gb) = (trigrams(a), trigrams(b));
    let intersection = ga.intersection(&gb).count();
    let union = ga.len() + gb.len() - intersection;
    if union == 0 {
        return 0.0;
    }
    intersection as f32 / union as f32
}

/// 旧库退场：移除 `memories.level`。分级早已废弃（落库恒为同一等级），
/// 该列没有任何读者。停止维护并 DROP（幂等：无该列则跳过）。不可逆，但列本就无信息量。
fn migrate_drop_level_column(conn: &Connection) -> Result<()> {
    let has_col: bool = conn.query_row(
        "SELECT count(*) FROM pragma_table_info('memories') WHERE name = 'level'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;
    if has_col {
        conn.execute("ALTER TABLE memories DROP COLUMN level", [])?;
        tracing::info!("memories.level 列已移除（分级维度已废弃）");
    }
    Ok(())
}

/// 旧库退场：清掉向量存储的残留。检索改由记忆模型精选后，`vec_memories` 虚拟表与更早的
/// `memories.embedding` 列都没有读者了。
///
/// 两步都是尽力而为、失败不致命：本进程不再注册 sqlite-vec，`DROP TABLE vec_memories`
/// 会因「no such module: vec0」而失败——留着也只是一张再没有语句引用的死表，不影响任何
/// 读写。真想彻底清干净，用带 vec0 扩展的 sqlite3 手动 DROP 一次即可。
fn migrate_drop_vector_storage(conn: &Connection) -> Result<()> {
    match conn.execute("DROP TABLE IF EXISTS vec_memories", []) {
        Ok(_) => tracing::info!("vec_memories 虚拟表已移除（检索不再使用向量）"),
        Err(error) => {
            tracing::debug!("跳过 vec_memories 清理（缺 vec0 模块，无影响）：{error}")
        }
    }
    let has_col: bool = conn.query_row(
        "SELECT count(*) FROM pragma_table_info('memories') WHERE name = 'embedding'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;
    if has_col {
        conn.execute("ALTER TABLE memories DROP COLUMN embedding", [])?;
        tracing::info!("memories.embedding 列已移除（检索不再使用向量）");
    }
    Ok(())
}

/// 旧库补列：`conversations.memory_upto_seq`（自动记忆巩固的水位线）。新库由 SCHEMA
/// 直接带上，旧库 `CREATE TABLE IF NOT EXISTS` 不会补列，故在此 ALTER 补齐（幂等）。
fn ensure_conversation_columns(conn: &Connection) -> Result<()> {
    let has: bool = conn.query_row(
        "SELECT count(*) FROM pragma_table_info('conversations') WHERE name = 'memory_upto_seq'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0;
    if !has {
        conn.execute(
            "ALTER TABLE conversations ADD COLUMN memory_upto_seq INTEGER NOT NULL DEFAULT 0",
            [],
        )?;
    }
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
  summary_at TEXT,
  memory_upto_seq INTEGER NOT NULL DEFAULT 0
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
const MEMORY_COLUMNS: &str = "id, text, kind, subject, created_at";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityView {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
}

// Deserialize 供 agent 层把工具即时保存的记忆结果（本已 Serialize 成 JSON）读回 MemoryView，
// 收集进本轮返回的 `saved`；被跳过序列化的可选字段缺省即 None。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryView {
    pub id: String,
    pub text: String,
    pub kind: String,
    pub subject: String,
    pub created_at: String,
    #[serde(default)]
    pub entities: Vec<EntityView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deduplicated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_memory_id: Option<String>,
}

/// 候选池里的一条记忆：精选阶段只需要这几个字段（实体、时间戳等对挑选没有帮助，
/// 却会成倍放大喂给模型的 token）。
#[derive(Debug, Clone)]
pub struct MemoryCandidate {
    pub id: String,
    pub text: String,
    pub kind: String,
    pub subject: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatTurn {
    pub role: String,
    pub content: String,
}

/// 一批「已滑出短期窗口、越过某条水位线、尚未处理」的旧消息，供摘要或记忆巩固消费。
/// 已有摘要正文不在这里（摘要路径另经 [`Store::get_conversation_summary`] 取）。
#[derive(Debug, Clone)]
pub struct PendingBatch {
    pub messages: Vec<ChatTurn>,
    pub max_seq: i64,
}

#[derive(Debug, Clone)]
pub struct NewMemory {
    pub user_id: String,
    pub text: String,
    pub kind: String,
    pub subject: String,
    pub entities: Vec<EntityView>,
    pub source: String,
}

#[derive(Clone)]
pub struct Store {
    // 一把全局 Mutex 会让候选池扫描与后台落库互相排队；改用小连接池，
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
        let mut pool = Vec::with_capacity(DB_POOL_SIZE);
        for index in 0..DB_POOL_SIZE {
            let conn = Connection::open(path).with_context(|| {
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
            // schema 建表与旧库迁移只需在一条连接上做一次（均幂等）。
            // 追加式存储：不再有开机清理过期记忆这一步。
            if index == 0 {
                conn.execute_batch(SCHEMA)?;
                ensure_conversation_columns(&conn)?;
                migrate_drop_level_column(&conn)?;
                migrate_drop_vector_storage(&conn)?;
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

    /// 取「已滑出短期窗口（seq <= total-window）且越过水位线（seq > <watermark_col>）」的旧消息。
    /// `watermark_col` 是 conversations 上的水位线列名，由调用方以字面量指定（`summary_upto_seq`
    /// 摘要用、`memory_upto_seq` 记忆巩固用），二者互不影响；绝不接受外部输入以免注入。
    pub async fn messages_beyond_watermark(
        &self,
        user_id: String,
        conversation_id: String,
        watermark_col: &'static str,
        window: i64,
        limit: i64,
    ) -> Result<Option<PendingBatch>> {
        self.run(move |conn, _| {
            let convo: Option<(i64, i64)> = conn
                .query_row(
                    &format!(
                        "SELECT {watermark_col}, message_count FROM conversations \
                         WHERE id = ?1 AND user_id = ?2"
                    ),
                    params![conversation_id, user_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((upto, total)) = convo else {
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
            Ok(Some(PendingBatch {
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

    /// 尾巴 flush 用：列出「最后活动早于 idle_before、且还有未巩固消息」的会话。
    /// updated_at 由 now_iso 统一格式写入，字典序即时间序，可直接比较。
    pub async fn conversations_idle_pending(
        &self,
        idle_before: String,
        limit: i64,
    ) -> Result<Vec<(String, String)>> {
        self.run(move |conn, _| {
            let mut stmt = conn.prepare(
                "SELECT user_id, id FROM conversations \n                 WHERE updated_at < ?1 AND memory_upto_seq < message_count \n                 ORDER BY updated_at ASC LIMIT ?2",
            )?;
            let rows: Vec<(String, String)> = stmt
                .query_map(params![idle_before, limit.clamp(1, 1000)], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?
                .collect::<std::result::Result<_, _>>()?;
            Ok(rows)
        })
        .await
    }

    /// 推进自动记忆巩固的水位线（只增不减）。巩固成功后调用；失败则水位线不动、下轮重跑。
    pub async fn advance_memory_watermark(
        &self,
        user_id: String,
        conversation_id: String,
        upto_seq: i64,
    ) -> Result<()> {
        self.run(move |conn, _| {
            conn.execute(
                "UPDATE conversations SET memory_upto_seq = ?1 \n                 WHERE id = ?2 AND user_id = ?3 AND ?1 > memory_upto_seq",
                params![upto_seq, conversation_id, user_id],
            )?;
            Ok(())
        })
        .await
    }

    // ---------- 长期记忆 ----------

    /// 候选池：该用户的活跃记忆，按最近提及倒序取最多 `limit` 条，只带精选所需的
    /// 最小字段（id/text/kind/subject），不查实体也不组装 [`MemoryView`]。
    ///
    /// 这是「LLM 精选」检索的第一步：整池原样喂给记忆模型让它挑，所以这里刻意**不做**
    /// 任何相关性判断。超出 `limit` 时按 `last_seen_at` 截断——留下最近被提及的那部分，
    /// 是无语义信息可用时最合理的取舍。
    pub async fn memory_pool(
        &self,
        user_id: String,
        limit: usize,
    ) -> Result<Vec<MemoryCandidate>> {
        self.run(move |conn, _| {
            let mut stmt = conn.prepare(
                // rowid 做次级排序键：同一微秒内写入的两条记忆 last_seen_at 可能相等，
                // 光按时间排序结果不确定，截断时会随机丢掉其中一条。
                "SELECT id, text, kind, subject FROM memories \n                 WHERE user_id = ?1 AND active = 1 \n                 ORDER BY last_seen_at DESC, rowid DESC LIMIT ?2",
            )?;
            let rows: Vec<MemoryCandidate> = stmt
                .query_map(params![user_id, limit as i64], |row| {
                    Ok(MemoryCandidate {
                        id: row.get(0)?,
                        text: row.get(1)?,
                        kind: row.get(2)?,
                        subject: row.get(3)?,
                    })
                })?
                .collect::<std::result::Result<_, _>>()?;
            Ok(rows)
        })
        .await
    }

    /// 按 id 批量取回完整记忆（含实体），用于把精选结果还原成 [`MemoryView`]。
    /// 返回顺序与 `ids` 一致（模型给出的顺序即相关性顺序，要保住）；查不到的 id 静默跳过。
    pub async fn memories_by_ids(
        &self,
        user_id: String,
        ids: Vec<String>,
    ) -> Result<Vec<MemoryView>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.run(move |conn, _| {
            let placeholders = std::iter::repeat("?").take(ids.len()).collect::<Vec<_>>().join(",");
            let mut stmt = conn.prepare(&format!(
                "SELECT {MEMORY_COLUMNS} FROM memories \n                 WHERE active = 1 AND user_id = ?1 AND id IN ({placeholders})"
            ))?;
            // 绑定参数：?1 是 user_id，其后依次是各个 id。
            let params = std::iter::once(&user_id).chain(ids.iter());
            let rows: Vec<MemoryRow> = stmt
                .query_map(rusqlite::params_from_iter(params), MemoryRow::from_row)?
                .collect::<std::result::Result<_, _>>()?;
            drop(stmt);

            // 检索为纯读路径：不回写 access_count/last_accessed_at（两列无任何读者，
            // 每次检索都写一批纯属写放大，还和真正的写入抢 WAL 单写锁）。
            // 一次性取回所有命中记忆的实体，避免逐条查询（N+1）。
            let id_refs: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
            let mut entities = fetch_entities_map(conn, &id_refs)?;
            let mut by_id: std::collections::HashMap<String, MemoryView> = rows
                .iter()
                .map(|row| {
                    let view = memory_view_with_entities(
                        row,
                        entities.remove(&row.id).unwrap_or_default(),
                    );
                    (row.id.clone(), view)
                })
                .collect();
            // IN (...) 的返回顺序由 SQLite 决定，这里按调用方给的 id 顺序重排。
            Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
        })
        .await
    }

    pub async fn recent_memories(&self, user_id: String, limit: usize) -> Result<Vec<MemoryView>> {
        self.run(move |conn, _| {
            // 追加式永不写 expires_at，无需过期过滤；仍按 last_seen_at 倒序取最近活跃记忆。
            let mut stmt = conn.prepare(&format!(
                "SELECT {MEMORY_COLUMNS} FROM memories WHERE user_id = ?1 AND active = 1 \n                 ORDER BY last_seen_at DESC LIMIT ?2"
            ))?;
            let rows: Vec<MemoryRow> = stmt
                .query_map(
                    params![user_id, limit.clamp(1, 100) as i64],
                    MemoryRow::from_row,
                )?
                .collect::<std::result::Result<_, _>>()?;
            drop(stmt);
            let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
            let mut entities = fetch_entities_map(conn, &ids)?;
            let views = rows
                .iter()
                .map(|r| memory_view_with_entities(r, entities.remove(&r.id).unwrap_or_default()))
                .collect();
            Ok(views)
        })
        .await
    }

    pub async fn create_memory(&self, new: NewMemory) -> Result<MemoryView> {
        self.run(move |conn, _| create_memory_sync(conn, new)).await
    }

    pub async fn forget_memory(&self, user_id: String, memory_id: String) -> Result<bool> {
        self.run(move |conn, _| {
            // 软删：memories 行保留（可审计、可沿取代链回溯），只把 active 置 0。
            // 候选池与按 id 取回都过滤 active = 1，停用后自然不再被检索到。
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
                // 停用旧记忆并记下取代关系；memories 行保留，可沿链回溯完整演变史。
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
            let mut stmt = conn.prepare(&format!(
                "WITH RECURSIVE newer(id) AS ( \n                   SELECT superseded_by FROM memories WHERE id = :mid AND superseded_by IS NOT NULL \n                   UNION \n                   SELECT m.superseded_by FROM memories m JOIN newer n ON m.id = n.id \n                   WHERE m.superseded_by IS NOT NULL \n                 ), older(id) AS ( \n                   SELECT id FROM memories WHERE superseded_by = :mid \n                   UNION \n                   SELECT m.id FROM memories m JOIN older o ON m.superseded_by = o.id \n                 ) \n                 SELECT {MEMORY_COLUMNS}, active, superseded_at \n                 FROM memories \n                 WHERE user_id = :uid \n                   AND (id = :mid OR id IN (SELECT id FROM newer) OR id IN (SELECT id FROM older)) \n                 ORDER BY created_at"
            ))?;
            let rows: Vec<(MemoryRow, bool, Option<String>)> = stmt
                .query_map(
                    rusqlite::named_params! {":mid": memory_id, ":uid": user_id},
                    |row| {
                        Ok((
                            MemoryRow::from_row(row)?,
                            row.get::<_, i64>(5)? != 0,
                            row.get(6)?,
                        ))
                    },
                )?
                .collect::<std::result::Result<_, _>>()?;
            drop(stmt);
            let ids: Vec<&str> = rows.iter().map(|(row, _, _)| row.id.as_str()).collect();
            let mut entities = fetch_entities_map(conn, &ids)?;
            let views = rows
                .into_iter()
                .map(|(rowdata, active, superseded_at)| {
                    let mut view = memory_view_with_entities(
                        &rowdata,
                        entities.remove(&rowdata.id).unwrap_or_default(),
                    );
                    view.active = Some(active);
                    view.superseded_at = superseded_at;
                    view
                })
                .collect();
            Ok(views)
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
    subject: String,
    created_at: String,
}

impl MemoryRow {
    /// 从以 [`MEMORY_COLUMNS`] 顺序开头的行取出各列（后续列由调用方另取）。
    fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Self> {
        Ok(MemoryRow {
            id: row.get(0)?,
            text: row.get(1)?,
            kind: row.get(2)?,
            subject: row.get(3)?,
            created_at: row.get(4)?,
        })
    }
}

/// 一次查询取回多条记忆的实体并按 memory_id 归组，供列表场景避免逐条查询（N+1）。
/// 空输入直接返回空表（`IN ()` 非法）；同一记忆的实体顺序不保证，调用方不应依赖。
fn fetch_entities_map(
    conn: &Connection,
    ids: &[&str],
) -> Result<std::collections::HashMap<String, Vec<EntityView>>> {
    let mut map: std::collections::HashMap<String, Vec<EntityView>> =
        std::collections::HashMap::new();
    if ids.is_empty() {
        return Ok(map);
    }
    let marks = vec!["?"; ids.len()].join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT me.memory_id, e.name, e.type FROM memory_entities me \
         JOIN entities e ON e.key = me.entity_key WHERE me.memory_id IN ({marks})"
    ))?;
    let rows = stmt.query_map(rusqlite::params_from_iter(ids.iter()), |row| {
        Ok((
            row.get::<_, String>(0)?,
            EntityView {
                name: row.get(1)?,
                kind: row.get(2)?,
            },
        ))
    })?;
    for row in rows {
        let (memory_id, entity) = row?;
        map.entry(memory_id).or_default().push(entity);
    }
    Ok(map)
}

/// 用已备好的实体列表拼出 MemoryView（不触库）；可选字段留 None，由调用方按需覆写。
fn memory_view_with_entities(row: &MemoryRow, entities: Vec<EntityView>) -> MemoryView {
    MemoryView {
        id: row.id.clone(),
        text: row.text.clone(),
        kind: row.kind.clone(),
        subject: row.subject.clone(),
        created_at: row.created_at.clone(),
        entities,
        score: None,
        deduplicated: None,
        active: None,
        superseded_at: None,
        superseded: None,
        superseded_memory_id: None,
    }
}

/// 单条记忆构建 MemoryView（touch/create/load 等单行路径用）；列表路径应改用
/// [`fetch_entities_map`] + [`memory_view_with_entities`] 批量取实体，避免 N+1。
fn memory_view_from_row(conn: &Connection, row: &MemoryRow) -> Result<MemoryView> {
    let entities = fetch_entities_map(conn, &[row.id.as_str()])?
        .remove(&row.id)
        .unwrap_or_default();
    Ok(memory_view_with_entities(row, entities))
}

fn load_memory_row(conn: &Connection, id: &str) -> Result<MemoryRow> {
    Ok(conn.query_row(
        &format!("SELECT {MEMORY_COLUMNS} FROM memories WHERE id = ?1"),
        params![id],
        MemoryRow::from_row,
    )?)
}

/// 同一记忆再次出现：更新最近提及时间与计数。
/// 追加式下不再有过期续期这回事，只维护 last_seen_at / repetitions。
fn touch_memory(conn: &Connection, id: &str, now: &str) -> Result<MemoryView> {
    conn.execute(
        "UPDATE memories SET last_seen_at = ?1, repetitions = repetitions + 1 WHERE id = ?2",
        params![now, id],
    )?;
    let row = load_memory_row(conn, id)?;
    let mut view = memory_view_from_row(conn, &row)?;
    view.deduplicated = Some(true);
    Ok(view)
}

fn create_memory_sync(conn: &mut Connection, new: NewMemory) -> Result<MemoryView> {
    let subject = if new.subject == "assistant" { "assistant" } else { "user" };
    let text = new.text.trim().to_string();
    let fingerprint = hex::encode(Sha256::digest(text.to_lowercase().as_bytes()));
    let now = now_iso();

    // 去重按主体隔离：同样文本但主体不同（关于用户 vs 关于助手）不应合并。
    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM memories WHERE user_id = ?1 AND fingerprint = ?2 \n             AND active = 1 AND subject = ?3 LIMIT 1",
            params![new.user_id, fingerprint, subject],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return touch_memory(conn, &id, &now);
    }

    // 近乎完全相同的表述用高阈值合并（字符三元组 Jaccard，默认 0.9）：扫同 user/subject
    // 的活跃记忆，最相似的一条若 ≥ 阈值即视为重复、并入旧记忆。作用域与上面的指纹去重一致
    // （都按 user+subject）；不按 kind 过滤，好让「同一件事被两次巩固标成不同 kind」的近似
    // 重复也能合并。
    //
    // 这一步只兜「改了个标点/语气词」这类字面近重；语义层面的重复（换种说法讲同一件事）
    // 由记忆模型在巩固时对照已有记忆 reconcile 成 update，不指望这里。
    //
    // 长度比在 [0.5, 2] 之外的直接跳过：字数差一倍以上不可能达到 0.9 的 Jaccard，
    // 先用一次整数比较挡掉大部分候选，省去建三元组集合的开销。
    let new_chars = text.chars().count();
    let mut stmt = conn.prepare(
        "SELECT id, text FROM memories \n         WHERE user_id = ?1 AND subject = ?2 AND active = 1",
    )?;
    let candidates: Vec<(String, String)> = stmt
        .query_map(params![new.user_id, subject], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<std::result::Result<_, _>>()?;
    drop(stmt);
    let mut best: Option<(String, f32)> = None;
    for (id, existing_text) in candidates {
        let existing_chars = existing_text.chars().count();
        if existing_chars * 2 < new_chars || new_chars * 2 < existing_chars {
            continue;
        }
        let similarity = trigram_similarity(&text, &existing_text);
        if best.as_ref().map_or(true, |(_, top)| similarity > *top) {
            best = Some((id, similarity));
        }
    }
    if let Some((id, similarity)) = best {
        if similarity >= crate::config::MEMORY_DUPLICATE_THRESHOLD {
            return touch_memory(conn, &id, &now);
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
        "INSERT INTO memories (id, user_id, text, kind, subject, \n         fingerprint, source, created_at, last_seen_at) \n         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        params![
            memory_id,
            new.user_id,
            text,
            new.kind,
            subject,
            fingerprint,
            new.source,
            now,
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

/// CLI `memory list` 的一行（只列活跃记忆的要点）。
#[derive(Debug, Serialize)]
pub struct MemoryListRow {
    pub id: String,
    pub user_id: String,
    pub created_at: String,
    pub last_seen_at: String,
    pub kind: String,
    pub repetitions: i64,
    pub text: String,
}

/// `memory list` 的过滤条件。
pub struct ListFilter {
    /// 只看某个 user_id；None = 全部用户。
    pub user_id: Option<String>,
    pub limit: usize,
}

/// CLI 用：直接查活跃记忆，不建表、不清理、不预热。用 query_only 防写，与运行中的
/// 服务共享同一 WAL 数据库（同机多进程读安全）。
pub fn cli_list_memories(cfg: &Config, filter: &ListFilter) -> Result<Vec<MemoryListRow>> {
    let conn = Connection::open(&cfg.db_path)
        .with_context(|| format!("无法打开数据库 {}", cfg.db_path))?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    // 防御性只读：本连接拒绝任何写入，但仍能正常读 WAL。
    conn.pragma_update(None, "query_only", true)?;

    let mut sql = String::from(
        "SELECT id, user_id, created_at, last_seen_at, kind, repetitions, text \
         FROM memories WHERE active = 1",
    );
    if filter.user_id.is_some() {
        sql.push_str(" AND user_id = ?1");
    }
    sql.push_str(&format!(" ORDER BY created_at DESC LIMIT {}", filter.limit.max(1)));

    let map_row = |row: &rusqlite::Row| -> rusqlite::Result<MemoryListRow> {
        Ok(MemoryListRow {
            id: row.get(0)?,
            user_id: row.get(1)?,
            created_at: row.get(2)?,
            last_seen_at: row.get(3)?,
            kind: row.get(4)?,
            repetitions: row.get(5)?,
            text: row.get(6)?,
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

/// delete 按 id 前缀解析（像 git 短哈希）。完整 id 精确命中优先；多前缀命中
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

/// CLI `memory delete <id>`：硬删除一条记忆（彻底 `DELETE` + FK 级联清实体链接），
/// 返回被删文本；找不到返回 None。不可逆。
pub fn cli_delete_memory(cfg: &Config, id: &str) -> Result<Option<String>> {
    let conn = Connection::open(&cfg.db_path)
        .with_context(|| format!("无法打开数据库 {}", cfg.db_path))?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    // 级联清掉 memory_entities/memory_links。
    conn.pragma_update(None, "foreign_keys", true)?;

    let full_id = match resolve_memory_id(&conn, id, false)? {
        Some(full) => full,
        None => return Ok(None),
    };
    let text: String = conn.query_row(
        "SELECT text FROM memories WHERE id = ?1",
        params![full_id],
        |row| row.get(0),
    )?;
    conn.execute("DELETE FROM memories WHERE id = ?1", params![full_id])?;
    Ok(Some(text))
}

/// CLI `memory stats` 的聚合结果。by_kind/oldest/newest 只统计活跃记忆。
#[derive(Debug, Serialize)]
pub struct MemoryStats {
    pub total: usize,
    pub active: usize,
    pub inactive: usize,
    pub users: usize,
    pub by_kind: Vec<(String, usize)>,
    pub oldest: Option<String>,
    pub newest: Option<String>,
}

/// CLI 用：单次扫描 memories 在 Rust 侧聚合出统计。只读打开。
pub fn cli_stats(cfg: &Config, user: Option<&str>) -> Result<MemoryStats> {
    use std::collections::{BTreeSet, HashMap};

    let conn = Connection::open(&cfg.db_path)
        .with_context(|| format!("无法打开数据库 {}", cfg.db_path))?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "query_only", true)?;

    let sql = if user.is_some() {
        "SELECT active, kind, created_at, user_id FROM memories WHERE user_id = ?1"
    } else {
        "SELECT active, kind, created_at, user_id FROM memories"
    };
    let mut stmt = conn.prepare(sql)?;
    // Option::iter() 产出 0 或 1 个 &&str（&str: ToSql），对应 SQL 里的 0/1 占位符。
    let rows = stmt.query_map(rusqlite::params_from_iter(user.iter()), |row| {
        Ok((
            row.get::<_, i64>(0)? != 0,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;

    let mut total = 0usize;
    let mut active = 0usize;
    let mut users: BTreeSet<String> = BTreeSet::new();
    let mut kind_map: HashMap<String, usize> = HashMap::new();
    let mut oldest: Option<String> = None;
    let mut newest: Option<String> = None;

    for row in rows {
        let (is_active, kind, created_at, user_id) = row?;
        total += 1;
        users.insert(user_id);
        if is_active {
            active += 1;
            *kind_map.entry(kind).or_default() += 1;
            if oldest.as_ref().map_or(true, |o| &created_at < o) {
                oldest = Some(created_at.clone());
            }
            if newest.as_ref().map_or(true, |n| &created_at > n) {
                newest = Some(created_at);
            }
        }
    }

    let mut by_kind: Vec<(String, usize)> = kind_map.into_iter().collect();
    by_kind.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    Ok(MemoryStats {
        total,
        active,
        inactive: total - active,
        users: users.len(),
        by_kind,
        oldest,
        newest,
    })
}

/// CLI `memory delete --all`：硬删全部记忆（彻底 `DELETE` + 级联），返回条数。
/// 危险操作、不可逆，调用方（cli）负责 --yes 确认。
pub fn cli_delete_all(cfg: &Config) -> Result<usize> {
    let conn = Connection::open(&cfg.db_path)
        .with_context(|| format!("无法打开数据库 {}", cfg.db_path))?;
    conn.pragma_update(None, "busy_timeout", 5000)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    // WHERE 1 = ?1 恒真，既删全部又避免空参数数组的类型推断歧义。
    let n = conn.execute("DELETE FROM memories WHERE 1 = ?1", params![1_i64])?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(db_path: &str) -> Arc<Config> {
        let mut cfg = Config::from_env().unwrap();
        cfg.db_path = db_path.to_string();
        Arc::new(cfg)
    }

    fn mem(user: &str, text: &str, kind: &str) -> NewMemory {
        NewMemory {
            user_id: user.into(),
            text: text.into(),
            kind: kind.into(),
            subject: "user".into(),
            entities: Vec::new(),
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
    fn trigram_similarity_separates_near_dupes_from_distinct_text() {
        // 只差一个句号 → 标点被滤掉，完全相同；换个说法讲同一件事 → 远低于合并阈值
        //（这类语义重复交给记忆模型 reconcile，不归近似去重管）。
        assert_eq!(trigram_similarity("用户喜欢喝美式咖啡", "用户喜欢喝美式咖啡。"), 1.0);
        assert!(trigram_similarity("用户养了一只叫年糕的猫", "用户家里有只小猫咪") < 0.5);
        // 否定与肯定字面高度重合，但阈值 0.9 仍能分开，不会被误合并。
        assert!(trigram_similarity("用户喜欢香菜", "用户不喜欢香菜") < 0.9);
        assert_eq!(trigram_similarity("猫", "猫"), 1.0);
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
    async fn pool_is_recency_ordered_and_user_isolated() {
        let (_dir, store) = open_store();
        let cat = store
            .create_memory(mem("u1", "用户养了一只叫年糕的猫", "fact"))
            .await
            .unwrap();
        assert_eq!(cat.deduplicated, Some(false));
        let show = store
            .create_memory(mem("u1", "用户最近在追一部剧", "event"))
            .await
            .unwrap();

        // 候选池按 last_seen_at 倒序：后写入的排前面。
        let pool = store.memory_pool("u1".into(), 10).await.unwrap();
        assert_eq!(
            pool.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            vec!["用户最近在追一部剧", "用户养了一只叫年糕的猫"]
        );
        // limit 截断保留最近的那部分。
        let capped = store.memory_pool("u1".into(), 1).await.unwrap();
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].text, "用户最近在追一部剧");
        // 用户隔离。
        assert!(store.memory_pool("u2".into(), 10).await.unwrap().is_empty());

        // 按 id 取回：顺序跟随传入的 id，跨用户取不到。
        let views = store
            .memories_by_ids("u1".into(), vec![cat.id.clone(), show.id.clone()])
            .await
            .unwrap();
        assert_eq!(
            views.iter().map(|v| v.text.as_str()).collect::<Vec<_>>(),
            vec!["用户养了一只叫年糕的猫", "用户最近在追一部剧"]
        );
        assert!(store
            .memories_by_ids("u2".into(), vec![cat.id])
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn memories_by_ids_skips_unknown_ids() {
        let (_dir, store) = open_store();
        let kept = store.create_memory(mem("u1", "用户在学日语", "goal")).await.unwrap();
        let views = store
            .memories_by_ids(
                "u1".into(),
                vec!["不存在的-id".to_string(), kept.id.clone()],
            )
            .await
            .unwrap();
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].id, kept.id);
    }

    #[tokio::test]
    async fn fingerprint_dedupe_merges() {
        let (_dir, store) = open_store();
        let first = store.create_memory(mem("u1", "用户在学日语", "goal")).await.unwrap();
        let again = store.create_memory(mem("u1", "用户在学日语", "goal")).await.unwrap();
        assert_eq!(again.deduplicated, Some(true));
        assert_eq!(again.id, first.id);
    }

    #[tokio::test]
    async fn near_duplicate_text_merges() {
        let (_dir, store) = open_store();
        let first = store
            .create_memory(mem("u1", "用户喜欢喝美式咖啡", "preference"))
            .await
            .unwrap();
        // 只多一个句号：指纹不同，靠三元组相似度合并。
        let merged = store
            .create_memory(mem("u1", "用户喜欢喝美式咖啡。", "preference"))
            .await
            .unwrap();
        assert_eq!(merged.deduplicated, Some(true));
        assert_eq!(merged.id, first.id);
    }

    #[tokio::test]
    async fn near_duplicate_merges_across_kind() {
        // 同一件事被两次巩固标成不同 kind、措辞略有差异（指纹不同）：近似去重应仍按
        // (user, subject) 合并，不因 kind 不同而各存一条。
        let (_dir, store) = open_store();
        let first = store.create_memory(mem("u1", "用户在学日语", "goal")).await.unwrap();
        let merged = store.create_memory(mem("u1", "用户在学日语。", "fact")).await.unwrap();
        assert_eq!(merged.deduplicated, Some(true));
        assert_eq!(merged.id, first.id);
    }

    #[tokio::test]
    async fn distinct_memories_are_not_merged() {
        // 措辞不同但讲同一件事：字面相似度不够，这里应各存一条（语义合并归记忆模型管）。
        let (_dir, store) = open_store();
        store
            .create_memory(mem("u1", "用户养了一只叫年糕的猫", "fact"))
            .await
            .unwrap();
        let second = store
            .create_memory(mem("u1", "用户家里有只小猫咪", "fact"))
            .await
            .unwrap();
        assert_eq!(second.deduplicated, Some(false));
        assert_eq!(store.memory_pool("u1".into(), 10).await.unwrap().len(), 2);
        // 否定句不能被并进肯定句。
        store.create_memory(mem("u1", "用户喜欢香菜", "preference")).await.unwrap();
        let negated = store
            .create_memory(mem("u1", "用户不喜欢香菜", "preference"))
            .await
            .unwrap();
        assert_eq!(negated.deduplicated, Some(false));
    }

    #[tokio::test]
    async fn forget_supersede_and_history() {
        let (_dir, store) = open_store();
        let old = store
            .create_memory(mem("u1", "用户在 A 公司上班", "fact"))
            .await
            .unwrap();
        let new = store
            .supersede_memory(old.id.clone(), mem("u1", "用户跳槽到了 B 公司", "fact"))
            .await
            .unwrap();
        assert_eq!(new.superseded, Some(true));
        // 被取代的旧记忆已停用，不再进候选池。
        let texts: Vec<String> = store
            .memory_pool("u1".into(), 10)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.text)
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
    async fn drops_legacy_vector_columns_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        let path_str = path.to_str().unwrap().to_string();
        // 造一个「旧库」：memories 还带着 level 与 embedding 两列，以及一条记忆。
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
                 VALUES ('m1', 'u1', '旧记忆', 'fact', 5, 'user', x'0000', 'fp', 'test', 't', 't')",
                [],
            )
            .unwrap();
        }
        // Store::open 触发迁移：两列都被移除，记忆本身完好保留。
        let store = Store::open(test_config(&path_str)).unwrap();
        let pool = store.memory_pool("u1".into(), 10).await.unwrap();
        assert_eq!(pool.len(), 1);
        assert_eq!(pool[0].text, "旧记忆");
        for column in ["embedding", "level"] {
            let has_col = store
                .run(move |conn, _| {
                    Ok(conn.query_row(
                        "SELECT count(*) FROM pragma_table_info('memories') WHERE name = ?1",
                        params![column],
                        |r| r.get::<_, i64>(0),
                    )?)
                })
                .await
                .unwrap();
            assert_eq!(has_col, 0, "{column} 列应已被移除");
        }
    }

    #[tokio::test]
    async fn consolidation_watermark_advances_and_drains() {
        let (_dir, store) = open_store();
        // 存 5 条消息（seq 1..=5）。窗口 = 2，故「已滑出窗口」= seq <= 5-2 = 3。
        for i in 0..5 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            store
                .save_message("u1".into(), "c1".into(), role.into(), format!("消息{i}"))
                .await
                .unwrap();
        }
        let batch = store
            .messages_beyond_watermark("u1".into(), "c1".into(), "memory_upto_seq", 2, 200)
            .await
            .unwrap()
            .expect("应有已滑出窗口的待巩固消息");
        assert_eq!(batch.messages.len(), 3);
        assert_eq!(batch.max_seq, 3);

        // 推进水位线后，这批不再被取到（只剩仍在窗口内的 seq 4、5，未滑出）。
        store
            .advance_memory_watermark("u1".into(), "c1".into(), batch.max_seq)
            .await
            .unwrap();
        assert!(store
            .messages_beyond_watermark("u1".into(), "c1".into(), "memory_upto_seq", 2, 200)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn idle_pending_lists_unconsolidated_convos() {
        let (_dir, store) = open_store();
        for i in 0..3 {
            let role = if i % 2 == 0 { "user" } else { "assistant" };
            store
                .save_message("u1".into(), "c1".into(), role.into(), format!("m{i}"))
                .await
                .unwrap();
        }
        // 未来时刻做上界：更新时间必然早于它 → 该会话被视为空闲且有未巩固消息。
        let future = "2999-01-01T00:00:00.000000+00:00".to_string();
        assert_eq!(
            store
                .conversations_idle_pending(future.clone(), 100)
                .await
                .unwrap(),
            vec![("u1".to_string(), "c1".to_string())]
        );
        // 过去时刻做上界：更新时间晚于它 → 不算空闲，取不到。
        assert!(store
            .conversations_idle_pending("2000-01-01T00:00:00.000000+00:00".into(), 100)
            .await
            .unwrap()
            .is_empty());
        // 全部巩固后（水位线 = message_count = 3），即便空闲也不再列出。
        store
            .advance_memory_watermark("u1".into(), "c1".into(), 3)
            .await
            .unwrap();
        assert!(store
            .conversations_idle_pending(future, 100)
            .await
            .unwrap()
            .is_empty());
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
