//! Cancellation is durable in SQLite; the registry only wakes running futures.
use crate::{App, domain::Message};
use axum::response::sse::Event;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, watch};

#[derive(Clone, Default)]
pub struct Cancellations(Arc<Mutex<HashMap<String, watch::Sender<bool>>>>);

impl Cancellations {
    pub fn cancel(&self, turn: &str) {
        if let Some(sender) = self.0.lock().unwrap().get(turn) {
            sender.send_replace(true);
        }
    }
}

struct Registration(Cancellations, String);
impl Drop for Registration {
    fn drop(&mut self) {
        self.0.0.lock().unwrap().remove(&self.1);
    }
}

pub(crate) async fn run(
    app: &App,
    user: &Message,
    output: &mpsc::Sender<Event>,
) -> anyhow::Result<String> {
    let (sender, mut cancelled) = watch::channel(false);
    app.cancellations
        .0
        .lock()
        .unwrap()
        .insert(user.turn_id.clone(), sender);
    let _registration = Registration(app.cancellations.clone(), user.turn_id.clone());
    // A cancellation can commit before this future has registered its wakeup.
    let generating: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM turns WHERE id=? AND status='generating')")
            .bind(&user.turn_id)
            .fetch_one(&app.store.pool)
            .await?;
    anyhow::ensure!(generating, "turn is no longer generating");
    let mut shutdown = app.shutdown.clone();
    anyhow::ensure!(!*shutdown.borrow(), "shutdown");
    tokio::select! {
        biased;
        _ = cancelled.wait_for(|value| *value) => anyhow::bail!("generation cancelled"),
        _ = shutdown.changed() => anyhow::bail!("shutdown"),
        result = crate::chat::generate(app, user, output) => result,
    }
}
