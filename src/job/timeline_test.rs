use super::*;

#[test]
fn in_progress_record_json_shape() {
    let record = TimelineRecord {
        id: "step-1".into(),
        state: Some(TimelineState::InProgress),
        result: None,
        start_time: Some("2024-01-01T00:00:00.0000000Z".into()),
        finish_time: None,
        name: Some("Run tests".into()),
        order: Some(1),
        log: None,
    };

    let json: serde_json::Value = serde_json::to_value(&record).unwrap();
    assert_eq!(json["state"], 1);
    assert!(json.get("result").is_none());
    assert!(json.get("finishTime").is_none());
    assert_eq!(json["name"], "Run tests");
}

#[test]
fn completed_record_with_result() {
    let record = TimelineRecord {
        id: "step-1".into(),
        state: Some(TimelineState::Completed),
        result: Some(TimelineResult::Succeeded),
        start_time: Some("2024-01-01T00:00:00.0000000Z".into()),
        finish_time: Some("2024-01-01T00:00:05.0000000Z".into()),
        name: Some("Run tests".into()),
        order: Some(1),
        log: Some(TimelineLogRef { id: 42 }),
    };

    let json: serde_json::Value = serde_json::to_value(&record).unwrap();
    assert_eq!(json["state"], 2);
    assert_eq!(json["result"], 0);
    assert_eq!(json["log"]["id"], 42);
}

#[test]
fn state_serializes_as_integer() {
    assert_eq!(serde_json::to_value(TimelineState::InProgress).unwrap(), 1);
    assert_eq!(serde_json::to_value(TimelineState::Completed).unwrap(), 2);
}

#[test]
fn result_serializes_as_integer() {
    assert_eq!(serde_json::to_value(TimelineResult::Succeeded).unwrap(), 0);
    assert_eq!(serde_json::to_value(TimelineResult::Failed).unwrap(), 2);
    assert_eq!(serde_json::to_value(TimelineResult::Cancelled).unwrap(), 3);
}
