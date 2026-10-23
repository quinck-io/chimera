use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::github::broker::{BrokerClient, BrokerMessage, MessageType};
use crate::job::cancel::JobCancel;

const CANCEL_POLL_DELAY: Duration = Duration::from_millis(2000);
const CANCEL_POLL_ERROR_DELAY: Duration = Duration::from_millis(5000);

/// Used when a cancellation message names no timeout.
const DEFAULT_CANCEL_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How long steps that outlive a cancel may keep running when the daemon
/// cancels jobs at shutdown.
pub const SHUTDOWN_CANCEL_GRACE: Duration = Duration::from_secs(30);

/// Spawn a background task that polls the broker for cancellation messages
/// until the job finishes. Every `JobCancellation` requests the cancel again,
/// so a later message reschedules when the job is forced.
pub fn spawn_cancel_poller(broker: &BrokerClient, cancel: JobCancel) -> JoinHandle<()> {
    let client = broker.client().clone();
    let server_url = broker.server_url().to_string();
    let session_id = broker.session_id().to_string();
    let token_manager = broker.token_manager_arc();

    tokio::spawn(async move {
        let poller = BrokerClient::new(client, server_url, session_id, token_manager);
        loop {
            let poll_result = tokio::select! {
                result = poller.poll_message() => result,
                _ = cancel.finished() => return,
            };

            let delay = match poll_result {
                Ok(Some(msg)) if msg.message_type == MessageType::JobCancellation => {
                    request_cancel(&cancel, &msg);
                    CANCEL_POLL_DELAY
                }
                Ok(_) => CANCEL_POLL_DELAY,
                Err(_) => CANCEL_POLL_ERROR_DELAY,
            };
            let delay = if cfg!(test) {
                Duration::from_millis(10)
            } else {
                delay
            };
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = cancel.finished() => return,
            }
        }
    })
}

/// Spawn a background task that cancels the job once the daemon stops waiting
/// for jobs at shutdown, so the job is reported and cleaned up instead of being
/// dropped mid-step.
pub fn spawn_shutdown_canceller(stop_jobs: CancellationToken, cancel: JobCancel) -> JoinHandle<()> {
    tokio::spawn(async move {
        tokio::select! {
            _ = stop_jobs.cancelled() => {
                warn!(grace = ?SHUTDOWN_CANCEL_GRACE, "daemon is shutting down, cancelling the job");
                cancel.request(SHUTDOWN_CANCEL_GRACE);
            }
            _ = cancel.finished() => {}
        }
    })
}

fn request_cancel(cancel: &JobCancel, msg: &BrokerMessage) {
    let (job_id, timeout) = match msg.parse_cancellation() {
        Ok(cancellation) => (cancellation.job_id, cancellation.timeout),
        Err(e) => {
            warn!(error = %format!("{e:#}"), "unreadable job cancellation");
            ("unknown".into(), None)
        }
    };
    let grace = cancel_grace(timeout);
    info!(job_id, ?grace, "received job cancellation from broker");
    cancel.request(grace);
}

/// How long steps that outlive a cancel may keep running. Like the official
/// runner, this is the cancel timeout, at least a minute, less 15 seconds.
fn cancel_grace(timeout: Option<Duration>) -> Duration {
    let timeout = timeout
        .unwrap_or(DEFAULT_CANCEL_TIMEOUT)
        .max(Duration::from_secs(60));
    timeout - Duration::from_secs(15)
}

#[cfg(test)]
#[path = "cancel_test.rs"]
mod cancel_test;
