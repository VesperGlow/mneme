//! Per-request elapsed time from existing messages, including compacted history.
use crate::{domain::Message, store::Store};
use serde_json::{Value, json};

#[derive(Clone, Default)]
pub struct ConversationClock {
    pub previous_exchange_at: Option<i64>,
}

impl ConversationClock {
    pub async fn load(store: &Store, session: &str, current_turn: &str) -> anyhow::Result<Self> {
        let previous_exchange_at = sqlx::query_scalar("SELECT m.created_at FROM messages m JOIN turns t ON t.id=m.turn_id WHERE m.session_id=? AND t.status='complete' AND t.id!=? ORDER BY m.seq DESC LIMIT 1")
            .bind(session).bind(current_turn).fetch_optional(&store.pool).await?;
        Ok(Self {
            previous_exchange_at,
        })
    }

    pub fn from_history(history: &[Message]) -> Self {
        Self {
            previous_exchange_at: history.last().map(|m| m.created_at),
        }
    }

    pub fn context(&self, current_time: i64) -> Value {
        let elapsed = self
            .previous_exchange_at
            .and_then(|at| current_time.checked_sub(at))
            .filter(|gap| *gap >= 0);
        json!({"seconds_since_previous_exchange":elapsed})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaps_are_plain_durations_without_calendar_or_topic_decisions() {
        let clock = ConversationClock {
            previous_exchange_at: Some(1000),
        };
        assert_eq!(
            clock.context(1180),
            json!({"seconds_since_previous_exchange":180})
        );
        assert_eq!(
            clock.context(87400),
            json!({"seconds_since_previous_exchange":86400})
        );
        assert_eq!(
            clock.context(999),
            json!({"seconds_since_previous_exchange":null})
        );
        assert_eq!(
            ConversationClock::default().context(1000),
            json!({"seconds_since_previous_exchange":null})
        );
    }
}
