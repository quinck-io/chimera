use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinHandle;
use tracing::warn;

use super::JobClient;

pub struct LogLine {
    pub timestamp: DateTime<Utc>,
    pub content: String,
}

#[derive(Clone)]
pub struct LogSender {
    tx: mpsc::Sender<LogLine>,
    masks: Arc<RwLock<Vec<String>>>,
}

impl LogSender {
    pub async fn send(&self, content: String) {
        let masked = self.apply_masks(&content).await;
        let line = LogLine {
            timestamp: Utc::now(),
            content: masked,
        };
        if self.tx.send(line).await.is_err() {
            warn!("log channel closed, dropping line");
        }
    }

    async fn apply_masks(&self, content: &str) -> String {
        let masks = self.masks.read().await;
        let mut result = content.to_string();
        for mask in masks.iter() {
            if !mask.is_empty() {
                result = result.replace(mask, "***");
            }
        }
        result
    }
}

const FLUSH_INTERVAL_MS: u64 = 1000;
const FLUSH_BUFFER_BYTES: usize = 64 * 1024;

/// Start a background log upload task.
/// Returns a LogSender to send lines to, and the task handle.
pub fn start_log_upload(
    client: Arc<JobClient>,
    plan_id: String,
    log_id: u64,
    masks: Arc<RwLock<Vec<String>>>,
) -> (LogSender, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel::<LogLine>(256);

    let sender = LogSender { tx, masks };

    let handle = tokio::spawn(log_upload_task(client, plan_id, log_id, rx));

    (sender, handle)
}

async fn log_upload_task(
    client: Arc<JobClient>,
    plan_id: String,
    log_id: u64,
    mut rx: mpsc::Receiver<LogLine>,
) {
    let mut buffer = String::new();
    let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(FLUSH_INTERVAL_MS));
    interval.tick().await; // skip first tick

    loop {
        tokio::select! {
            maybe_line = rx.recv() => {
                match maybe_line {
                    Some(line) => {
                        let formatted = format!(
                            "{} {}\n",
                            format_log_timestamp(line.timestamp),
                            line.content
                        );
                        buffer.push_str(&formatted);

                        if buffer.len() >= FLUSH_BUFFER_BYTES {
                            flush(&client, &plan_id, log_id, &mut buffer).await;
                        }
                    }
                    None => {
                        // Channel closed — flush remaining and exit
                        if !buffer.is_empty() {
                            flush(&client, &plan_id, log_id, &mut buffer).await;
                        }
                        return;
                    }
                }
            }
            _ = interval.tick() => {
                if !buffer.is_empty() {
                    flush(&client, &plan_id, log_id, &mut buffer).await;
                }
            }
        }
    }
}

async fn flush(client: &JobClient, plan_id: &str, log_id: u64, buffer: &mut String) {
    let content = std::mem::take(buffer);
    if let Err(e) = client.upload_log_lines(plan_id, log_id, &content).await {
        warn!(error = %e, "failed to upload log lines");
    }
}

/// Format a timestamp for log lines: RFC3339 with 7 decimal places.
pub fn format_log_timestamp(ts: DateTime<Utc>) -> String {
    let nanos = ts.timestamp_subsec_nanos();
    let frac = nanos / 100;
    format!("{}.{:07}Z", ts.format("%Y-%m-%dT%H:%M:%S"), frac)
}

#[cfg(test)]
#[path = "logs_test.rs"]
mod logs_test;
