use std::time::Duration;

use tokio_util::sync::CancellationToken;

/// A job cancel, in the two stages the official runner uses.
///
/// A request stops every step whose `if:` no longer holds, but steps guarded by
/// `always()` or `cancelled()`, and post steps that allow it, still run. Once the
/// grace period ends, the job is forced and those steps are killed too.
#[derive(Debug, Clone, Default)]
pub struct JobCancel {
    requested: CancellationToken,
    forced: CancellationToken,
}

impl JobCancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the job to stop, and force it once `grace` has passed.
    pub fn request(&self, grace: Duration) {
        self.requested.cancel();
        let forced = self.forced.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(grace) => forced.cancel(),
                _ = forced.cancelled() => {}
            }
        });
    }

    /// Stop everything at once, including steps that would survive a request.
    pub fn force(&self) {
        self.requested.cancel();
        self.forced.cancel();
    }

    pub fn is_requested(&self) -> bool {
        self.requested.is_cancelled()
    }

    pub async fn requested(&self) {
        self.requested.cancelled().await;
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

#[cfg(test)]
#[path = "cancel_test.rs"]
mod cancel_test;
