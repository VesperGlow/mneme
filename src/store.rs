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
    pub(crate) extract_every_tokens: usize,
    pub(crate) extract_max_delay_seconds: u64,
    pub(crate) tokens: crate::tokens::Counter,
    pub(crate) preference_minimum_confidence: f64,
    pub(crate) preference_minimum_importance: f64,
    pub(crate) job_notify: Arc<tokio::sync::Notify>,
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
            extract_every_tokens: 2048,
            job_notify: Default::default(),
            extract_max_delay_seconds: 120,
            tokens: Default::default(),
            preference_minimum_confidence: 0.7,
            preference_minimum_importance: 0.4,
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
        sqlx::query(
            "UPDATE extraction_runs SET status='interrupted',completed_at=? WHERE status='running'",
        )
        .bind(now())
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
        self.job_notify.notify_one();
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

    /// The completion and cancellation transactions compete for the same row.
    /// Whichever commits first wins; a committed reply is never retracted.
    pub async fn cancel_turn(&self, turn: &str) -> Result<Option<(String, Option<String>)>> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let state = cancel_turn_in(&mut tx, turn).await?;
        tx.commit().await?;
        Ok(state)
    }

    pub async fn turn_cancelled(&self, turn: &str) -> Result<bool> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM turns WHERE id=? AND error_code='cancelled')",
        )
        .bind(turn)
        .fetch_one(&self.pool)
        .await?)
    }

    /// Count full conversation tokens, then enqueue completed intervals or an aged partial batch.
    /// A single long turn stays whole. The cursor and its job commit with the reply.
    pub(crate) async fn schedule_extraction(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
        let through: i64 = sqlx::query_scalar("SELECT COALESCE((SELECT CAST(value AS INTEGER) FROM metadata WHERE key='memory.extraction.through_seq'),0)")
            .fetch_one(&mut **tx).await?;
        let rows: Vec<(String,i64,String,String,i64)> = sqlx::query_as("SELECT u.id,u.seq,u.content,a.content,t.completed_at FROM messages u JOIN turns t ON t.id=u.turn_id JOIN messages a ON a.turn_id=t.id AND a.role='assistant' WHERE u.role='user' AND t.status='complete' AND u.seq>? ORDER BY u.seq")
            .bind(through).fetch_all(&mut **tx).await?;
        let mut tokens = 0usize;
        let mut sources = Vec::new();
        let mut oldest = None;
        let mut last_seq = through;
        for (message, seq, user, assistant, completed) in rows {
            tokens += self.tokens.count(&user) + self.tokens.count(&assistant);
            oldest.get_or_insert(completed);
            last_seq = seq;
            let processed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM extracted_messages WHERE message_id=?) OR EXISTS(SELECT 1 FROM jobs WHERE kind='extract' AND payload=?)")
                .bind(&message).bind(&message).fetch_one(&mut **tx).await?;
            if !processed {
                sources.push(message);
            }
            if tokens >= self.extract_every_tokens {
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
                tokens = 0;
                oldest = None;
            }
        }
        if oldest.is_some_and(|at| now() - at >= self.extract_max_delay_seconds as i64) {
            if !sources.is_empty() {
                enqueue(
                    tx,
                    &format!("extract:batch:{last_seq}"),
                    "extract",
                    &json!({"message_ids":sources}).to_string(),
                )
                .await?;
            }
            sqlx::query("INSERT INTO metadata(key,value) VALUES('memory.extraction.through_seq',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
                .bind(last_seq.to_string()).execute(&mut **tx).await?;
        }
        Ok(())
    }

    pub(crate) async fn flush_due_extraction(&self) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        self.schedule_extraction(&mut tx).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn extraction_pending_tokens(&self) -> Result<i64> {
        let rows: Vec<(String,String)> = sqlx::query_as("SELECT u.content,a.content FROM messages u JOIN turns t ON t.id=u.turn_id JOIN messages a ON a.turn_id=t.id AND a.role='assistant' WHERE u.role='user' AND t.status='complete' AND u.seq>COALESCE((SELECT CAST(value AS INTEGER) FROM metadata WHERE key='memory.extraction.through_seq'),0)")
            .fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(|(u, a)| self.tokens.count(u) + self.tokens.count(a))
            .sum::<usize>() as i64)
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
    /// Reuse already-read facts for immediate follow-ups, without another model
    /// or a new cache. Access statistics are not refreshed by this carry-over.
    pub(crate) async fn recent_recall_context(
        &self,
        session: &str,
        limit: i64,
    ) -> Result<Vec<Memory>> {
        Ok(sqlx::query_as("SELECT * FROM memories WHERE status='active' AND (valid_until IS NULL OR valid_until>?) AND last_accessed_at>=? AND last_accessed_at>=(SELECT MIN(created_at) FROM (SELECT created_at FROM turns WHERE session_id=? AND status='complete' ORDER BY created_at DESC,rowid DESC LIMIT 6)) ORDER BY last_accessed_at DESC,id LIMIT ?")
            .bind(now()).bind(now()-600).bind(session).bind(limit).fetch_all(&self.pool).await?)
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
        let rows = sqlx::query("SELECT s.message_id,s.quote,s.observed_at,s.evidence_type,s.confidence,s.valid_until,m.session_id,m.turn_id,m.content FROM memory_sources s JOIN messages m ON m.id=s.message_id WHERE s.memory_id=? ORDER BY s.observed_at DESC LIMIT 100")
            .bind(memory_id).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|r| json!({"message_id":r.get::<String,_>("message_id"),"quote":r.get::<String,_>("quote"),"evidence_type":r.get::<String,_>("evidence_type"),"confidence":r.get::<f64,_>("confidence"),"valid_until":r.get::<Option<i64>,_>("valid_until"),"content":r.get::<String,_>("content"),"observed_at":r.get::<i64,_>("observed_at"),"session_id":r.get::<String,_>("session_id"),"turn_id":r.get::<String,_>("turn_id")})).collect())
    }

    /// Audit APIs retain the originals; prompts omit sources protected by a
    /// manual edit or deletion, including when shared by another active fact.
    pub(crate) async fn prompt_sources(&self, memory_id: &str) -> Result<Vec<Value>> {
        let rows: Vec<(String, String, i64)> = sqlx::query_as("SELECT s.message_id,s.quote,s.observed_at FROM memory_sources s WHERE s.memory_id=? AND NOT EXISTS(SELECT 1 FROM suppressed_sources p WHERE p.message_id=s.message_id) ORDER BY s.observed_at DESC,s.message_id LIMIT 2")
            .bind(memory_id).fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|(id, quote, at)| json!({"message_id":id,"quote":quote,"observed_at":at}))
            .collect())
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
            self.apply_candidates_in(&mut tx, source, candidates, true, job_id)
                .await?;
            Self::finish_extraction_in(&mut tx, job_id).await?;
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

    pub(crate) async fn apply_partial(
        &self,
        source: &Message,
        candidates: MemoryRecords,
        job_id: &str,
    ) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        self.apply_candidates_in(&mut tx, source, candidates, false, job_id)
            .await?;
        tx.commit().await?;
        Ok(())
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
                    self.apply_candidates_in(&mut tx, &source, candidates, false, job_id)
                        .await?;
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
                    self.apply_candidates_in(&mut tx, &source, candidates, true, job_id)
                        .await?;
                }
            }
            Self::finish_extraction_in(&mut tx, job_id).await?;
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
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        source: &Message,
        candidates: Vec<(Candidate, Vec<f32>, Option<i64>)>,
        mark_processed: bool,
        job_id: &str,
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
        'candidates: for (mut candidate, embedding, mut valid_until) in candidates {
            crate::memory::prepare_candidate(&mut candidate, source, &mut valid_until);
            if let Some(slot) = sqlx::query_scalar::<_, String>(
                "SELECT slot FROM memory_slot_aliases WHERE alias=?",
            )
            .bind(&candidate.key)
            .fetch_optional(&mut **tx)
            .await?
            {
                candidate.key = slot;
            }
            candidate.key = candidate.storage_key();
            let mut superseded = Vec::new();
            if candidate.relation == FactRelation::Replace {
                for target in candidate.supersedes.iter().take(8) {
                    let (column, value) = target
                        .strip_prefix("slot:")
                        .map(|s| ("slot", s))
                        .unwrap_or(("id", target));
                    if let Some(old) = sqlx::query_as::<_, Memory>(&format!(
                        "SELECT * FROM memories WHERE {column}=? ORDER BY source_seq DESC LIMIT 1"
                    ))
                    .bind(value)
                    .fetch_optional(&mut **tx)
                    .await?
                    {
                        let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM forget_barriers WHERE slot=? AND (through_seq>=? OR (? IS NOT NULL AND through_qq>=?)))")
                            .bind(&old.slot).bind(source_seq).bind(source_qq).bind(source_qq).fetch_one(&mut **tx).await?;
                        if blocked || old.source_seq > source_seq {
                            Self::decision_in(
                                tx,
                                job_id,
                                source,
                                &candidate,
                                "rejected",
                                "barrier_or_newer_evidence",
                                None,
                            )
                            .await?;
                            continue 'candidates;
                        }
                        if old.status != "active" {
                            if let Some(next) = old.superseded_by.as_deref() {
                                let later: Option<i64> = sqlx::query_scalar(
                                    "SELECT source_seq FROM memories WHERE id=?",
                                )
                                .bind(next)
                                .fetch_optional(&mut **tx)
                                .await?;
                                if later.is_some_and(|seq| seq > source_seq) {
                                    Self::decision_in(
                                        tx,
                                        job_id,
                                        source,
                                        &candidate,
                                        "rejected",
                                        "newer_evidence",
                                        None,
                                    )
                                    .await?;
                                    continue 'candidates;
                                }
                            }
                            anyhow::bail!(
                                "reconciliation target changed; retry against current facts"
                            );
                        }
                        superseded.push(old);
                    } else {
                        Self::decision_in(
                            tx,
                            job_id,
                            source,
                            &candidate,
                            "rejected",
                            "missing_reconciliation_target",
                            None,
                        )
                        .await?;
                        continue 'candidates;
                    }
                }
            }
            let barrier: Option<(i64, i64)> =
                sqlx::query_as("SELECT through_seq,through_qq FROM forget_barriers WHERE slot=?")
                    .bind(&candidate.key)
                    .fetch_optional(&mut **tx)
                    .await?;
            if barrier
                .is_some_and(|(seq, qq)| source_seq <= seq || source_qq.is_some_and(|s| s <= qq))
            {
                Self::decision_in(
                    tx,
                    job_id,
                    source,
                    &candidate,
                    "rejected",
                    "forget_barrier",
                    None,
                )
                .await?;
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
                Self::decision_in(
                    tx,
                    job_id,
                    source,
                    &candidate,
                    "rejected",
                    "content_barrier",
                    None,
                )
                .await?;
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
                Self::decision_in(
                    tx,
                    job_id,
                    source,
                    &candidate,
                    "rejected",
                    "newer_evidence",
                    None,
                )
                .await?;
                continue;
            }
            let reinforce = exact.or_else(|| {
                current
                    .as_ref()
                    .filter(|m| candidate.equivalent_to.as_deref() == Some(&m.id))
                    .cloned()
            });
            let reinforced = reinforce.is_some();
            let (memory_id, revision) = if let Some(memory) = reinforce {
                if candidate.key != memory.slot {
                    // Flatten aliases when two formerly independent slots merge.
                    sqlx::query("UPDATE memory_slot_aliases SET slot=? WHERE slot=?")
                        .bind(&memory.slot)
                        .bind(&candidate.key)
                        .execute(&mut **tx)
                        .await?;
                    sqlx::query("INSERT INTO memory_slot_aliases(alias,slot) VALUES(?,?) ON CONFLICT(alias) DO UPDATE SET slot=excluded.slot")
                        .bind(&candidate.key).bind(&memory.slot).execute(&mut **tx).await?;
                }
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
                let already_observed: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM memory_sources WHERE memory_id=? AND message_id=?)")
                    .bind(&memory.id).bind(&source.id).fetch_one(&mut **tx).await?;
                if already_observed {
                    Self::decision_in(
                        tx,
                        job_id,
                        source,
                        &candidate,
                        "stored",
                        "already_observed",
                        Some(&memory.id),
                    )
                    .await?;
                    continue;
                }
                let mut entities = candidate.entities.clone();
                for entity in serde_json::from_str::<Vec<String>>(&memory.entities)? {
                    if entities.len() < 8 && !entities.contains(&entity) {
                        entities.push(entity);
                    }
                }
                if memory.tier == "long_term" && candidate.kind == "preference" {
                    // A further confirmation must not reintroduce the temporary
                    // TTL applied to a new preference candidate.
                    valid_until = candidate
                        .valid_until
                        .as_ref()
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|d| d.timestamp())
                        .or(memory.valid_until);
                }
                sqlx::query("UPDATE memories SET importance=MAX(importance,?),confidence=MAX(confidence,?),updated_at=?,source_seq=?,valid_until=?,entities=?,revision=revision+1 WHERE id=?")
                    .bind(candidate.importance).bind(candidate.confidence).bind(source.created_at).bind(source_seq).bind(valid_until.or(memory.valid_until.filter(|end|*end > source.created_at))).bind(serde_json::to_string(&entities)?).bind(&memory.id).execute(&mut **tx).await?;
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
                sqlx::query("INSERT INTO memories(id,slot,content,content_hash,kind,importance,confidence,entities,embedding,status,created_at,updated_at,source_seq,valid_until,tier,evidence_type) VALUES(?,?,?,?,?,?,?,?,?,'active',?,?,?,?,?,?)")
                    .bind(&new_id).bind(&candidate.key).bind(&candidate.content).bind(hash).bind(&candidate.kind)
                    .bind(candidate.importance).bind(candidate.confidence).bind(serde_json::to_string(&candidate.entities)?)
                    .bind(serde_json::to_string(&embedding)?).bind(source.created_at).bind(source.created_at).bind(source_seq).bind(valid_until).bind(&candidate.tier).bind(&candidate.evidence_type)
                    .execute(&mut **tx).await?;
                (new_id, 1)
            };
            for old in superseded.into_iter().filter(|m| m.id != memory_id) {
                let changed = sqlx::query("UPDATE memories SET status='superseded',superseded_by=?,revision=revision+1 WHERE id=? AND status='active' AND revision=?")
                    .bind(&memory_id).bind(&old.id).bind(old.revision).execute(&mut **tx).await?.rows_affected();
                if changed > 0 {
                    enqueue(
                        tx,
                        &format!("project:{}:{}", old.id, old.revision + 1),
                        "project",
                        &old.id,
                    )
                    .await?;
                }
            }
            sqlx::query("INSERT OR IGNORE INTO memory_sources(memory_id,message_id,quote,observed_at,evidence_type,confidence,valid_until) VALUES(?,?,?,?,?,?,?)")
                .bind(&memory_id).bind(&source.id).bind(&candidate.source_quote).bind(source.created_at).bind(&candidate.evidence_type).bind(candidate.confidence).bind(candidate.valid_until.as_ref().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp())).execute(&mut **tx).await?;
            let mut revision = revision;
            let confirmations: i64 = if candidate.kind == "preference"
                && candidate.evidence_type == "explicit_fact"
                && candidate.importance >= self.preference_minimum_importance
            {
                sqlx::query_scalar("SELECT COUNT(DISTINCT m.turn_id) FROM memory_sources s JOIN messages m ON m.id=s.message_id WHERE s.memory_id=? AND s.evidence_type='explicit_fact' AND s.confidence>=? AND NOT EXISTS(SELECT 1 FROM suppressed_sources p WHERE p.message_id=s.message_id)")
                    .bind(&memory_id).bind(self.preference_minimum_confidence).fetch_one(&mut **tx).await?
            } else {
                0
            };
            if confirmations >= 2 {
                // Promotion only strengthens the repeated, explicit proposition.
                // It never generalizes a game comment into a taste or personality.
                let promoted: Option<i64> = sqlx::query_scalar("UPDATE memories SET tier='long_term',valid_until=?,revision=revision+1 WHERE id=? AND tier='episodic' RETURNING revision")
                    .bind(candidate.valid_until.as_ref().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.timestamp()))
                    .bind(&memory_id).fetch_optional(&mut **tx).await?;
                if let Some(next) = promoted {
                    revision = next;
                }
            }
            Self::decision_in(
                tx,
                job_id,
                source,
                &candidate,
                "stored",
                if confirmations >= 2 {
                    "preference_confirmed"
                } else if reinforced {
                    "reinforced"
                } else {
                    "created"
                },
                Some(&memory_id),
            )
            .await?;
            enqueue(
                tx,
                &format!("project:{memory_id}:{revision}"),
                "project",
                &memory_id,
            )
            .await?;
        }
        Self::trim_episodes_in(tx).await?;
        if mark_processed {
            sqlx::query("INSERT OR IGNORE INTO extracted_messages(message_id) VALUES(?)")
                .bind(&source.id)
                .execute(&mut **tx)
                .await?;
        }
        Ok(())
    }

    async fn trim_episodes_in(tx: &mut Transaction<'_, Sqlite>) -> Result<usize> {
        let overflow: Vec<(String, i64)> = sqlx::query_as("SELECT id,revision FROM memories WHERE tier='episodic' AND status='active' ORDER BY updated_at DESC,source_seq DESC,id LIMIT -1 OFFSET 256")
            .fetch_all(&mut **tx).await?;
        let count = overflow.len();
        for (memory_id, revision) in overflow {
            sqlx::query("UPDATE memories SET status='forgotten',revision=revision+1 WHERE id=?")
                .bind(&memory_id)
                .execute(&mut **tx)
                .await?;
            enqueue(
                tx,
                &format!("project:{memory_id}:{}", revision + 1),
                "project",
                &memory_id,
            )
            .await?;
        }
        Ok(count)
    }

    /// Hourly evidence consolidation uses persisted, independent source turns.
    /// It never guesses a preference from a similar topic or an inferred claim.
    pub async fn consolidate_memories(&self) -> Result<Value> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let candidates: Vec<Memory> = sqlx::query_as("SELECT * FROM memories WHERE tier='episodic' AND kind='preference' AND evidence_type='explicit_fact' AND status='active' AND deleted_at IS NULL AND (valid_until IS NULL OR valid_until>?) AND importance>=? AND confidence>=?")
            .bind(now()).bind(self.preference_minimum_importance).bind(self.preference_minimum_confidence).fetch_all(&mut *tx).await?;
        let mut promoted = 0;
        for memory in candidates {
            let evidence: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT m.turn_id) FROM memory_sources s JOIN messages m ON m.id=s.message_id WHERE s.memory_id=? AND s.evidence_type='explicit_fact' AND s.confidence>=? AND NOT EXISTS(SELECT 1 FROM suppressed_sources p WHERE p.message_id=s.message_id)")
                .bind(&memory.id).bind(self.preference_minimum_confidence).fetch_one(&mut *tx).await?;
            if evidence < 2 {
                continue;
            }
            // Keep an explicitly declared deadline; discard only our generated TTL.
            let until: Option<i64> = sqlx::query_scalar("SELECT s.valid_until FROM memory_sources s JOIN messages m ON m.id=s.message_id WHERE s.memory_id=? ORDER BY m.seq DESC LIMIT 1")
                .bind(&memory.id).fetch_optional(&mut *tx).await?.flatten();
            sqlx::query(
                "UPDATE memories SET tier='long_term',valid_until=?,revision=revision+1 WHERE id=?",
            )
            .bind(until)
            .bind(&memory.id)
            .execute(&mut *tx)
            .await?;
            enqueue(
                &mut tx,
                &format!("project:{}:{}", memory.id, memory.revision + 1),
                "project",
                &memory.id,
            )
            .await?;
            promoted += 1;
        }
        let retired = Self::trim_episodes_in(&mut tx).await?;
        let result = json!({"at":now(),"promoted":promoted,"retired":retired});
        sqlx::query("INSERT INTO metadata(key,value) VALUES('memory.consolidation',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(result.to_string()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(result)
    }

    pub(crate) async fn begin_extraction(&self, job: &Job) -> Result<()> {
        let ids: Vec<String> = if job.payload.starts_with('{') {
            serde_json::from_str::<Value>(&job.payload)?["message_ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        } else {
            vec![job.payload.clone()]
        };
        let mut tokens = 0;
        for id in &ids {
            let texts: Option<(String, String)> = sqlx::query_as("SELECT u.content,a.content FROM messages u JOIN messages a ON a.turn_id=u.turn_id AND a.role='assistant' WHERE u.id=?")
                .bind(id).fetch_optional(&self.pool).await?;
            if let Some((user, assistant)) = texts {
                tokens += self.tokens.count(&user) + self.tokens.count(&assistant);
            }
        }
        // Queue retries can reset attempts after a dead-letter retry. Audit
        // attempts stay monotonic so the old failure is never overwritten.
        sqlx::query("INSERT INTO extraction_runs(job_id,attempt,source_count,input_tokens) SELECT ?,COALESCE(MAX(attempt),0)+1,?,? FROM extraction_runs WHERE job_id=?")
            .bind(&job.id).bind(ids.len() as i64).bind(tokens as i64).bind(&job.id).execute(&self.pool).await?;
        Ok(())
    }

    // A committed extraction receipt and its successful history entry share
    // the same transaction, including across a crash before the worker returns.
    async fn finish_extraction_in(tx: &mut Transaction<'_, Sqlite>, job_id: &str) -> Result<()> {
        sqlx::query("UPDATE extraction_runs SET status='complete',completed_at=? WHERE job_id=? AND status='running'")
            .bind(now()).bind(job_id).execute(&mut **tx).await?;
        Ok(())
    }

    pub(crate) async fn finish_extraction(&self, job: &Job) -> Result<()> {
        let state: Option<String> = sqlx::query_scalar("SELECT status FROM jobs WHERE id=?")
            .bind(&job.id)
            .fetch_optional(&self.pool)
            .await?;
        sqlx::query(
            "UPDATE extraction_runs SET status=?,completed_at=? WHERE job_id=? AND status='running'",
        )
        .bind(state.as_deref().unwrap_or("complete"))
        .bind(now())
        .bind(&job.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn decision_in(
        tx: &mut Transaction<'_, Sqlite>,
        job: &str,
        source: &Message,
        candidate: &Candidate,
        decision: &str,
        reason: &str,
        memory: Option<&str>,
    ) -> Result<()> {
        sqlx::query("INSERT INTO extraction_events(job_id,message_id,candidate,decision,reason,memory_id) VALUES(?,?,?,?,?,?)")
            .bind(job).bind(&source.id).bind(serde_json::to_string(candidate)?).bind(decision).bind(reason).bind(memory).execute(&mut **tx).await?;
        Ok(())
    }

    pub(crate) async fn extraction_event(
        &self,
        job: &str,
        source: Option<&str>,
        candidate: Option<Value>,
        decision: &str,
        reason: &str,
    ) -> Result<()> {
        sqlx::query("INSERT INTO extraction_events(job_id,message_id,candidate,decision,reason) VALUES(?,?,?,?,?)")
            .bind(job).bind(source).bind(candidate.map(|v| v.to_string())).bind(decision).bind(reason).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn extraction_diagnostics(&self) -> Result<Value> {
        let rows = sqlx::query("SELECT e.*,m.status,m.tier,m.revision,m.indexed_revision FROM extraction_events e LEFT JOIN memories m ON m.id=e.memory_id ORDER BY e.seq DESC LIMIT 100")
            .fetch_all(&self.pool).await?;
        let events: Vec<_> = rows.into_iter().map(|r| json!({
            "seq":r.get::<i64,_>("seq"),"job_id":r.get::<String,_>("job_id"),"message_id":r.get::<Option<String>,_>("message_id"),
            "candidate":r.get::<Option<String>,_>("candidate").and_then(|s| serde_json::from_str::<Value>(&s).ok()),
            "decision":r.get::<String,_>("decision"),"reason":r.get::<String,_>("reason"),"memory_id":r.get::<Option<String>,_>("memory_id"),"created_at":r.get::<i64,_>("created_at"),
            "storage_status":r.get::<Option<String>,_>("status"),"stored_tier":r.get::<Option<String>,_>("tier"),"revision":r.get::<Option<i64>,_>("revision"),"indexed_revision":r.get::<Option<i64>,_>("indexed_revision")
        })).collect();
        let queued = sqlx::query("SELECT id,kind,status,attempts,available_at,last_error FROM jobs ORDER BY created_at DESC,id LIMIT 50")
            .fetch_all(&self.pool).await?;
        let jobs: Vec<_> = queued.into_iter().map(|r| json!({"id":r.get::<String,_>("id"),"kind":r.get::<String,_>("kind"),"status":r.get::<String,_>("status"),"attempts":r.get::<i64,_>("attempts"),"available_at":r.get::<i64,_>("available_at"),"last_error":r.get::<Option<String>,_>("last_error")})).collect();
        let runs: Vec<Value> = sqlx::query_scalar::<_, String>("SELECT json_object('job_id',job_id,'attempt',attempt,'status',status,'source_count',source_count,'input_tokens',input_tokens,'candidate_count',candidate_count,'stored_count',stored_count,'rejected_count',rejected_count,'started_at',started_at,'completed_at',completed_at) FROM extraction_runs ORDER BY seq DESC LIMIT 50")
            .fetch_all(&self.pool).await?.into_iter().map(|s| serde_json::from_str(&s)).collect::<std::result::Result<_,_>>()?;
        let counts: Vec<(String,i64)> = sqlx::query_as("SELECT tier,COUNT(*) FROM memories WHERE status='active' AND deleted_at IS NULL AND (valid_until IS NULL OR valid_until>?) GROUP BY tier")
            .bind(now()).fetch_all(&self.pool).await?;
        let consolidation: Option<String> =
            sqlx::query_scalar("SELECT value FROM metadata WHERE key='memory.consolidation'")
                .fetch_optional(&self.pool)
                .await?;
        Ok(
            json!({"events":events,"jobs":jobs,"runs":runs,"counts":counts.into_iter().collect::<std::collections::BTreeMap<_,_>>(),"consolidation":consolidation.and_then(|s| serde_json::from_str::<Value>(&s).ok()),"pending_tokens":self.extraction_pending_tokens().await?,"extract_every_tokens":self.extract_every_tokens,"extract_max_delay_seconds":self.extract_max_delay_seconds}),
        )
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
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        Self::finish_extraction_in(&mut tx, id).await?;
        sqlx::query("DELETE FROM jobs WHERE id=?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
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

pub(crate) async fn cancel_turn_in(
    tx: &mut Transaction<'_, Sqlite>,
    turn: &str,
) -> Result<Option<(String, Option<String>)>> {
    let session: Option<String> = sqlx::query_scalar("UPDATE turns SET status='failed',error_code='cancelled',completed_at=? WHERE id=? AND status='generating' RETURNING session_id")
            .bind(now()).bind(turn).fetch_optional(&mut **tx).await?;
    if let Some(session) = session {
        sqlx::query(
            "UPDATE model_requests SET status='interrupted' WHERE turn_id=? AND status='running'",
        )
        .bind(turn)
        .execute(&mut **tx)
        .await?;
        sqlx::query("UPDATE tool_calls SET status='failed',error='turn_cancelled',completed_at=? WHERE turn_id=? AND status='running'")
                .bind(now()).bind(turn).execute(&mut **tx).await?;
        sqlx::query("UPDATE sessions SET active_turn=NULL WHERE id=? AND active_turn=?")
            .bind(session)
            .bind(turn)
            .execute(&mut **tx)
            .await?;
        sqlx::query("UPDATE qq_inbox SET status='dead',last_error='cancelled' WHERE id=? AND status IN ('pending','running')")
                .bind(turn).execute(&mut **tx).await?;
    }
    let state = sqlx::query_as("SELECT status,error_code FROM turns WHERE id=?")
        .bind(turn)
        .fetch_optional(&mut **tx)
        .await?;
    Ok(state)
}
