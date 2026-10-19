use crate::domain::*;
use anyhow::{Result, ensure};
use fs2::FileExt;
use serde_json::{Value, json};
use sqlx::{
    Row, Sqlite, SqlitePool, Transaction,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};
use std::{path::Path, sync::Arc, time::Duration};

#[derive(Clone)]
pub struct Store {
    pub pool: SqlitePool,
    pub memory_write_metrics: Arc<crate::telemetry::Metrics>,
    pub(crate) extract_every_chars: usize,
    _lock: Arc<std::fs::File>,
    embeddings: Arc<std::sync::Mutex<std::collections::HashMap<String, CachedEmbedding>>>,
}
type CachedEmbedding = (i64, String, Arc<Vec<f32>>);

pub enum BeginTurn {
    Started(Message),
    Existing {
        status: String,
        content: Option<String>,
    },
    Busy,
    Missing,
    Conflict,
    InvalidAssets,
}
pub(crate) type MemoryRecords = Vec<(Candidate, Vec<f32>, Option<i64>)>;
pub(crate) type BatchMemoryRecords = Vec<(Message, MemoryRecords)>;

impl Store {
    pub async fn open(path: &str) -> Result<Self> {
        if let Some(parent) = Path::new(path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(format!("{path}.lock"))?;
        lock.try_lock_exclusive()
            .map_err(|_| anyhow::anyhow!("database is already owned by another Mneme process"))?;
        let file = options.open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        drop(file);
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(10));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        sqlx::migrate!().run(&pool).await?;
        Ok(Self {
            pool,
            memory_write_metrics: Default::default(),
            extract_every_chars: 10_000,
            _lock: Arc::new(lock),
            embeddings: Default::default(),
        })
    }

    /// Single application process per SQLite file. Interrupted generations are explicit failures.
    pub async fn recover(&self) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("UPDATE model_requests SET status='interrupted' WHERE status='running'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE tool_calls SET status='failed',error='interrupted',completed_at=? WHERE status='running'").bind(now()).execute(&mut *tx).await?;
        sqlx::query("UPDATE turns SET status='failed', error_code='interrupted', completed_at=? WHERE status='generating'")
            .bind(now()).execute(&mut *tx).await?;
        sqlx::query("UPDATE sessions SET active_turn=NULL")
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE jobs SET status='pending' WHERE status='running'")
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE qq_inbox SET status='pending' WHERE status='running'")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn create_session(&self) -> Result<String> {
        self.conversation().await
    }

    /// All transports share one durable context and one generation lock.
    pub async fn conversation(&self) -> Result<String> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let session = Self::conversation_in(&mut tx).await?;
        tx.commit().await?;
        Ok(session)
    }
    pub(crate) async fn conversation_in(tx: &mut Transaction<'_, Sqlite>) -> Result<String> {
        let saved = sqlx::query_scalar::<_, String>(
            "SELECT value FROM metadata WHERE key='conversation.id'",
        )
        .fetch_optional(&mut **tx)
        .await?;
        let global: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM metadata WHERE key='conversation.scope' AND value='global')")
            .fetch_one(&mut **tx).await?;
        if global && let Some(session) = &saved {
            return Ok(session.clone());
        }
        let sessions: Vec<String> =
            sqlx::query_scalar("SELECT id FROM sessions ORDER BY created_at,id")
                .fetch_all(&mut **tx)
                .await?;
        let session = saved
            .or_else(|| sessions.first().cloned())
            .unwrap_or_else(id);
        sqlx::query("INSERT OR IGNORE INTO sessions(id,created_at) VALUES(?,?)")
            .bind(&session)
            .bind(now())
            .execute(&mut **tx)
            .await?;
        let histories: Vec<String> = sqlx::query_scalar("SELECT DISTINCT session_id FROM messages")
            .fetch_all(&mut **tx)
            .await?;
        if histories.len() > 1 {
            // Rebuild from the preserved raw history; separate rolling summaries cannot simply
            // share a cursor without skipping messages from another transport.
            sqlx::query("DELETE FROM conversation_context")
                .execute(&mut **tx)
                .await?;
        } else if let Some(old) = histories.first() {
            sqlx::query("DELETE FROM conversation_context WHERE session_id<>?")
                .bind(old)
                .execute(&mut **tx)
                .await?;
            sqlx::query(
                "UPDATE conversation_context SET session_id=? WHERE session_id=? AND session_id<>?",
            )
            .bind(&session)
            .bind(old)
            .bind(&session)
            .execute(&mut **tx)
            .await?;
        }
        // Mark old QQ turns before moving their session IDs. New turns use the inbox ID.
        sqlx::query("UPDATE turns SET channel='qq' WHERE session_id IN (SELECT session_id FROM qq_sessions UNION SELECT session_id FROM qq_inbox)")
            .execute(&mut **tx).await?;
        for old in &sessions {
            for table in [
                "messages",
                "turns",
                "assets",
                "tool_calls",
                "model_requests",
                "qq_sessions",
                "qq_inbox",
            ] {
                sqlx::query(&format!(
                    "UPDATE {table} SET session_id=? WHERE session_id=?"
                ))
                .bind(&session)
                .bind(old)
                .execute(&mut **tx)
                .await?;
            }
            sqlx::query("INSERT INTO conversation_aliases(session_id,conversation_id) VALUES(?,?) ON CONFLICT(session_id) DO UPDATE SET conversation_id=excluded.conversation_id")
                .bind(old)
                .bind(&session)
                .execute(&mut **tx)
                .await?;
        }
        sqlx::query(
            "INSERT OR IGNORE INTO conversation_aliases(session_id,conversation_id) VALUES(?,?)",
        )
        .bind(&session)
        .bind(&session)
        .execute(&mut **tx)
        .await?;
        sqlx::query("UPDATE conversation_aliases SET conversation_id=?")
            .bind(&session)
            .execute(&mut **tx)
            .await?;
        sqlx::query("INSERT INTO metadata(key,value) VALUES('conversation.id',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(&session)
            .execute(&mut **tx)
            .await?;
        sqlx::query("INSERT INTO metadata(key,value) VALUES('conversation.scope','global') ON CONFLICT(key) DO UPDATE SET value='global'")
            .execute(&mut **tx).await?;
        Ok(session)
    }

    pub async fn resolve_conversation(&self, session: &str) -> Result<String> {
        self.conversation().await?;
        Ok(sqlx::query_scalar(
            "SELECT conversation_id FROM conversation_aliases WHERE session_id=?",
        )
        .bind(session)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or_else(|| session.to_owned()))
    }

    pub async fn begin_turn(&self, session: &str, request: &str, text: &str) -> Result<BeginTurn> {
        self.begin_turn_with_assets(session, request, text, &[])
            .await
    }

    pub async fn begin_turn_with_assets(
        &self,
        session: &str,
        request: &str,
        text: &str,
        assets: &[String],
    ) -> Result<BeginTurn> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if assets.len() > 4
            || assets
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != assets.len()
        {
            return Ok(BeginTurn::InvalidAssets);
        }
        for asset in assets {
            let valid: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM assets WHERE id=? AND session_id=?)",
            )
            .bind(asset)
            .bind(session)
            .fetch_one(&mut *tx)
            .await?;
            if !valid {
                return Ok(BeginTurn::InvalidAssets);
            }
        }
        let existing = sqlx::query("SELECT t.session_id,t.status,m.content FROM turns t JOIN messages m ON m.turn_id=t.id AND m.role='user' WHERE t.id=?")
            .bind(request).fetch_optional(&mut *tx).await?;
        if let Some(row) = existing {
            let previous: Vec<String> = sqlx::query_scalar("SELECT ma.asset_id FROM message_assets ma JOIN messages m ON m.id=ma.message_id WHERE m.turn_id=? ORDER BY ma.position").bind(request).fetch_all(&mut *tx).await?;
            if row.get::<String, _>("session_id") != session
                || row.get::<String, _>("content") != text
                || previous != assets
            {
                return Ok(BeginTurn::Conflict);
            }
            let content = sqlx::query_scalar(
                "SELECT content FROM messages WHERE turn_id=? AND role='assistant'",
            )
            .bind(request)
            .fetch_optional(&mut *tx)
            .await?;
            return Ok(BeginTurn::Existing {
                status: row.get("status"),
                content,
            });
        }
        let row = sqlx::query("SELECT active_turn FROM sessions WHERE id=?")
            .bind(session)
            .fetch_optional(&mut *tx)
            .await?;
        let Some(row) = row else {
            return Ok(BeginTurn::Missing);
        };
        if row.get::<Option<String>, _>("active_turn").is_some() {
            return Ok(BeginTurn::Busy);
        }
        let message = Message {
            id: id(),
            session_id: session.into(),
            turn_id: request.into(),
            role: "user".into(),
            content: text.into(),
            created_at: now(),
        };
        sqlx::query(
            "INSERT INTO turns(id,session_id,status,created_at,channel) VALUES(?,?,'generating',?,CASE WHEN EXISTS(SELECT 1 FROM qq_inbox WHERE id=?) THEN 'qq' ELSE 'api' END)",
        )
        .bind(request)
        .bind(session)
        .bind(message.created_at)
        .bind(request)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO messages(id,session_id,turn_id,role,content,created_at) VALUES(?,?,?,'user',?,?)")
            .bind(&message.id).bind(session).bind(request).bind(text).bind(message.created_at).execute(&mut *tx).await?;
        for (position, asset) in assets.iter().enumerate() {
            sqlx::query("INSERT INTO message_assets(message_id,asset_id,position) VALUES(?,?,?)")
                .bind(&message.id)
                .bind(asset)
                .bind(position as i64)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("UPDATE sessions SET active_turn=? WHERE id=?")
            .bind(request)
            .bind(session)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(BeginTurn::Started(message))
    }

    pub async fn complete_turn(&self, user: &Message, content: &str) -> Result<String> {
        self.complete_turn_with_media(user, content, &[], &[]).await
    }

    pub(crate) async fn complete_turn_with_media(
        &self,
        user: &Message,
        content: &str,
        stickers: &[String],
        images: &[String],
    ) -> Result<String> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let changed = sqlx::query(
            "UPDATE turns SET status='complete',completed_at=? WHERE id=? AND status='generating'",
        )
        .bind(now())
        .bind(&user.turn_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        ensure!(changed == 1, "turn is no longer generating");
        let message_id = id();
        sqlx::query("INSERT INTO messages(id,session_id,turn_id,role,content,created_at) VALUES(?,?,?,'assistant',?,?)")
            .bind(&message_id).bind(&user.session_id).bind(&user.turn_id).bind(content).bind(now()).execute(&mut *tx).await?;
        sqlx::query("UPDATE sessions SET active_turn=NULL WHERE id=? AND active_turn=?")
            .bind(&user.session_id)
            .bind(&user.turn_id)
            .execute(&mut *tx)
            .await?;
        for sticker in stickers {
            let added = sqlx::query("INSERT INTO message_stickers(message_id,image_id,name) SELECT ?,image_id,name FROM stickers WHERE id=? AND enabled=1 AND analysis_status IN ('complete','manual') AND COALESCE((SELECT value FROM metadata WHERE key='agent.stickers.enabled'),'true')='true' ON CONFLICT(message_id) DO NOTHING")
                .bind(&message_id).bind(sticker).execute(&mut *tx).await?.rows_affected();
            if added > 0 {
                break;
            }
        }
        for (position, image) in images.iter().take(4).enumerate() {
            sqlx::query("INSERT OR IGNORE INTO message_assets(message_id,asset_id,position) SELECT ?,id,? FROM assets WHERE id=? AND session_id=?")
                .bind(&message_id).bind(position as i64).bind(image).bind(&user.session_id).execute(&mut *tx).await?;
        }
        self.schedule_extraction(&mut tx).await?;
        tx.commit().await?;
        Ok(message_id)
    }

    pub async fn fail_turn(&self, user: &Message, code: &str) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "UPDATE model_requests SET status='interrupted' WHERE turn_id=? AND status='running'",
        )
        .bind(&user.turn_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE tool_calls SET status='failed',error='turn_cancelled',completed_at=? WHERE turn_id=? AND status='running'").bind(now()).bind(&user.turn_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE turns SET status='failed',error_code=?,completed_at=? WHERE id=? AND status='generating'")
            .bind(code).bind(now()).bind(&user.turn_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE sessions SET active_turn=NULL WHERE id=? AND active_turn=?")
            .bind(&user.session_id)
            .bind(&user.turn_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Count full conversation text, then enqueue one extraction for each completed interval.
    /// A single long turn stays whole. The cursor and its job commit with the reply.
    pub(crate) async fn schedule_extraction(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
        let through: i64 = sqlx::query_scalar("SELECT COALESCE((SELECT CAST(value AS INTEGER) FROM metadata WHERE key='memory.extraction.through_seq'),0)")
            .fetch_one(&mut **tx).await?;
        let rows: Vec<(String,i64,i64)> = sqlx::query_as("SELECT u.id,u.seq,length(u.content)+length(a.content) FROM messages u JOIN turns t ON t.id=u.turn_id JOIN messages a ON a.turn_id=t.id AND a.role='assistant' WHERE u.role='user' AND t.status='complete' AND u.seq>? ORDER BY u.seq")
            .bind(through).fetch_all(&mut **tx).await?;
        let mut chars = 0i64;
        let mut sources = Vec::new();
        for (message, seq, size) in rows {
            chars += size;
            let processed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM extracted_messages WHERE message_id=?) OR EXISTS(SELECT 1 FROM jobs WHERE kind='extract' AND payload=?)")
                .bind(&message).bind(&message).fetch_one(&mut **tx).await?;
            if !processed {
                sources.push(message);
            }
            if chars >= self.extract_every_chars as i64 {
                if !sources.is_empty() {
                    enqueue(
                        tx,
                        &format!("extract:batch:{seq}"),
                        "extract",
                        &json!({"message_ids":sources}).to_string(),
                    )
                    .await?;
                }
                sqlx::query("INSERT INTO metadata(key,value) VALUES('memory.extraction.through_seq',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
                    .bind(seq.to_string()).execute(&mut **tx).await?;
                sources.clear();
                chars = 0;
            }
        }
        Ok(())
    }

    pub async fn extraction_pending_chars(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COALESCE(SUM(length(u.content)+length(a.content)),0) FROM messages u JOIN turns t ON t.id=u.turn_id JOIN messages a ON a.turn_id=t.id AND a.role='assistant' WHERE u.role='user' AND t.status='complete' AND u.seq>COALESCE((SELECT CAST(value AS INTEGER) FROM metadata WHERE key='memory.extraction.through_seq'),0)")
            .fetch_one(&self.pool).await?)
    }

    pub async fn history(&self, session: &str, turns: i64) -> Result<Vec<Message>> {
        let mut rows = sqlx::query_as::<_,Message>("SELECT m.id,m.session_id,m.turn_id,m.role,m.content,m.created_at FROM messages m JOIN turns t ON t.id=m.turn_id WHERE m.session_id=? AND t.status='complete' ORDER BY m.seq DESC LIMIT ?")
            .bind(session).bind(turns * 2).fetch_all(&self.pool).await?;
        rows.reverse();
        Ok(rows)
    }
    pub async fn message(&self, id: &str) -> Result<Message> {
        Ok(sqlx::query_as(
            "SELECT id,session_id,turn_id,role,content,created_at FROM messages WHERE id=?",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await?)
    }
    pub async fn memory(&self, id: &str) -> Result<Option<Memory>> {
        Ok(sqlx::query_as("SELECT * FROM memories WHERE id=?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }
    pub(crate) fn parsed_embedding(&self, memory: &Memory) -> Option<Arc<Vec<f32>>> {
        if memory.embedding == "[]" {
            return None;
        }
        let mut cache = self.embeddings.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((revision, serialized, vector)) = cache.get(&memory.id)
            && *revision == memory.revision
            && serialized == &memory.embedding
        {
            return Some(vector.clone());
        }
        let vector: Vec<f32> = serde_json::from_str(&memory.embedding).ok()?;
        if vector.is_empty() || !vector.iter().all(|x| x.is_finite()) {
            return None;
        }
        let vector = Arc::new(vector);
        if cache.len() >= 512 {
            // Bounded per-store cache: no unbounded growth with conversation age.
            if let Some(key) = cache.keys().next().cloned() {
                cache.remove(&key);
            }
        }
        cache.insert(
            memory.id.clone(),
            (memory.revision, memory.embedding.clone(), vector.clone()),
        );
        Some(vector)
    }
    pub(crate) fn invalidate_embedding(&self, id: &str) {
        self.embeddings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
    }
    /// Fetch a bounded candidate set in one snapshot instead of one query per hit.
    pub async fn memories_by_ids(&self, ids: &[String]) -> Result<Vec<Memory>> {
        let mut result = Vec::new();
        for chunk in ids.chunks(256) {
            let mut query =
                sqlx::QueryBuilder::<Sqlite>::new("SELECT * FROM memories WHERE id IN (");
            let mut values = query.separated(",");
            for id in chunk {
                values.push_bind(id);
            }
            values.push_unseparated(")");
            result.extend(
                query
                    .build_query_as::<Memory>()
                    .fetch_all(&self.pool)
                    .await?,
            );
        }
        Ok(result)
    }
    pub async fn active_memories(&self, limit: i64) -> Result<Vec<Memory>> {
        Ok(sqlx::query_as("SELECT * FROM memories WHERE status='active' AND (valid_until IS NULL OR valid_until>?) ORDER BY updated_at DESC,id LIMIT ?")
            .bind(now()).bind(limit).fetch_all(&self.pool).await?)
    }
    pub async fn list_memories(&self, limit: i64, offset: i64) -> Result<Vec<Memory>> {
        Ok(
            sqlx::query_as("SELECT * FROM memories ORDER BY created_at DESC,id LIMIT ? OFFSET ?")
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await?,
        )
    }
    pub async fn sources(&self, memory_id: &str) -> Result<Vec<Value>> {
        let rows = sqlx::query("SELECT s.message_id,s.quote,s.observed_at,m.session_id,m.turn_id,m.content FROM memory_sources s JOIN messages m ON m.id=s.message_id WHERE s.memory_id=? ORDER BY s.observed_at DESC LIMIT 100")
            .bind(memory_id).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|r| json!({"message_id":r.get::<String,_>("message_id"),"quote":r.get::<String,_>("quote"),"content":r.get::<String,_>("content"),"observed_at":r.get::<i64,_>("observed_at"),"session_id":r.get::<String,_>("session_id"),"turn_id":r.get::<String,_>("turn_id")})).collect())
    }

    /// All extracted facts and job acknowledgement commit together; retries cannot create versions twice.
    pub async fn apply_candidates(
        &self,
        source: &Message,
        candidates: Vec<(Candidate, Vec<f32>, Option<i64>)>,
        job_id: &str,
    ) -> Result<()> {
        let start = std::time::Instant::now();
        let result = async {
            let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
            Self::apply_candidates_in(&mut tx, source, candidates).await?;
            sqlx::query("DELETE FROM jobs WHERE id=?")
                .bind(job_id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(())
        }
        .await;
        self.memory_write_metrics.record(start, result.is_ok());
        result
    }

    pub(crate) async fn apply_batch(
        &self,
        records: BatchMemoryRecords,
        job_id: &str,
    ) -> Result<()> {
        self.apply_batch_with_retries(records, &std::collections::HashSet::new(), job_id)
            .await
    }

    pub(crate) async fn apply_batch_with_retries(
        &self,
        records: BatchMemoryRecords,
        retry_sources: &std::collections::HashSet<String>,
        job_id: &str,
    ) -> Result<()> {
        let start = std::time::Instant::now();
        let result = async {
            let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
            for (source, candidates) in records {
                if retry_sources.contains(&source.id) {
                    ensure!(candidates.is_empty(), "cannot retry committed source");
                    // No receipt for an entirely rejected source. Persist its
                    // bounded single-source retry together with this batch ack.
                    enqueue(
                        &mut tx,
                        &format!("extract:retry:{}", source.id),
                        "extract",
                        &source.id,
                    )
                    .await?;
                } else {
                    Self::apply_candidates_in(&mut tx, &source, candidates).await?;
                }
            }
            sqlx::query("DELETE FROM jobs WHERE id=?")
                .bind(job_id)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(())
        }
        .await;
        self.memory_write_metrics.record(start, result.is_ok());
        result
    }

    async fn apply_candidates_in(
        tx: &mut Transaction<'_, Sqlite>,
        source: &Message,
        candidates: Vec<(Candidate, Vec<f32>, Option<i64>)>,
    ) -> Result<()> {
        let processed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM extracted_messages WHERE message_id=?)",
        )
        .bind(&source.id)
        .fetch_one(&mut **tx)
        .await?;
        if processed {
            return Ok(());
        }
        let persisted: Message = sqlx::query_as(
            "SELECT id,session_id,turn_id,role,content,created_at FROM messages WHERE id=?",
        )
        .bind(&source.id)
        .fetch_one(&mut **tx)
        .await?;
        ensure!(
            persisted.role == "user"
                && source.role == "user"
                && persisted.content == source.content
                && persisted.session_id == source.session_id
                && persisted.turn_id == source.turn_id
                && persisted.created_at == source.created_at,
            "memory source does not match persisted user evidence"
        );
        let source_seq: i64 = sqlx::query_scalar("SELECT seq FROM messages WHERE id=?")
            .bind(&source.id)
            .fetch_one(&mut **tx)
            .await?;
        let source_qq: Option<i64> =
            sqlx::query_scalar("SELECT seq FROM qq_inbox WHERE id=? AND session_id=?")
                .bind(&source.turn_id)
                .bind(&source.session_id)
                .fetch_optional(&mut **tx)
                .await?;
        let suppressed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM suppressed_sources WHERE message_id=?)",
        )
        .bind(&source.id)
        .fetch_one(&mut **tx)
        .await?;
        if suppressed {
            sqlx::query("INSERT OR IGNORE INTO extracted_messages(message_id) VALUES(?)")
                .bind(&source.id)
                .execute(&mut **tx)
                .await?;
            return Ok(());
        }
        for (candidate, embedding, valid_until) in candidates {
            let barrier: Option<(i64, i64)> =
                sqlx::query_as("SELECT through_seq,through_qq FROM forget_barriers WHERE slot=?")
                    .bind(&candidate.key)
                    .fetch_optional(&mut **tx)
                    .await?;
            if barrier
                .is_some_and(|(seq, qq)| source_seq <= seq || source_qq.is_some_and(|s| s <= qq))
            {
                continue;
            }
            let hash = content_hash(&candidate.content);
            let content_barrier: Option<(i64, i64)> = sqlx::query_as(
                "SELECT through_seq,through_qq FROM content_barriers WHERE content_hash=?",
            )
            .bind(&hash)
            .fetch_optional(&mut **tx)
            .await?;
            if content_barrier
                .is_some_and(|(seq, qq)| source_seq <= seq || source_qq.is_some_and(|s| s <= qq))
            {
                continue;
            }
            let exact: Option<Memory> =
                sqlx::query_as("SELECT * FROM memories WHERE content_hash=? AND status='active'")
                    .bind(&hash)
                    .fetch_optional(&mut **tx)
                    .await?;
            let current: Option<Memory> =
                sqlx::query_as("SELECT * FROM memories WHERE slot=? AND status='active'")
                    .bind(&candidate.key)
                    .fetch_optional(&mut **tx)
                    .await?;
            // Late retried jobs cannot overwrite a more recently observed value of a slot.
            if current.as_ref().is_some_and(|m| m.source_seq > source_seq)
                || exact.as_ref().is_some_and(|m| m.source_seq > source_seq)
            {
                continue;
            }
            let reinforce = exact.or_else(|| {
                current
                    .as_ref()
                    .filter(|m| candidate.equivalent_to.as_deref() == Some(&m.id))
                    .cloned()
            });
            let (memory_id, revision) = if let Some(memory) = reinforce {
                // Content deduplication can point to a different slot. The slot being
                // corrected still has to lose its old value, in this same transaction.
                if let Some(old) = current.as_ref().filter(|old| old.id != memory.id) {
                    sqlx::query("UPDATE memories SET status='superseded',superseded_by=?,revision=revision+1 WHERE id=?")
                        .bind(&memory.id).bind(&old.id).execute(&mut **tx).await?;
                    enqueue(
                        tx,
                        &format!("project:{}:{}", old.id, old.revision + 1),
                        "project",
                        &old.id,
                    )
                    .await?;
                }
                sqlx::query("UPDATE memories SET importance=MAX(importance,?),confidence=MAX(confidence,?),updated_at=?,source_seq=?,valid_until=?,revision=revision+1 WHERE id=?")
                    .bind(candidate.importance).bind(candidate.confidence).bind(source.created_at).bind(source_seq).bind(valid_until).bind(&memory.id).execute(&mut **tx).await?;
                (memory.id, memory.revision + 1)
            } else {
                let new_id = id();
                if let Some(old) = current {
                    sqlx::query("UPDATE memories SET status='superseded',superseded_by=?,revision=revision+1 WHERE id=?")
                        .bind(&new_id).bind(&old.id).execute(&mut **tx).await?;
                    enqueue(
                        tx,
                        &format!("project:{}:{}", old.id, old.revision + 1),
                        "project",
                        &old.id,
                    )
                    .await?;
                }
                sqlx::query("INSERT INTO memories(id,slot,content,content_hash,kind,importance,confidence,entities,embedding,status,created_at,updated_at,source_seq,valid_until) VALUES(?,?,?,?,?,?,?,?,?,'active',?,?,?,?)")
                    .bind(&new_id).bind(&candidate.key).bind(&candidate.content).bind(hash).bind(&candidate.kind)
                    .bind(candidate.importance).bind(candidate.confidence).bind(serde_json::to_string(&candidate.entities)?)
                    .bind(serde_json::to_string(&embedding)?).bind(source.created_at).bind(source.created_at).bind(source_seq).bind(valid_until)
                    .execute(&mut **tx).await?;
                (new_id, 1)
            };
            sqlx::query("INSERT OR IGNORE INTO memory_sources(memory_id,message_id,quote,observed_at) VALUES(?,?,?,?)")
                .bind(&memory_id).bind(&source.id).bind(&candidate.source_quote).bind(source.created_at).execute(&mut **tx).await?;
            enqueue(
                tx,
                &format!("project:{memory_id}:{revision}"),
                "project",
                &memory_id,
            )
            .await?;
        }
        sqlx::query("INSERT OR IGNORE INTO extracted_messages(message_id) VALUES(?)")
            .bind(&source.id)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    pub async fn forget(&self, memory_id: &str) -> Result<bool> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        if let Some(memory) = sqlx::query_as::<_, Memory>("SELECT * FROM memories WHERE id=?")
            .bind(memory_id)
            .fetch_optional(&mut *tx)
            .await?
        {
            let seq = sqlx::query_scalar("SELECT COALESCE(MAX(seq),0) FROM messages")
                .fetch_one(&mut *tx)
                .await?;
            crate::console_store::protect(&mut tx, &memory, seq).await?;
        }
        let revision: Option<i64> = sqlx::query_scalar("UPDATE memories SET status='forgotten',revision=revision+1 WHERE id=? RETURNING revision")
            .bind(memory_id).fetch_optional(&mut *tx).await?;
        if let Some(revision) = revision {
            enqueue(
                &mut tx,
                &format!("project:{memory_id}:{revision}"),
                "project",
                memory_id,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(revision.is_some())
    }
    pub async fn touch(&self, ids: &[String]) -> Result<()> {
        for id in ids {
            sqlx::query("UPDATE memories SET last_accessed_at=?,access_count=access_count+1 WHERE id=? AND status='active'")
                .bind(now()).bind(id).execute(&self.pool).await?;
        }
        Ok(())
    }
    pub async fn claim_job(&self) -> Result<Option<Job>> {
        Ok(sqlx::query_as("UPDATE jobs SET status='running',attempts=attempts+1 WHERE id=(SELECT id FROM jobs WHERE status='pending' AND available_at<=? ORDER BY available_at,created_at,id LIMIT 1) RETURNING id,kind,payload,attempts")
            .bind(now()).fetch_optional(&self.pool).await?)
    }
    pub async fn ack_job(&self, id: &str) -> Result<()> {
        sqlx::query("DELETE FROM jobs WHERE id=?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    pub async fn retry_job(&self, job: &Job, max_attempts: i64) -> Result<()> {
        let status = if job.attempts >= max_attempts {
            "dead"
        } else {
            "pending"
        };
        let delay = 2i64.pow(job.attempts.min(10) as u32).min(900);
        sqlx::query("UPDATE jobs SET status=?,available_at=?,last_error='dependency_or_validation_failure' WHERE id=?")
            .bind(status).bind(now() + delay).bind(&job.id).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn retry_dead(&self) -> Result<u64> {
        Ok(sqlx::query("UPDATE jobs SET status='pending',attempts=0,available_at=?,last_error=NULL WHERE status='dead'")
            .bind(now()).execute(&self.pool).await?.rows_affected())
    }
    pub async fn job_stats(&self) -> Result<Value> {
        let rows = sqlx::query("SELECT status,COUNT(*) AS count FROM jobs GROUP BY status")
            .fetch_all(&self.pool)
            .await?;
        let mut value = json!({"pending":0,"running":0,"dead":0});
        for row in rows {
            value[row.get::<String, _>("status")] = json!(row.get::<i64, _>("count"));
        }
        Ok(value)
    }
    pub async fn reindex(&self) -> Result<u64> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM memories")
            .fetch_all(&mut *tx)
            .await?;
        let run = id();
        for memory in &ids {
            enqueue(
                &mut tx,
                &format!("project:{memory}:rebuild:{run}"),
                "project",
                memory,
            )
            .await?;
        }
        tx.commit().await?;
        Ok(ids.len() as u64)
    }
    pub async fn expire(&self, half_life: f64, threshold: f64) -> Result<usize> {
        // Iterate in pages so large stores do not require loading all embeddings into RAM.
        let mut cursor = String::new();
        let mut count = 0;
        loop {
            let memories: Vec<Memory> = sqlx::query_as(
                "SELECT * FROM memories WHERE status='active' AND id>? ORDER BY id LIMIT 200",
            )
            .bind(&cursor)
            .fetch_all(&self.pool)
            .await?;
            if memories.is_empty() {
                break;
            }
            for memory in memories {
                cursor = memory.id.clone();
                if !memory.available(now()) || memory.strength(now(), half_life) < threshold {
                    let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
                    let revision:Option<i64>=sqlx::query_scalar("UPDATE memories SET status='forgotten',revision=revision+1 WHERE id=? AND revision=? AND status='active' RETURNING revision")
                        .bind(&memory.id).bind(memory.revision).fetch_optional(&mut *tx).await?;
                    if let Some(revision) = revision {
                        // Automatic decay only protects evidence through this old
                        // observation. A newer queued confirmation must remain eligible.
                        let qq:i64=sqlx::query_scalar("SELECT COALESCE(MAX(q.seq),0) FROM qq_inbox q JOIN messages m ON m.turn_id=q.id WHERE m.seq<=?")
                            .bind(memory.source_seq).fetch_one(&mut *tx).await?;
                        crate::console_store::protect_through(
                            &mut tx,
                            &memory,
                            memory.source_seq,
                            qq,
                        )
                        .await?;
                        enqueue(
                            &mut tx,
                            &format!("project:{}:{revision}", memory.id),
                            "project",
                            &memory.id,
                        )
                        .await?;
                        count += 1;
                    }
                    tx.commit().await?;
                }
            }
        }
        Ok(count)
    }
}

pub(crate) async fn enqueue(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    kind: &str,
    payload: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT OR IGNORE INTO jobs(id,kind,payload,available_at,created_at) VALUES(?,?,?,?,?)",
    )
    .bind(id)
    .bind(kind)
    .bind(payload)
    .bind(now())
    .bind(now())
    .execute(&mut **tx)
    .await?;
    Ok(())
}
