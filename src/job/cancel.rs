use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// A job cancel, in the two stages the official runner uses.
///
/// A request stops every step whose `if:` no longer holds, but steps guarded by
/// `always()` or `cancelled()`, and post steps that allow it, still run. Once the
/// grace period ends, the job is forced and those steps are killed too.
#[derive(Debug, Clone)]
pub struct JobCancel {
    requested: CancellationToken,
    forced: CancellationToken,
    finished: CancellationToken,
    force_deadline: Arc<watch::Sender<Option<Instant>>>,
}

impl Default for JobCancel {
    fn default() -> Self {
        Self {
            requested: CancellationToken::new(),
            forced: CancellationToken::new(),
            finished: CancellationToken::new(),
            force_deadline: Arc::new(watch::Sender::new(None)),
        }
    }
}

impl JobCancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the job to stop, and force it once `grace` has passed. Asking again
    /// moves the forced deadline to `grace` from now, as the official runner
    /// does for every cancel message it receives.
    pub fn request(&self, grace: Duration) {
        self.requested.cancel();
        let previous = self
            .force_deadline
            .send_replace(Some(Instant::now() + grace));
        if previous.is_none() {
            tokio::spawn(force_at_deadline(
                self.force_deadline.subscribe(),
                self.forced.clone(),
                self.finished.clone(),
            ));
        }
    }

    /// Stop everything at once, including steps that would survive a request.
    pub fn force(&self) {
        self.requested.cancel();
        self.forced.cancel();
    }

    /// The job is over: stop watching for cancels and drop the forced deadline.
    pub fn finish(&self) {
        self.finished.cancel();
    }

    pub fn is_requested(&self) -> bool {
        self.requested.is_cancelled()
    }

    pub async fn finished(&self) {
        self.finished.cancelled().await;
    }

    /// The token a step runs under. A step that `survives_cancel` is only
    /// stopped when the job is forced.
    pub fn step_token(&self, survives_cancel: bool) -> CancellationToken {
        if survives_cancel {
            self.forced.child_token()
        } else {
            self.requested.child_token()
        }
    }
}

async fn force_at_deadline(
    mut deadline: watch::Receiver<Option<Instant>>,
    forced: CancellationToken,
    finished: CancellationToken,
) {
    loop {
        let Some(force_at) = *deadline.borrow_and_update() else {
            return;
        };
        tokio::select! {
            _ = tokio::time::sleep_until(force_at) => {
                forced.cancel();
                return;
            }
            changed = deadline.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            _ = forced.cancelled() => return,
            _ = finished.cancelled() => return,
        }
    }
}

#[cfg(test)]
#[path = "cancel_test.rs"]
mod cancel_test;
