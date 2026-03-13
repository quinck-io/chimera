use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::github::broker::{BrokerClient, MessageType};

const CANCEL_POLL_DELAY: Duration = Duration::from_millis(2000);
const CANCEL_POLL_ERROR_DELAY: Duration = Duration::from_millis(5000);

/// Spawn a background task that polls the broker for cancellation messages.
/// When a `JobCancellation` arrives, it triggers the token.
pub fn spawn_cancel_poller(
    broker: &BrokerClient,
    cancel_token: CancellationToken,
) -> JoinHandle<()> {
    let client = broker.client().clone();
    let server_url = broker.server_url().to_string();
    let session_id = broker.session_id().to_string();
    let token_manager = broker.token_manager_arc();

    tokio::spawn(async move {
        let poller = BrokerClient::new(client, server_url, session_id, token_manager);
        loop {
            if cancel_token.is_cancelled() {
                return;
            }

            let poll_result = tokio::select! {
                result = poller.poll_message() => result,
                _ = cancel_token.cancelled() => return,
            };

            match poll_result {
                Ok(Some(msg)) if msg.message_type == MessageType::JobCancellation => {
                    let job_id = msg
                        .parse_cancellation_job_id()
                        .unwrap_or_else(|_| "unknown".into());
                    info!(job_id, "received job cancellation from broker");
                    cancel_token.cancel();
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
                        _ = cancel_token.cancelled() => return,
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
                        _ = cancel_token.cancelled() => return,
                    }
                }
            }
        }
    })
}

#[cfg(test)]
#[path = "cancel_test.rs"]
mod cancel_test;
