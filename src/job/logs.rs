use std::sync::Arc;

use chrono::Utc;
use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinHandle;
use tracing::warn;

use super::JobClient;
use crate::utils::format_log_timestamp;

pub struct LogLine {
    pub timestamp: chrono::DateTime<Utc>,
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

    pub async fn send_banner(&self, runner_name: &str) {
        let version = env!("CARGO_PKG_VERSION");
        let y = "\x1b[33m";
        let r = "\x1b[0m";
        self.send(format!("{y}========================================{r}"))
            .await;
        self.send(format!("{y}  chimera v{version}{r}")).await;
        self.send(format!("{y}  running on: {runner_name}{r}"))
            .await;
        self.send(format!("{y}========================================{r}"))
            .await;
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

// ---------------------------------------------------------------------------
// Step log lifecycle — encapsulates create → collect/stream → upload
// ---------------------------------------------------------------------------

/// Collected log output from a step (text + line count).
pub struct CollectedLog {
    pub text: String,
    pub line_count: i64,
}

/// Manages the full log lifecycle for a single step.
///
/// For the Results API path: collects lines in memory, then uploads via signed URL.
/// For the legacy path: streams lines to GitHub in real-time via the VSS API.
pub enum StepLogger {
    Results {
        sender: LogSender,
        handle: JoinHandle<(String, i64)>,
    },
    Legacy {
        sender: LogSender,
        handle: JoinHandle<()>,
        log_id: u64,
    },
}

impl StepLogger {
    /// Create a StepLogger for the Results twirp API (collects in memory).
    pub fn results(masks: Arc<RwLock<Vec<String>>>) -> Self {
        let (sender, handle) = start_log_collector(masks);
        Self::Results { sender, handle }
    }

    /// Create a StepLogger for the legacy VSS API (streams to GitHub).
    pub async fn legacy(
        client: Arc<JobClient>,
        plan_id: &str,
        step_name: &str,
        masks: Arc<RwLock<Vec<String>>>,
    ) -> Self {
        let log_id = client
            .create_log(plan_id, step_name)
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "failed to create log, using 0");
                0
            });
        let (sender, handle) = start_log_upload(client, plan_id.to_string(), log_id, masks);
        Self::Legacy {
            sender,
            handle,
            log_id,
        }
    }

    pub fn sender(&self) -> &LogSender {
        match self {
            Self::Results { sender, .. } | Self::Legacy { sender, .. } => sender,
        }
    }

    /// Finish the step log: flush, collect output, and upload if Results mode.
    /// Returns the collected log (Results) or None (legacy).
    pub async fn finish(
        self,
        client: &JobClient,
        plan_id: &str,
        job_id: &str,
        step_id: &str,
        step_name: &str,
    ) -> Option<CollectedLog> {
        match self {
            Self::Results { sender, handle } => {
                drop(sender);
                match handle.await {
                    Ok((text, line_count)) => {
                        if !text.is_empty()
                            && let Err(e) = client
                                .upload_step_log(plan_id, job_id, step_id, &text, line_count)
                                .await
                        {
                            warn!(error = %e, step = step_name, "failed to upload step log");
                        }
                        Some(CollectedLog { text, line_count })
                    }
                    Err(e) => {
                        warn!(error = %e, "log collector task panicked");
                        None
                    }
                }
            }
            Self::Legacy {
                sender,
                handle,
                log_id: _,
            } => {
                drop(sender);
                let _ = handle.await;
                None
            }
        }
    }

    /// The legacy log ID (only meaningful for legacy mode).
    pub fn log_id(&self) -> u64 {
        match self {
            Self::Legacy { log_id, .. } => *log_id,
            Self::Results { .. } => 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Legacy log upload (real-time streaming to GitHub)
// ---------------------------------------------------------------------------

/// Start a background log upload task (legacy VSS API).
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
    interval.tick().await;

    loop {
        tokio::select! {
            maybe_line = rx.recv() => {
                match maybe_line {
                    Some(line) => {
                        buffer.push_str(&format_line(&line));
                        if buffer.len() >= FLUSH_BUFFER_BYTES {
                            flush_to_api(&client, &plan_id, log_id, &mut buffer).await;
                        }
                    }
                    None => {
                        if !buffer.is_empty() {
                            flush_to_api(&client, &plan_id, log_id, &mut buffer).await;
                        }
                        return;
                    }
                }
            }
            _ = interval.tick() => {
                if !buffer.is_empty() {
                    flush_to_api(&client, &plan_id, log_id, &mut buffer).await;
                }
            }
        }
    }
}

async fn flush_to_api(client: &JobClient, plan_id: &str, log_id: u64, buffer: &mut String) {
    let content = std::mem::take(buffer);
    if let Err(e) = client.upload_log_lines(plan_id, log_id, &content).await {
        warn!(error = %e, "failed to upload log lines");
    }
}

// ---------------------------------------------------------------------------
// Results log collector (in-memory collection for later upload)
// ---------------------------------------------------------------------------

/// Start a log collector that buffers lines in memory (Results twirp API).
fn start_log_collector(masks: Arc<RwLock<Vec<String>>>) -> (LogSender, JoinHandle<(String, i64)>) {
    let (tx, rx) = mpsc::channel::<LogLine>(256);
    let sender = LogSender { tx, masks };
    let handle = tokio::spawn(log_collector_task(rx));
    (sender, handle)
}

async fn log_collector_task(mut rx: mpsc::Receiver<LogLine>) -> (String, i64) {
    let mut buffer = String::new();
    let mut line_count: i64 = 0;

    while let Some(line) = rx.recv().await {
        buffer.push_str(&format_line(&line));
        line_count += 1;
    }

    (buffer, line_count)
}

fn format_line(line: &LogLine) -> String {
    format!(
        "{} {}\n",
        format_log_timestamp(line.timestamp),
        line.content
    )
}

#[cfg(test)]
#[path = "logs_test.rs"]
mod logs_test;
