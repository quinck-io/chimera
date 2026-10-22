//! Retry policy for the broker long poll, matching the official runner's
//! `BrokerMessageListener.GetNextMessageAsync`.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::Duration;

/// After this many failed polls in a row the poll loop gives up, so the
/// supervisor restarts the runner with a fresh token and broker session.
pub(super) const MAX_CONSECUTIVE_ERRORS: u32 = 10;

const SHORT_BACKOFF_ERRORS: u32 = 5;
const SHORT_BACKOFF_MS: (u64, u64) = (15_000, 30_000);
const LONG_BACKOFF_MS: (u64, u64) = (30_000, 60_000);

/// How long to wait after the `consecutive_errors`-th failed poll, picked
/// within the window by `random`. The jitter keeps every runner kicked off a
/// broker outage from reconnecting at the same instant.
pub(super) fn retry_delay(consecutive_errors: u32, random: u64) -> Duration {
    let (min, max) = if consecutive_errors <= SHORT_BACKOFF_ERRORS {
        SHORT_BACKOFF_MS
    } else {
        LONG_BACKOFF_MS
    };
    Duration::from_millis(min + random % (max - min))
}

pub(super) fn random_retry_delay(consecutive_errors: u32) -> Duration {
    let delay = retry_delay(consecutive_errors, random_u64());
    // Real delays would make the poll loop tests take minutes.
    if cfg!(test) { delay / 1000 } else { delay }
}

/// `RandomState` is seeded randomly per instance, so hashing nothing with a
/// fresh one gives a random value without pulling in a rand crate.
fn random_u64() -> u64 {
    RandomState::new().build_hasher().finish()
}

#[cfg(test)]
#[path = "poll_backoff_test.rs"]
mod poll_backoff_test;
