use super::*;
use chrono::TimeZone;

#[test]
fn timeline_timestamp_seven_decimals() {
    let ts = Utc.with_ymd_and_hms(2024, 6, 15, 12, 30, 45).unwrap();
    assert_eq!(
        format_timeline_timestamp(ts),
        "2024-06-15T12:30:45.0000000Z"
    );
}

#[test]
fn log_timestamp_matches_timeline_format() {
    let ts = Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap();
    assert_eq!(format_log_timestamp(ts), format_timeline_timestamp(ts));
}

#[test]
fn results_timestamp_three_decimals() {
    let ts = Utc.with_ymd_and_hms(2024, 6, 15, 12, 30, 45).unwrap();
    assert_eq!(format_results_timestamp(ts), "2024-06-15T12:30:45.000Z");
}
