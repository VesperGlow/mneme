use crate::{domain::*, store::Store};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{QueryBuilder, Row, Sqlite};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryFilter {
    pub q: Option<String>,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub sort: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryWrite {
    pub content: String,
    pub kind: String,
    pub importance: f64,
    pub confidence: f64,
    #[serde(default)]
    pub entities: Vec<String>,
    pub valid_until: Option<i64>,
    pub expected_revision: Option<i64>,
}
#[derive(Serialize)]
pub struct MemoryPage {
    pub memories: Vec<Memory>,
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}
pub enum WriteResult {
    Saved(Box<Memory>),
    Missing,
    Conflict,
    Duplicate,
}

#[derive(Serialize, sqlx::FromRow)]
pub struct TranscriptMessage {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub message: Message,
    pub channel: String,
    pub status: String,
}

impl MemoryWrite {
    pub fn valid(&self) -> bool {
        !self.content.trim().is_empty()
            && self.content.chars().count() <= 500
            && matches!(
                self.kind.as_str(),
                "fact" | "preference" | "event" | "relationship" | "emotion"
            )
            && self.importance.is_finite()
            && (0.0..=1.0).contains(&self.importance)
            && self.confidence.is_finite()
            && (0.0..=1.0).contains(&self.confidence)
            && self.entities.len() <= 8
            && self
                .entities
                .iter()
                .all(|e| !e.trim().is_empty() && e.chars().count() <= 60)
            && self
                .valid_until
                .is_none_or(|t| t > 0 && chrono::DateTime::from_timestamp(t, 0).is_some())
    }
}

impl Store {
    /// The management transcript includes all transports, including archived QQ conversations.
    /// Generation history remains scoped to its own conversation and complete turns.
    pub async fn console_history(&self) -> Result<Vec<TranscriptMessage>> {
        let mut messages = sqlx::query_as("SELECT m.id,m.session_id,m.turn_id,m.role,m.content,m.created_at,t.status,t.channel FROM messages m JOIN turns t ON t.id=m.turn_id ORDER BY m.seq DESC LIMIT 200")
            .fetch_all(&self.pool).await?;
        messages.reverse();
        Ok(messages)
    }

    pub async fn memory_page(&self, f: &MemoryFilter) -> Result<MemoryPage> {
        let limit = f.limit.unwrap_or(30).clamp(1, 100);
        let offset = f.offset.unwrap_or(0).max(0);
        let mut count = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM memories WHERE ");
        filter(&mut count, f);
        let total: i64 = count.build_query_scalar().fetch_one(&self.pool).await?;
        let mut query = QueryBuilder::<Sqlite>::new("SELECT * FROM memories WHERE ");
        filter(&mut query, f);
        let order = match f.sort.as_deref() {
            Some("oldest") => "created_at ASC,id",
            Some("importance") => "importance DESC,updated_at DESC,id",
            Some("confidence") => "confidence DESC,updated_at DESC,id",
            Some("created") => "created_at DESC,id",
            _ => "updated_at DESC,id",
        };
        query
            .push(" ORDER BY ")
            .push(order)
            .push(" LIMIT ")
            .push_bind(limit)
            .push(" OFFSET ")
            .push_bind(offset);
        let memories = query.build_query_as().fetch_all(&self.pool).await?;
        Ok(MemoryPage {
            memories,
            total,
            limit,
            offset,
        })
    }

    pub async fn console_stats(&self) -> Result<Value> {
        let mut tx = self.pool.begin().await?;
        let at = now();
        let row=sqlx::query("SELECT COUNT(*) FILTER(WHERE deleted_at IS NULL) AS total, COUNT(*) FILTER(WHERE deleted_at IS NULL AND status='active' AND (valid_until IS NULL OR valid_until>?1)) AS active, COUNT(*) FILTER(WHERE deleted_at IS NOT NULL) AS deleted, COUNT(*) FILTER(WHERE deleted_at IS NULL AND created_at>=?2) AS recent FROM memories")
            .bind(at).bind(at-7*86400).fetch_one(&mut *tx).await?;
        let total: i64 = row.get("total");
        let active: i64 = row.get("active");
        let mut kinds = json!({"fact":0,"preference":0,"event":0,"relationship":0,"emotion":0});
        for row in sqlx::query(
            "SELECT kind,COUNT(*) AS count FROM memories WHERE deleted_at IS NULL GROUP BY kind",
        )
        .fetch_all(&mut *tx)
        .await?
        {
            kinds[row.get::<String, _>("kind")] = json!(row.get::<i64, _>("count"));
        }
        let start = at - at.rem_euclid(86400) - 6 * 86400;
        let mut daily = [0i64; 7];
        for row in sqlx::query("SELECT (created_at-?1)/86400 AS bucket,COUNT(*) AS count FROM memories WHERE deleted_at IS NULL AND created_at>=?1 AND created_at<?2 GROUP BY bucket")
            .bind(start).bind(start+7*86400).fetch_all(&mut *tx).await? {
            daily[row.get::<i64,_>("bucket") as usize]=row.get("count");
        }
        let activity: Vec<Value> = daily
            .into_iter()
            .enumerate()
            .map(|(i, count)| json!({"day":start+i as i64*86400,"count":count}))
            .collect();
        tx.commit().await?;
        Ok(
            json!({"total":total,"active":active,"invalid":total-active,"deleted":row.get::<i64,_>("deleted"),"recent_7_days":row.get::<i64,_>("recent"),"kinds":kinds,"activity":activity,"as_of":at,"activity_timezone":"UTC"}),
        )
    }

    pub async fn manual_memory(
        &self,
        memory_id: Option<&str>,
        input: &MemoryWrite,
    ) -> Result<WriteResult> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let current: Option<Memory> = if let Some(id) = memory_id {
            sqlx::query_as("SELECT * FROM memories WHERE id=?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?
        } else {
            None
        };
        if memory_id.is_some() && current.is_none() {
            return Ok(WriteResult::Missing);
        }
        if let Some(ref m) = current
            && (m.status != "active"
                || m.deleted_at.is_some()
                || input.expected_revision != Some(m.revision))
        {
            return Ok(WriteResult::Conflict);
        }
        let hash = content_hash(input.content.trim());
        let duplicate: Option<String> =
            sqlx::query_scalar("SELECT id FROM memories WHERE content_hash=? AND status='active'")
                .bind(&hash)
                .fetch_optional(&mut *tx)
                .await?;
        if duplicate.as_deref().is_some_and(|id| Some(id) != memory_id) {
            return Ok(WriteResult::Duplicate);
        }
        let at = now();
        let seq: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq),0) FROM messages")
            .fetch_one(&mut *tx)
            .await?;
        let entities: Vec<String> = input
            .entities
            .iter()
            .map(|e| e.trim().to_owned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let saved_id = if let Some(m) = current {
            protect(&mut tx, &m, seq).await?;
            let embedding = if m.content.trim() == input.content.trim() {
                m.embedding
            } else {
                "[]".into()
            };
            sqlx::query("UPDATE memories SET content=?,content_hash=?,kind=?,importance=?,confidence=?,entities=?,embedding=?,valid_until=?,origin='manual',updated_at=?,source_seq=?,revision=revision+1 WHERE id=?")
                .bind(input.content.trim()).bind(&hash).bind(&input.kind).bind(input.importance).bind(input.confidence)
                .bind(serde_json::to_string(&entities)?).bind(embedding).bind(input.valid_until).bind(at).bind(seq).bind(&m.id).execute(&mut *tx).await?;
            m.id
        } else {
            let new_id = id();
            sqlx::query("INSERT INTO memories(id,slot,content,content_hash,kind,importance,confidence,entities,embedding,status,created_at,updated_at,source_seq,valid_until,origin) VALUES(?,?,?,?,?,?,?,?,'[]','active',?,?,?,?,'manual')")
                .bind(&new_id).bind(format!("console.{new_id}")).bind(input.content.trim()).bind(&hash).bind(&input.kind)
                .bind(input.importance).bind(input.confidence).bind(serde_json::to_string(&entities)?).bind(at).bind(at).bind(seq).bind(input.valid_until).execute(&mut *tx).await?;
            new_id
        };
        sqlx::query("INSERT INTO content_barriers(content_hash,through_seq,through_qq) VALUES(?,?,(SELECT COALESCE(MAX(seq),0) FROM qq_inbox)) ON CONFLICT(content_hash) DO UPDATE SET through_seq=MAX(through_seq,excluded.through_seq),through_qq=MAX(through_qq,excluded.through_qq)")
            .bind(hash).bind(seq).execute(&mut *tx).await?;
        let saved: Memory = sqlx::query_as("SELECT * FROM memories WHERE id=?")
            .bind(&saved_id)
            .fetch_one(&mut *tx)
            .await?;
        super::store::enqueue(
            &mut tx,
            &format!("project:{}:{}", saved.id, saved.revision),
            "project",
            &saved.id,
        )
        .await?;
        tx.commit().await?;
        Ok(WriteResult::Saved(Box::new(saved)))
    }

    pub async fn delete_memory(&self, id: &str, expected: Option<i64>) -> Result<WriteResult> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let Some(memory): Option<Memory> = sqlx::query_as("SELECT * FROM memories WHERE id=?")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
        else {
            return Ok(WriteResult::Missing);
        };
        if expected.is_some_and(|rev| rev != memory.revision) {
            return Ok(WriteResult::Conflict);
        }
        if memory.deleted_at.is_none() {
            let seq: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq),0) FROM messages")
                .fetch_one(&mut *tx)
                .await?;
            protect(&mut tx, &memory, seq).await?;
            sqlx::query("UPDATE memories SET status='forgotten',deleted_at=?,updated_at=?,revision=revision+1 WHERE id=?")
                .bind(now()).bind(now()).bind(id).execute(&mut *tx).await?;
            super::store::enqueue(
                &mut tx,
                &format!("project:{id}:{}", memory.revision + 1),
                "project",
                id,
            )
            .await?;
        }
        let saved = sqlx::query_as("SELECT * FROM memories WHERE id=?")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(WriteResult::Saved(Box::new(saved)))
    }

    pub async fn memory_versions(&self, id: &str) -> Result<Vec<Value>> {
        let rows=sqlx::query("SELECT r.seq,r.memory_id,r.revision,r.recorded_at,r.snapshot FROM memory_revisions r JOIN memories m ON m.id=r.memory_id WHERE m.slot=(SELECT slot FROM memories WHERE id=?) ORDER BY r.seq DESC LIMIT 100")
            .bind(id).fetch_all(&self.pool).await?;
        rows.into_iter().map(|r|Ok(json!({"sequence":r.get::<i64,_>("seq"),"memory_id":r.get::<String,_>("memory_id"),"revision":r.get::<i64,_>("revision"),"recorded_at":r.get::<i64,_>("recorded_at"),"snapshot":serde_json::from_str::<Value>(&r.get::<String,_>("snapshot"))?}))).collect()
    }

    pub async fn save_embedding(&self, id: &str, revision: i64, embedding: &[f32]) -> Result<bool> {
        Ok(sqlx::query("UPDATE memories SET embedding=? WHERE id=? AND revision=? AND status='active' AND deleted_at IS NULL AND (valid_until IS NULL OR valid_until>?)")
            .bind(serde_json::to_string(embedding)?).bind(id).bind(revision).bind(now()).execute(&self.pool).await?.rows_affected()==1)
    }
    pub async fn mark_indexed(&self, id: &str, revision: i64) -> Result<()> {
        sqlx::query("UPDATE memories SET indexed_revision=? WHERE id=? AND revision=?")
            .bind(revision)
            .bind(id)
            .bind(revision)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

fn filter<'a>(q: &mut QueryBuilder<'a, Sqlite>, f: &'a MemoryFilter) {
    match f.status.as_deref() {
        Some("deleted") => {
            q.push("deleted_at IS NOT NULL");
        }
        _ => {
            q.push("deleted_at IS NULL");
        }
    }
    match f.status.as_deref() {
        Some("active") => {
            q.push(" AND status='active' AND (valid_until IS NULL OR valid_until>")
                .push_bind(now())
                .push(")");
        }
        Some("invalid") => {
            q.push(" AND (status!='active' OR valid_until<=")
                .push_bind(now())
                .push(")");
        }
        Some("expired") => {
            q.push(" AND status='active' AND valid_until<=")
                .push_bind(now());
        }
        Some("forgotten" | "superseded") => {
            q.push(" AND status=").push_bind(f.status.as_ref().unwrap());
        }
        _ => {}
    }
    if let Some(kind) = f.kind.as_ref().filter(|s| !s.is_empty()) {
        q.push(" AND kind=").push_bind(kind);
    }
    if let Some(text) = f.q.as_ref().filter(|s| !s.is_empty()) {
        q.push(" AND (instr(lower(content),lower(")
            .push_bind(text)
            .push("))>0 OR instr(lower(slot),lower(")
            .push_bind(text)
            .push("))>0 OR instr(lower(entities),lower(")
            .push_bind(text)
            .push("))>0)");
    }
}

async fn protect(tx: &mut sqlx::Transaction<'_, Sqlite>, memory: &Memory, seq: i64) -> Result<()> {
    sqlx::query("INSERT INTO forget_barriers(slot,through_seq,through_qq) VALUES(?,?,(SELECT COALESCE(MAX(seq),0) FROM qq_inbox)) ON CONFLICT(slot) DO UPDATE SET through_seq=MAX(through_seq,excluded.through_seq),through_qq=MAX(through_qq,excluded.through_qq)")
        .bind(&memory.slot).bind(seq).execute(&mut **tx).await?;
    sqlx::query("INSERT OR IGNORE INTO suppressed_sources(message_id) SELECT s.message_id FROM memory_sources s JOIN memories m ON m.id=s.memory_id WHERE m.slot=?")
        .bind(&memory.slot).execute(&mut **tx).await?;
    // All historic wordings are protected, not only the currently visible content.
    let snapshots: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT json_extract(r.snapshot,'$.content') FROM memory_revisions r JOIN memories m ON m.id=r.memory_id WHERE m.slot=?",
    )
    .bind(&memory.slot)
    .fetch_all(&mut **tx)
    .await?;
    for text in snapshots
        .into_iter()
        .chain(std::iter::once(memory.content.clone()))
    {
        sqlx::query("INSERT INTO content_barriers(content_hash,through_seq,through_qq) VALUES(?,?,(SELECT COALESCE(MAX(seq),0) FROM qq_inbox)) ON CONFLICT(content_hash) DO UPDATE SET through_seq=MAX(through_seq,excluded.through_seq),through_qq=MAX(through_qq,excluded.through_qq)")
            .bind(content_hash(&text)).bind(seq).execute(&mut **tx).await?;
    }
    Ok(())
}
