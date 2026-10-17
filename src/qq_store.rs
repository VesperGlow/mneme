use crate::{
    domain::{id, now},
    store::Store,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::FromRow;

#[derive(FromRow)]
pub struct Delivery {
    pub id: String,
    pub message_id: String,
    pub user_openid: String,
    pub content: String,
    pub session_id: String,
    pub reply: Option<String>,
    pub next_chunk: i64,
    pub attempts: i64,
    pub available_at: i64,
    pub expires_at: i64,
}

pub struct Incoming {
    pub message_id: String,
    pub sender: String,
    pub content: String,
    pub expires_at: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Acceptance {
    Queued,
    Duplicate,
    OtherAccount,
}

pub fn delivery_id(app_id: &str, message_id: &str) -> String {
    let digest = Sha256::digest(format!("qq:{app_id}:{message_id}"));
    let mut bytes: [u8; 16] = digest[..16].try_into().unwrap();
    bytes[6] = (bytes[6] & 15) | 128;
    bytes[8] = (bytes[8] & 63) | 128;
    uuid::Uuid::from_bytes(bytes).to_string()
}
fn key(app_id: &str, field: &str) -> String {
    format!("qq.{app_id}.{field}")
}

impl Store {
    pub async fn qq_status(&self, app_id: &str) -> Result<Value> {
        let owner: Option<String> = sqlx::query_scalar("SELECT value FROM metadata WHERE key=?")
            .bind(key(app_id, "owner"))
            .fetch_optional(&self.pool)
            .await?;
        let has_session: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM qq_sessions WHERE app_id=?)")
                .bind(app_id)
                .fetch_one(&self.pool)
                .await?;
        let counts: Vec<(String, i64)> =
            sqlx::query_as("SELECT status,COUNT(*) FROM qq_inbox WHERE app_id=? GROUP BY status")
                .bind(app_id)
                .fetch_all(&self.pool)
                .await?;
        let mut queue = json!({"pending":0,"running":0,"sent":0,"dead":0});
        for (state, count) in counts {
            queue[&state] = json!(count);
        }
        Ok(
            json!({"bound":owner.is_some(),"has_session":has_session,"owner_openid":owner,"queue":queue}),
        )
    }

    /// The first private message starts the personal session in the same transaction as its inbox.
    /// Keep an existing owner across upgrades, and acknowledge only after committing the event.
    pub async fn qq_accept(&self, app_id: &str, message: Incoming) -> Result<Acceptance> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let owner: Option<String> = sqlx::query_scalar("SELECT value FROM metadata WHERE key=?")
            .bind(key(app_id, "owner"))
            .fetch_optional(&mut *tx)
            .await?;
        let text = message.content.trim();
        if let Some(owner) = owner {
            if owner != message.sender {
                return Ok(Acceptance::OtherAccount);
            }
        } else {
            sqlx::query("INSERT INTO metadata(key,value) VALUES(?,?)")
                .bind(key(app_id, "owner"))
                .bind(&message.sender)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM metadata WHERE key=?")
                .bind(key(app_id, "pair"))
                .execute(&mut *tx)
                .await?;
        }
        let request = delivery_id(app_id, &message.message_id);
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM qq_inbox WHERE id=?)")
            .bind(&request)
            .fetch_one(&mut *tx)
            .await?;
        if exists {
            return Ok(Acceptance::Duplicate);
        }
        let queued: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM qq_inbox WHERE app_id=? AND status IN ('pending','running')",
        )
        .bind(app_id)
        .fetch_one(&mut *tx)
        .await?;
        ensure!(queued < 256, "QQ queue full");
        let mut session: Option<String> = sqlx::query_scalar(
            "SELECT session_id FROM qq_sessions WHERE app_id=? AND user_openid=?",
        )
        .bind(app_id)
        .bind(&message.sender)
        .fetch_optional(&mut *tx)
        .await?;
        if session.is_none() || text == "/新会话" {
            let new = id();
            sqlx::query("INSERT INTO sessions(id,created_at) VALUES(?,?)")
                .bind(&new)
                .bind(now())
                .execute(&mut *tx)
                .await?;
            sqlx::query("INSERT INTO qq_sessions(app_id,user_openid,session_id) VALUES(?,?,?) ON CONFLICT(app_id,user_openid) DO UPDATE SET session_id=excluded.session_id")
                .bind(app_id).bind(&message.sender).bind(&new).execute(&mut *tx).await?;
            session = Some(new);
        }
        let reply: Option<String> = match text {
                "/新会话" => Some("已开启新会话，长期记忆仍会保留。".into()),
                "/帮助" => Some("直接发送文字即可聊天。\n/新会话：清空当前对话上下文，保留长期记忆。\n记忆管理与遗忘请使用本机 Mneme Console。".into()),
                // Do not put an obsolete pairing code in history or send it to the LLM.
                _ if text == "/绑定" || text.starts_with("/绑定 ") => Some("无需绑定，直接发送文字即可聊天。".into()),
                _ if text.is_empty() => Some("目前支持文字对话，请发送文字内容。".into()),
                _ if text.chars().count() > 4000 => Some("单条消息请控制在 4000 字以内。".into()),
                _ => None,
            };
        sqlx::query("INSERT INTO qq_inbox(id,app_id,message_id,user_openid,content,session_id,reply,available_at,expires_at,created_at) VALUES(?,?,?,?,?,?,?,?,?,?)")
            .bind(request).bind(app_id).bind(message.message_id).bind(message.sender)
            .bind(if reply.is_some(){""}else{text}).bind(session.unwrap()).bind(reply)
            .bind(now()).bind(message.expires_at).bind(now()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Acceptance::Queued)
    }

    pub async fn qq_claim(&self, app_id: &str) -> Result<Option<Delivery>> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let delivery: Option<Delivery> = sqlx::query_as("SELECT id,message_id,user_openid,content,session_id,reply,next_chunk,attempts,available_at,expires_at FROM qq_inbox WHERE app_id=? AND status='pending' ORDER BY created_at,rowid LIMIT 1")
            .bind(app_id).fetch_optional(&mut *tx).await?;
        let Some(delivery) = delivery else {
            return Ok(None);
        };
        if delivery.available_at > now() {
            return Ok(None);
        }
        sqlx::query("UPDATE qq_inbox SET status='running',attempts=attempts+1 WHERE id=?")
            .bind(&delivery.id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(delivery))
    }

    pub async fn qq_reply(&self, id: &str, reply: &str) -> Result<()> {
        sqlx::query("UPDATE qq_inbox SET reply=? WHERE id=? AND reply IS NULL")
            .bind(reply)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    pub async fn qq_chunk(&self, id: &str, next: i64) -> Result<()> {
        sqlx::query("UPDATE qq_inbox SET next_chunk=? WHERE id=?")
            .bind(next)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    pub async fn qq_sent(&self, id: &str) -> Result<()> {
        sqlx::query("UPDATE qq_inbox SET status='sent',last_error=NULL WHERE id=?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
    pub async fn qq_failed(&self, item: &Delivery, code: &str, permanent: bool) -> Result<()> {
        let dead = permanent || item.attempts >= 7 || now() >= item.expires_at;
        sqlx::query("UPDATE qq_inbox SET status=?,last_error=?,available_at=? WHERE id=?")
            .bind(if dead { "dead" } else { "pending" })
            .bind(code)
            .bind(now() + (1i64 << (item.attempts + 1).min(6)))
            .bind(&item.id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
