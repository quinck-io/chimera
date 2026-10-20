use std::time::Duration;

use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::github::broker::{BrokerClient, MessageType};
use crate::job::cancel::JobCancel;

const CANCEL_POLL_DELAY: Duration = Duration::from_millis(2000);
const CANCEL_POLL_ERROR_DELAY: Duration = Duration::from_millis(5000);

/// Used when a cancellation message names no timeout.
const DEFAULT_CANCEL_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Spawn a background task that polls the broker for cancellation messages.
/// When a `JobCancellation` arrives, it requests the cancel.
pub fn spawn_cancel_poller(broker: &BrokerClient, cancel: JobCancel) -> JoinHandle<()> {
    let client = broker.client().clone();
    let server_url = broker.server_url().to_string();
    let session_id = broker.session_id().to_string();
    let token_manager = broker.token_manager_arc();

    tokio::spawn(async move {
        let poller = BrokerClient::new(client, server_url, session_id, token_manager);
        loop {
            if cancel.is_requested() {
                return;
            }

            let poll_result = tokio::select! {
                result = poller.poll_message() => result,
                _ = cancel.requested() => return,
            };

            match poll_result {
                Ok(Some(msg)) if msg.message_type == MessageType::JobCancellation => {
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
                    return;
                }
                Ok(_) => {
                    // Not a cancellation — brief pause to avoid tight loop
                    let delay = if cfg!(test) {
                        Duration::from_millis(10)
                    } else {
                        CANCEL_POLL_DELAY
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = cancel.requested() => return,
                    }
                }
                Err(_) => {
                    // Poll error — brief pause then retry
                    let delay = if cfg!(test) {
                        Duration::from_millis(10)
                    } else {
                        CANCEL_POLL_ERROR_DELAY
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = cancel.requested() => return,
                    }
                }
            }
        }
    })
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
