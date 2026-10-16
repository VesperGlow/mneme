use serde::Serialize;
use std::{sync::Mutex, time::Instant};

#[derive(Default, Serialize, Clone)]
pub struct Sample {
    pub requests: u64,
    pub failures: u64,
    pub last_ms: f64,
    pub average_ms: f64,
}
#[derive(Default)]
pub struct Metrics(Mutex<Sample>);
impl Metrics {
    pub fn record(&self, start: Instant, success: bool) {
        let mut value = self.0.lock().unwrap_or_else(|e| e.into_inner());
        value.requests += 1;
        if !success {
            value.failures += 1;
        }
        value.last_ms = start.elapsed().as_secs_f64() * 1000.0;
        value.average_ms += (value.last_ms - value.average_ms) / value.requests as f64;
    }
    pub fn snapshot(&self) -> Sample {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}
