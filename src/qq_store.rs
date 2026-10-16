use crate::{
    domain::{id, now},
    store::Store,
};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use subtle::ConstantTimeEq;

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

pub fn delivery_id(app_id: &str, message_id: &str) -> String {
    let digest = Sha256::digest(format!("qq:{app_id}:{message_id}"));
    let mut bytes: [u8; 16] = digest[..16].try_into().unwrap();
    bytes[6] = (bytes[6] & 15) | 128;
    bytes[8] = (bytes[8] & 63) | 128;
    uuid::Uuid::from_bytes(bytes).to_string()
}
fn hash(code: &str) -> String {
    format!("{:x}", Sha256::digest(code.as_bytes()))
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
        let counts: Vec<(String, i64)> =
            sqlx::query_as("SELECT status,COUNT(*) FROM qq_inbox WHERE app_id=? GROUP BY status")
                .bind(app_id)
                .fetch_all(&self.pool)
                .await?;
        let mut queue = json!({"pending":0,"running":0,"sent":0,"dead":0});
        for (state, count) in counts {
            queue[&state] = json!(count);
        }
        Ok(json!({"bound":owner.is_some(),"owner_openid":owner,"queue":queue}))
    }

    pub async fn qq_pair_code(&self, app_id: &str) -> Result<Option<String>> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let bound: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM metadata WHERE key=?)")
            .bind(key(app_id, "owner"))
            .fetch_one(&mut *tx)
            .await?;
        if bound {
            return Ok(None);
        }
        let code = uuid::Uuid::new_v4().simple().to_string();
        sqlx::query("INSERT INTO metadata(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(key(app_id,"pair")).bind(json!({"hash":hash(&code),"expires":now()+600}).to_string())
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(code))
    }

    /// Acknowledge only after committing the authorized event. Pairing codes never enter history.
    pub async fn qq_accept(&self, app_id: &str, message: Incoming) -> Result<()> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let owner: Option<String> = sqlx::query_scalar("SELECT value FROM metadata WHERE key=?")
            .bind(key(app_id, "owner"))
            .fetch_optional(&mut *tx)
            .await?;
        let text = message.content.trim();
        let binding = text.strip_prefix("/绑定 ");
        let mut reply: Option<String> = None;
        if let Some(owner) = owner {
            if owner != message.sender {
                return Ok(());
            }
        } else {
            let Some(code) = binding.filter(|c| c.len() <= 128) else {
                return Ok(());
            };
            let pair: Option<String> = sqlx::query_scalar("SELECT value FROM metadata WHERE key=?")
                .bind(key(app_id, "pair"))
                .fetch_optional(&mut *tx)
                .await?;
            let pair: Value = pair
                .and_then(|v| serde_json::from_str(&v).ok())
                .unwrap_or(Value::Null);
            let valid = pair["expires"].as_i64().is_some_and(|t| t > now())
                && bool::from(
                    hash(code)
                        .as_bytes()
                        .ct_eq(pair["hash"].as_str().unwrap_or("").as_bytes()),
                );
            if !valid {
                return Ok(());
            }
            sqlx::query("INSERT INTO metadata(key,value) VALUES(?,?)")
                .bind(key(app_id, "owner"))
                .bind(&message.sender)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM metadata WHERE key=?")
                .bind(key(app_id, "pair"))
                .execute(&mut *tx)
                .await?;
            reply = Some("绑定成功。我是 Mneme，现在可以和我聊天了。发送 /新会话 可开启新对话，长期记忆会保留。".into());
        }
        let request = delivery_id(app_id, &message.message_id);
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM qq_inbox WHERE id=?)")
            .bind(&request)
            .fetch_one(&mut *tx)
            .await?;
        if exists {
            return Ok(());
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
        if reply.is_none() {
            reply = match text {
                "/新会话" => Some("已开启新会话，长期记忆仍会保留。".into()),
                "/帮助" => Some("直接发送文字即可聊天。\n/新会话：清空当前对话上下文，保留长期记忆。\n记忆管理与遗忘请使用本机 Mneme Console。".into()),
                _ if binding.is_some() => Some("当前账号已经绑定，无需再次绑定。".into()),
                _ if text.is_empty() => Some("目前支持文字对话，请发送文字内容。".into()),
                _ if text.chars().count() > 4000 => Some("单条消息请控制在 4000 字以内。".into()),
                _ => None,
            };
        }
        sqlx::query("INSERT INTO qq_inbox(id,app_id,message_id,user_openid,content,session_id,reply,available_at,expires_at,created_at) VALUES(?,?,?,?,?,?,?,?,?,?)")
            .bind(request).bind(app_id).bind(message.message_id).bind(message.sender)
            .bind(if reply.is_some(){""}else{text}).bind(session.unwrap()).bind(reply)
            .bind(now()).bind(message.expires_at).bind(now()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
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
