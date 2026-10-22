use std::collections::HashSet;
use std::time::Duration;

use super::*;

#[test]
fn first_five_errors_wait_between_15_and_30_seconds() {
    for errors in 1..=5 {
        assert_eq!(retry_delay(errors, 0), Duration::from_secs(15));
        assert_eq!(retry_delay(errors, 14_999), Duration::from_millis(29_999));
    }
}

#[test]
fn later_errors_wait_between_30_and_60_seconds() {
    for errors in 6..=MAX_CONSECUTIVE_ERRORS {
        assert_eq!(retry_delay(errors, 0), Duration::from_secs(30));
        assert_eq!(retry_delay(errors, 29_999), Duration::from_millis(59_999));
    }
}

#[test]
fn random_value_wraps_into_the_window() {
    let delay = retry_delay(1, u64::MAX);

    assert!(delay >= Duration::from_secs(15) && delay < Duration::from_secs(30));
}

#[test]
fn random_values_differ_between_calls() {
    let values: HashSet<u64> = (0..8).map(|_| random_u64()).collect();

    assert!(values.len() > 1, "expected jitter, got {values:?}");
}

#[test]
fn delays_are_scaled_down_under_test() {
    let delay = random_retry_delay(6);

    assert!(delay >= Duration::from_millis(30) && delay < Duration::from_millis(60));
}
