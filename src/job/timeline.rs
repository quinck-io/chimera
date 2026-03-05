use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_repr::Serialize_repr;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineRecord {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<TimelineState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<TimelineResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log: Option<TimelineLogRef>,
}

#[derive(Debug, Clone, Copy, Serialize_repr)]
#[repr(u8)]
pub enum TimelineState {
    InProgress = 1,
    Completed = 2,
}

#[derive(Debug, Clone, Copy, Serialize_repr)]
#[repr(u8)]
pub enum TimelineResult {
    Succeeded = 0,
    Failed = 2,
    Cancelled = 3,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimelineLogRef {
    pub id: u64,
}

/// Format a timestamp for timeline updates: RFC3339 with 7-decimal fractional seconds.
pub fn format_timeline_timestamp(ts: DateTime<Utc>) -> String {
    let nanos = ts.timestamp_subsec_nanos();
    // 7 decimal places = 100ns precision
    let frac = nanos / 100;
    format!("{}.{:07}Z", ts.format("%Y-%m-%dT%H:%M:%S"), frac)
}

#[cfg(test)]
#[path = "timeline_test.rs"]
mod timeline_test;
