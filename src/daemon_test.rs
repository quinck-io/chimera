use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use tempfile::TempDir;

use super::*;

// --- PID lock tests ---

#[test]
fn acquire_lock_succeeds_when_no_file() {
    let tmp = TempDir::new().unwrap();
    let pid_path = tmp.path().join("chimera.pid");

    let lock = PidLock::acquire(&pid_path).unwrap();

    assert!(pid_path.exists());
    let content = std::fs::read_to_string(&pid_path).unwrap();
    assert_eq!(content.trim(), std::process::id().to_string());

    drop(lock);
}

#[test]
fn acquire_lock_fails_when_already_held() {
    let tmp = TempDir::new().unwrap();
    let pid_path = tmp.path().join("chimera.pid");

    // Write our own PID — process is alive
    std::fs::write(&pid_path, std::process::id().to_string()).unwrap();

    let result = PidLock::acquire(&pid_path);
    assert!(result.is_err());

    let err = result.unwrap_err().to_string();
    assert!(err.contains("already running"), "got: {err}");
}

#[test]
fn acquire_lock_removes_stale_file() {
    let tmp = TempDir::new().unwrap();
    let pid_path = tmp.path().join("chimera.pid");

    // PID that almost certainly doesn't exist
    std::fs::write(&pid_path, "1000000000").unwrap();

    let lock = PidLock::acquire(&pid_path).unwrap();

    let content = std::fs::read_to_string(&pid_path).unwrap();
    assert_eq!(content.trim(), std::process::id().to_string());

    drop(lock);
}

#[test]
fn release_lock_removes_file() {
    let tmp = TempDir::new().unwrap();
    let pid_path = tmp.path().join("chimera.pid");

    {
        let _lock = PidLock::acquire(&pid_path).unwrap();
        assert!(pid_path.exists());
    }
    // Drop guard should have removed it
    assert!(!pid_path.exists());
}

#[test]
fn acquire_lock_fails_with_running_pid() {
    let tmp = TempDir::new().unwrap();
    let pid_path = tmp.path().join("chimera.pid");

    // PID 1 is init/launchd — always alive
    std::fs::write(&pid_path, "1").unwrap();

    let result = PidLock::acquire(&pid_path);
    assert!(result.is_err());

    let err = result.unwrap_err().to_string();
    assert!(err.contains("already running"), "got: {err}");
}

// --- State file tests ---

#[test]
fn state_file_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("state.json");

    let now = Utc::now();
    let mut runners = HashMap::new();
    runners.insert(
        "runner-0".to_string(),
        RunnerStatus {
            phase: RunnerPhase::Idle,
            current_job: None,
            last_error: None,
            started_at: now,
            phase_changed_at: now,
        },
    );
    runners.insert(
        "runner-1".to_string(),
        RunnerStatus {
            phase: RunnerPhase::Stopped,
            current_job: None,
            last_error: Some("bad credentials".into()),
            started_at: now,
            phase_changed_at: now,
        },
    );

    let snapshot = StateSnapshot {
        pid: 12345,
        started_at: now,
        runners,
    };

    write_state_file(&path, &snapshot).unwrap();
    let loaded = read_state_file(&path).unwrap();

    assert_eq!(loaded.pid, 12345);
    assert_eq!(loaded.runners.len(), 2);
    assert_eq!(loaded.runners["runner-0"].phase, RunnerPhase::Idle);
    assert_eq!(loaded.runners["runner-1"].phase, RunnerPhase::Stopped);
    assert_eq!(
        loaded.runners["runner-1"].last_error.as_deref(),
        Some("bad credentials")
    );
}

#[test]
fn state_file_atomic_write() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("state.json");

    let snapshot = StateSnapshot {
        pid: 1,
        started_at: Utc::now(),
        runners: HashMap::new(),
    };

    write_state_file(&path, &snapshot).unwrap();

    assert!(path.exists());
    assert!(!path.with_extension("json.tmp").exists());
}

#[test]
fn state_file_with_job_info() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("state.json");

    let now = Utc::now();
    let mut runners = HashMap::new();
    runners.insert(
        "runner-0".to_string(),
        RunnerStatus {
            phase: RunnerPhase::Running,
            current_job: Some(JobInfo {
                repo: "org/repo".into(),
                job_id: "job-123".into(),
                started_at: now,
            }),
            last_error: None,
            started_at: now,
            phase_changed_at: now,
        },
    );

    let snapshot = StateSnapshot {
        pid: 42,
        started_at: now,
        runners,
    };

    write_state_file(&path, &snapshot).unwrap();
    let loaded = read_state_file(&path).unwrap();

    let job = loaded.runners["runner-0"].current_job.as_ref().unwrap();
    assert_eq!(job.repo, "org/repo");
    assert_eq!(job.job_id, "job-123");
}

// --- RunnerStatus / phase transition tests ---

#[tokio::test]
async fn set_phase_updates_status() {
    let state = DaemonState::new(&["runner-0".into()]);

    state.set_phase("runner-0", RunnerPhase::Idle).await;

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.runners["runner-0"].phase, RunnerPhase::Idle);
}

#[tokio::test]
async fn set_job_clears_on_idle() {
    let state = DaemonState::new(&["runner-0".into()]);

    state
        .set_running(
            "runner-0",
            JobInfo {
                repo: "org/repo".into(),
                job_id: "j1".into(),
                started_at: Utc::now(),
            },
        )
        .await;

    let snapshot = state.snapshot().await;
    assert!(snapshot.runners["runner-0"].current_job.is_some());

    state.set_phase("runner-0", RunnerPhase::Idle).await;

    let snapshot = state.snapshot().await;
    assert_eq!(snapshot.runners["runner-0"].phase, RunnerPhase::Idle);
    assert!(snapshot.runners["runner-0"].current_job.is_none());
}

#[tokio::test]
async fn concurrent_phase_updates() {
    let names: Vec<String> = (0..10).map(|i| format!("runner-{i}")).collect();
    let state = Arc::new(DaemonState::new(&names));

    let mut handles = Vec::new();
    for name in &names {
        let state = Arc::clone(&state);
        let name = name.clone();
        handles.push(tokio::spawn(async move {
            state.set_phase(&name, RunnerPhase::Idle).await;
            state.set_phase(&name, RunnerPhase::Running).await;
            state.set_phase(&name, RunnerPhase::Idle).await;
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let snapshot = state.snapshot().await;
    for name in &names {
        assert_eq!(snapshot.runners[name].phase, RunnerPhase::Idle);
    }
}

// --- PID liveness check tests ---

#[test]
fn is_process_alive_for_current_process() {
    assert!(is_process_alive(std::process::id()));
}

#[test]
fn is_process_alive_for_dead_process() {
    assert!(!is_process_alive(1_000_000_000));
}

// --- Status display tests ---

#[test]
fn format_runner_status_idle() {
    let now = Utc::now();
    let status = RunnerStatus {
        phase: RunnerPhase::Idle,
        current_job: None,
        last_error: None,
        started_at: now,
        phase_changed_at: now,
    };

    let line = format_runner_line(&status);
    assert!(line.starts_with("Idle ("), "got: {line}");
}

#[test]
fn format_runner_status_running_job() {
    let now = Utc::now();
    let status = RunnerStatus {
        phase: RunnerPhase::Running,
        current_job: Some(JobInfo {
            repo: "org/repo".into(),
            job_id: "j1".into(),
            started_at: now,
        }),
        last_error: None,
        started_at: now,
        phase_changed_at: now,
    };

    let line = format_runner_line(&status);
    assert!(line.contains("org/repo"), "got: {line}");
    assert!(line.starts_with("Running job"), "got: {line}");
}

#[test]
fn format_runner_status_stopped_with_error() {
    let now = Utc::now();
    let status = RunnerStatus {
        phase: RunnerPhase::Stopped,
        current_job: None,
        last_error: Some("bad credentials".into()),
        started_at: now,
        phase_changed_at: now,
    };

    let line = format_runner_line(&status);
    assert!(line.contains("bad credentials"), "got: {line}");
    assert!(line.contains("Stopped"), "got: {line}");
}
