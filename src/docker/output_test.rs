use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use super::OutputProcessor;
use crate::job::execute::JobState;
use crate::job::logs::{LogLine, LogSender};

fn make_processor(debug_enabled: bool) -> (OutputProcessor, tokio::sync::mpsc::Receiver<LogLine>) {
    let masks = Arc::new(RwLock::new(Vec::new()));
    let (tx, rx) = tokio::sync::mpsc::channel(256);
    let sender = LogSender::new_for_test(tx, masks.clone());
    let processor = OutputProcessor::new(sender, masks, debug_enabled);
    (processor, rx)
}

fn make_job_state() -> JobState {
    let masks = Arc::new(RwLock::new(Vec::new()));
    JobState::new(masks, HashMap::new(), serde_json::json!({}))
}

#[tokio::test]
async fn plain_line_forwarded() {
    let (proc, mut rx) = make_processor(false);
    proc.process_line("hello world").await;
    assert_eq!(rx.recv().await.unwrap().content, "hello world");
}

#[tokio::test]
async fn set_env_collected() {
    let (proc, _rx) = make_processor(false);
    proc.process_line("::set-env name=FOO::bar").await;

    let mut state = make_job_state();
    proc.apply_to_job_state(&mut state).await;
    assert_eq!(state.env.get("FOO").unwrap(), "bar");
}

#[tokio::test]
async fn set_output_collected() {
    let (proc, _rx) = make_processor(false);
    proc.process_line("::set-output name=result::42").await;

    let mut state = make_job_state();
    proc.apply_to_job_state(&mut state).await;
    assert_eq!(state.outputs.get("result").unwrap(), "42");
}

#[tokio::test]
async fn add_path_collected() {
    let (proc, _rx) = make_processor(false);
    proc.process_line("::add-path::/usr/local/bin").await;

    let mut state = make_job_state();
    proc.apply_to_job_state(&mut state).await;
    assert_eq!(state.path_prepends, vec!["/usr/local/bin"]);
}

#[tokio::test]
async fn add_mask_causes_masking() {
    let (proc, mut rx) = make_processor(false);
    proc.process_line("::add-mask::supersecret").await;
    proc.process_line("the supersecret value is here").await;

    // The LogSender masks content before sending, so the secret should be replaced
    assert_eq!(rx.recv().await.unwrap().content, "the *** value is here");
}

#[tokio::test]
async fn save_state_collected() {
    let (proc, _rx) = make_processor(false);
    proc.process_line("::save-state name=key::val").await;

    let mut state = make_job_state();
    proc.apply_to_job_state(&mut state).await;
    let bucket = state.action_states.get("").unwrap();
    assert_eq!(bucket.get("key").unwrap(), "val");
}

#[tokio::test]
async fn warning_forwarded() {
    let (proc, mut rx) = make_processor(false);
    proc.process_line("::warning::something fishy").await;
    assert_eq!(
        rx.recv().await.unwrap().content,
        "##[warning]something fishy"
    );
}

#[tokio::test]
async fn error_forwarded() {
    let (proc, mut rx) = make_processor(false);
    proc.process_line("::error::oh no").await;
    assert_eq!(rx.recv().await.unwrap().content, "##[error]oh no");
}

#[tokio::test]
async fn group_and_endgroup_forwarded() {
    let (proc, mut rx) = make_processor(false);
    proc.process_line("::group::My Group").await;
    proc.process_line("::endgroup::").await;
    assert_eq!(rx.recv().await.unwrap().content, "##[group]My Group");
    assert_eq!(rx.recv().await.unwrap().content, "##[endgroup]");
}

#[tokio::test]
async fn debug_suppressed_when_disabled() {
    let (proc, mut rx) = make_processor(false);
    proc.process_line("::debug::secret info").await;
    proc.process_line("visible line").await;

    // Only the plain line should come through
    assert_eq!(rx.recv().await.unwrap().content, "visible line");
}

#[tokio::test]
async fn debug_forwarded_when_enabled() {
    let (proc, mut rx) = make_processor(true);
    proc.process_line("::debug::secret info").await;
    assert_eq!(rx.recv().await.unwrap().content, "##[debug]secret info");
}

#[tokio::test]
async fn apply_drains_buffers() {
    let (proc, _rx) = make_processor(false);
    proc.process_line("::set-env name=A::1").await;
    proc.process_line("::set-output name=B::2").await;

    let mut state = make_job_state();
    proc.apply_to_job_state(&mut state).await;
    assert_eq!(state.env.get("A").unwrap(), "1");
    assert_eq!(state.outputs.get("B").unwrap(), "2");

    // Second apply should find empty buffers
    let mut state2 = make_job_state();
    proc.apply_to_job_state(&mut state2).await;
    assert!(state2.env.is_empty());
    assert!(state2.outputs.is_empty());
}
