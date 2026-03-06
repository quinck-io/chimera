use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer};

/// Deserialize a bool that might be null in JSON (treat null as false).
pub fn deserialize_nullable_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<bool>::deserialize(deserializer).map(|opt| opt.unwrap_or(false))
}

/// RFC3339 with 7 decimal places (100ns precision), used for timeline records.
pub fn format_timeline_timestamp(ts: DateTime<Utc>) -> String {
    let frac = ts.timestamp_subsec_nanos() / 100;
    format!("{}.{:07}Z", ts.format("%Y-%m-%dT%H:%M:%S"), frac)
}

/// RFC3339 with 7 decimal places, used for log lines.
pub fn format_log_timestamp(ts: DateTime<Utc>) -> String {
    format_timeline_timestamp(ts)
}

/// RFC3339 with 3 decimal places (millisecond precision), used for Results API.
pub fn format_results_timestamp(ts: DateTime<Utc>) -> String {
    ts.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// GitHub Actions OS label for the current platform.
pub fn os_label() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "macOS",
        "windows" => "Windows",
        other => other,
    }
}

/// GitHub Actions architecture label for the current platform.
pub fn arch_label() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "X64",
        "aarch64" => "ARM64",
        "arm" => "ARM",
        other => other,
    }
}

#[cfg(test)]
#[path = "utils_test.rs"]
mod utils_test;
