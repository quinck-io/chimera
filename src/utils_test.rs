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

#[test]
fn nullable_bool_false_for_null() {
    #[derive(serde::Deserialize)]
    struct T {
        #[serde(default, deserialize_with = "deserialize_nullable_bool")]
        val: bool,
    }

    let t: T = serde_json::from_str(r#"{"val": null}"#).unwrap();
    assert!(!t.val);
}

#[test]
fn nullable_bool_true_for_true() {
    #[derive(serde::Deserialize)]
    struct T {
        #[serde(default, deserialize_with = "deserialize_nullable_bool")]
        val: bool,
    }

    let t: T = serde_json::from_str(r#"{"val": true}"#).unwrap();
    assert!(t.val);
}

#[test]
fn nullable_bool_false_when_absent() {
    #[derive(serde::Deserialize)]
    struct T {
        #[serde(default, deserialize_with = "deserialize_nullable_bool")]
        val: bool,
    }

    let t: T = serde_json::from_str(r#"{}"#).unwrap();
    assert!(!t.val);
}
