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
    feed: Option<(super::live_feed::FeedSender, String)>,
}

impl LogSender {
    #[cfg(test)]
    pub fn new_for_test(tx: mpsc::Sender<LogLine>, masks: Arc<RwLock<Vec<String>>>) -> Self {
        Self {
            tx,
            masks,
            feed: None,
        }
    }

    pub async fn send(&self, content: String) {
        let masked = self.apply_masks(&content).await;
        if let Some((ref feed, ref step_id)) = self.feed {
            feed.send(step_id, &masked).await;
        }
        let line = LogLine {
            timestamp: Utc::now(),
            content: masked,
        };
        if self.tx.send(line).await.is_err() {
            warn!("log channel closed, dropping line");
        }
    }

    pub async fn send_banner(&self, runner_name: &str, container_mode: bool) {
        let version = env!("CARGO_PKG_VERSION");
        let y = "\x1b[33m";
        let r = "\x1b[0m";
        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        let mode = match container_mode {
            true => "container",
            false => "host",
        };

        self.send(format!("{y}========================================{r}"))
            .await;
        self.send(format!("{y}  chimera v{version} ({os}/{arch}){r}"))
            .await;
        self.send(format!("{y}  runner: {runner_name}{r}")).await;
        self.send(format!("{y}  executing job in {mode} mode{r}"))
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
/// **Results mode**: streams lines to an Azure Append Blob incrementally via
/// the Results twirp API. The GitHub UI reads from the blob directly.
///
/// **Legacy mode**: streams lines to the legacy VSS API for live UI visibility.
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
    /// Create a StepLogger for the Results twirp API.
    ///
    /// Lines are streamed incrementally to an Azure Append Blob. The blob is
    /// left unsealed during execution so GitHub's UI can read partial content,
    /// and sealed when the step finishes.
    pub fn results(
        client: Arc<JobClient>,
        plan_id: String,
        job_id: String,
        step_id: String,
        masks: Arc<RwLock<Vec<String>>>,
        feed: Option<(super::live_feed::FeedSender, String)>,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<LogLine>(256);
        let sender = LogSender { tx, masks, feed };
        let handle = tokio::spawn(blob_collector_task(client, plan_id, job_id, step_id, rx));
        Self::Results { sender, handle }
    }

    /// Create a StepLogger for the legacy VSS API (streams to GitHub).
    pub async fn legacy(
        client: Arc<JobClient>,
        plan_id: &str,
        step_name: &str,
        masks: Arc<RwLock<Vec<String>>>,
        feed: Option<(super::live_feed::FeedSender, String)>,
    ) -> Self {
        let log_id = client
            .create_log(plan_id, step_name)
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "failed to create log, using 0");
                0
            });
        let (tx, rx) = mpsc::channel::<LogLine>(256);
        let sender = LogSender { tx, masks, feed };
        let handle = tokio::spawn(vss_upload_task(client, plan_id.to_string(), log_id, rx));
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

    /// Finish the step log: flush remaining content, seal blob (Results),
    /// and return collected output.
    pub async fn finish(self) -> Option<CollectedLog> {
        match self {
            Self::Results { sender, handle } => {
                drop(sender);
                match handle.await {
                    Ok((text, line_count)) => Some(CollectedLog { text, line_count }),
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

    /// Test-only: creates a Results logger that collects lines without uploading.
    #[cfg(test)]
    pub fn results_for_test(masks: Arc<RwLock<Vec<String>>>) -> Self {
        let (tx, rx) = mpsc::channel::<LogLine>(256);
        let sender = LogSender {
            tx,
            masks,
            feed: None,
        };
        let handle = tokio::spawn(simple_log_collector(rx));
        Self::Results { sender, handle }
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
// Legacy VSS log upload (real-time streaming)
// ---------------------------------------------------------------------------

async fn vss_upload_task(
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
                            flush_to_vss(&client, &plan_id, log_id, &mut buffer).await;
                        }
                    }
                    None => {
                        if !buffer.is_empty() {
                            flush_to_vss(&client, &plan_id, log_id, &mut buffer).await;
                        }
                        return;
                    }
                }
            }
            _ = interval.tick() => {
                if !buffer.is_empty() {
                    flush_to_vss(&client, &plan_id, log_id, &mut buffer).await;
                }
            }
        }
    }
}

async fn flush_to_vss(client: &JobClient, plan_id: &str, log_id: u64, buffer: &mut String) {
    let content = std::mem::take(buffer);
    if let Err(e) = client.upload_log_lines(plan_id, log_id, &content).await {
        warn!(error = %e, "failed to upload log lines");
    }
}

// ---------------------------------------------------------------------------
// Results blob collector (incremental append blob streaming)
// ---------------------------------------------------------------------------

/// Mutable state for the incremental blob upload.
struct BlobUploadState {
    signed_url: Option<super::client::SignedUrlResponse>,
}

/// Collects log lines and streams them to an Azure Append Blob incrementally.
///
/// On first content: gets a signed URL, creates the blob, appends content.
/// On each interval: flushes the buffer to the blob and clears it to bound memory.
/// On channel close: flushes remaining, seals the blob, posts final metadata.
async fn blob_collector_task(
    client: Arc<JobClient>,
    plan_id: String,
    job_id: String,
    step_id: String,
    mut rx: mpsc::Receiver<LogLine>,
) -> (String, i64) {
    let mut buffer = String::new();
    let mut line_count: i64 = 0;
    let mut blob = BlobUploadState { signed_url: None };
    let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(FLUSH_INTERVAL_MS));
    interval.tick().await;

    loop {
        tokio::select! {
            maybe_line = rx.recv() => {
                match maybe_line {
                    Some(line) => {
                        buffer.push_str(&format_line(&line));
                        line_count += 1;
                    }
                    None => break,
                }
            }
            _ = interval.tick() => {
                if !buffer.is_empty() {
                    flush_to_blob(
                        &client, &plan_id, &job_id, &step_id,
                        &buffer, line_count, &mut blob,
                    ).await;
                    buffer.clear();
                }
            }
        }
    }

    // Final flush: append remaining + seal
    if !buffer.is_empty() {
        flush_to_blob(
            &client, &plan_id, &job_id, &step_id, &buffer, line_count, &mut blob,
        )
        .await;
        buffer.clear();
    }
    if let Some(ref url) = blob.signed_url {
        if let Err(e) = client.seal_blob(url).await {
            warn!(error = %e, "failed to seal step log blob");
        }
        if let Err(e) = client
            .create_step_log_metadata(&plan_id, &job_id, &step_id, line_count)
            .await
        {
            warn!(error = %e, "failed to create final step log metadata");
        }
    }

    // Return empty string — content was already streamed to the blob
    (String::new(), line_count)
}

async fn flush_to_blob(
    client: &JobClient,
    plan_id: &str,
    job_id: &str,
    step_id: &str,
    buffer: &str,
    line_count: i64,
    blob: &mut BlobUploadState,
) {
    // Create the blob on first flush
    if blob.signed_url.is_none() {
        match client
            .get_step_log_signed_url(plan_id, job_id, step_id)
            .await
        {
            Ok(url) => {
                if let Err(e) = client.create_append_blob(&url).await {
                    warn!(error = %e, "failed to create append blob");
                    return;
                }
                blob.signed_url = Some(url);
            }
            Err(e) => {
                warn!(error = %e, "failed to get signed URL for step log");
                return;
            }
        }
    }

    let Some(url) = blob.signed_url.as_ref() else {
        return;
    };
    if let Err(e) = client.append_blob_block(url, buffer).await {
        warn!(error = %e, "failed to append log block");
        return;
    }

    // Post metadata to notify GitHub that new log data is available
    if let Err(e) = client
        .create_step_log_metadata(plan_id, job_id, step_id, line_count)
        .await
    {
        warn!(error = %e, "intermediate log metadata update failed");
    }
}

fn format_line(line: &LogLine) -> String {
    format!(
        "{} {}\n",
        format_log_timestamp(line.timestamp),
        line.content
    )
}

#[cfg(test)]
async fn simple_log_collector(mut rx: mpsc::Receiver<LogLine>) -> (String, i64) {
    let mut buffer = String::new();
    let mut line_count: i64 = 0;
    while let Some(line) = rx.recv().await {
        buffer.push_str(&format_line(&line));
        line_count += 1;
    }
    (buffer, line_count)
}

#[cfg(test)]
#[path = "logs_test.rs"]
mod logs_test;
