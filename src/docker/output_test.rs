use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use bollard::container::LogOutput;

use super::{LineAssembler, OutputProcessor};
use crate::job::commands::{
    ALLOW_UNSECURE_COMMANDS_ENV, ALLOW_UNSECURE_STOP_TOKENS_ENV, CommandPolicy,
};
use crate::job::execute::JobState;
use crate::job::logs::{LogLine, LogSender};

fn make_processor(debug_enabled: bool) -> (OutputProcessor, tokio::sync::mpsc::Receiver<LogLine>) {
    build_processor(debug_enabled, CommandPolicy::default())
}

fn make_unsecure_processor() -> (OutputProcessor, tokio::sync::mpsc::Receiver<LogLine>) {
    make_processor_with_flag(ALLOW_UNSECURE_COMMANDS_ENV)
}

/// A processor for a step whose env sets `flag=true`.
fn make_processor_with_flag(flag: &str) -> (OutputProcessor, tokio::sync::mpsc::Receiver<LogLine>) {
    let env = HashMap::from([(flag.to_string(), "true".to_string())]);
    build_processor(false, CommandPolicy::from_env(&env))
}

fn build_processor(
    debug_enabled: bool,
    policy: CommandPolicy,
) -> (OutputProcessor, tokio::sync::mpsc::Receiver<LogLine>) {
    let masks = Arc::new(RwLock::new(Vec::new()));
    let (tx, rx) = tokio::sync::mpsc::channel(256);
    let sender = LogSender::new_for_test(tx, masks.clone());
    let processor = OutputProcessor::new(sender, masks, debug_enabled, policy);
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
async fn set_env_collected_when_unsecure_allowed() {
    let (proc, _rx) = make_unsecure_processor();
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
async fn add_path_collected_when_unsecure_allowed() {
    let (proc, _rx) = make_unsecure_processor();
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
    let (proc, _rx) = make_unsecure_processor();
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

#[tokio::test]
async fn set_env_rejected_by_default() {
    let (proc, mut rx) = make_processor(false);
    proc.process_line("::set-env name=LD_PRELOAD::/tmp/evil.so")
        .await;

    let mut state = make_job_state();
    proc.apply_to_job_state(&mut state).await;

    assert!(state.env.is_empty());
    assert!(proc.command_failed());
    assert_eq!(
        rx.recv().await.unwrap().content,
        "##[error]Unable to process command '::set-env name=LD_PRELOAD::/tmp/evil.so' successfully."
    );
    assert!(
        rx.recv()
            .await
            .unwrap()
            .content
            .contains("`set-env` command is disabled")
    );
}

#[tokio::test]
async fn add_path_rejected_by_default() {
    let (proc, _rx) = make_processor(false);
    proc.process_line("::add-path::/tmp/evil").await;

    let mut state = make_job_state();
    proc.apply_to_job_state(&mut state).await;
    assert!(state.path_prepends.is_empty());
    assert!(proc.command_failed());
}

#[tokio::test]
async fn safe_commands_do_not_fail_step() {
    let (proc, _rx) = make_processor(false);
    proc.process_line("::set-output name=a::1").await;
    proc.process_line("::warning::careful").await;

    assert!(!proc.command_failed());
}

#[tokio::test]
async fn stop_commands_ignores_commands_until_token() {
    let (proc, mut rx) = make_processor(false);
    proc.process_line("::stop-commands::tok123").await;
    proc.process_line("::set-output name=injected::x").await;
    proc.process_line("::add-mask::not-a-mask").await;
    proc.process_line("::tok123::").await;
    proc.process_line("::set-output name=real::y").await;

    let mut state = make_job_state();
    proc.apply_to_job_state(&mut state).await;
    assert!(!state.outputs.contains_key("injected"));
    assert_eq!(state.outputs.get("real").unwrap(), "y");
    assert_eq!(rx.recv().await.unwrap().content, "::stop-commands::tok123");
    assert_eq!(
        rx.recv().await.unwrap().content,
        "::set-output name=injected::x"
    );
    assert_eq!(rx.recv().await.unwrap().content, "::add-mask::not-a-mask");
    assert_eq!(rx.recv().await.unwrap().content, "::tok123::");
}

#[tokio::test]
async fn stop_commands_state_shared_between_clones() {
    let (proc, _rx) = make_processor(false);
    let stderr_proc = proc.clone();
    proc.process_line("::stop-commands::tok123").await;
    stderr_proc
        .process_line("::set-output name=injected::x")
        .await;

    let mut state = make_job_state();
    proc.apply_to_job_state(&mut state).await;
    assert!(state.outputs.is_empty());
}

#[tokio::test]
async fn stop_commands_rejects_weak_token() {
    let (proc, _rx) = make_processor(false);
    proc.process_line("::stop-commands::pause-logging").await;
    proc.process_line("::set-output name=still::processed")
        .await;

    let mut state = make_job_state();
    proc.apply_to_job_state(&mut state).await;
    assert!(proc.command_failed());
    assert_eq!(state.outputs.get("still").unwrap(), "processed");
}

#[tokio::test]
async fn stop_commands_resume_is_case_insensitive() {
    let (proc, _rx) = make_processor(false);
    proc.process_line("::stop-commands::Tok123").await;
    proc.process_line("::TOK123::").await;

    proc.process_line("::set-output name=real::y").await;

    let mut state = make_job_state();
    proc.apply_to_job_state(&mut state).await;
    assert_eq!(state.outputs.get("real").unwrap(), "y");
}

#[tokio::test]
async fn stop_commands_resume_allows_leading_whitespace_and_trailing_data() {
    let (proc, _rx) = make_processor(false);
    proc.process_line("::stop-commands::tok123").await;
    proc.process_line("  ::tok123::anything here").await;

    proc.process_line("::set-output name=real::y").await;

    let mut state = make_job_state();
    proc.apply_to_job_state(&mut state).await;
    assert_eq!(state.outputs.get("real").unwrap(), "y");
}

#[tokio::test]
async fn stop_commands_rejects_command_name_token() {
    let (proc, _rx) = make_processor(false);

    proc.process_line("::stop-commands::Set-Output").await;
    proc.process_line("::set-output name=still::processed")
        .await;

    let mut state = make_job_state();
    proc.apply_to_job_state(&mut state).await;
    assert!(proc.command_failed());
    assert_eq!(state.outputs.get("still").unwrap(), "processed");
}

#[tokio::test]
async fn stop_commands_rejects_empty_token() {
    let (proc, _rx) = make_processor(false);

    proc.process_line("::stop-commands::").await;

    assert!(proc.command_failed());
}

#[tokio::test]
async fn unsecure_stop_tokens_flag_allows_weak_token() {
    let (proc, _rx) = make_processor_with_flag(ALLOW_UNSECURE_STOP_TOKENS_ENV);

    proc.process_line("::stop-commands::pause-logging").await;
    proc.process_line("::set-output name=injected::x").await;

    let mut state = make_job_state();
    proc.apply_to_job_state(&mut state).await;
    assert!(!proc.command_failed());
    assert!(state.outputs.is_empty());
}

#[tokio::test]
async fn unsecure_commands_flag_does_not_allow_weak_token() {
    let (proc, _rx) = make_unsecure_processor();

    proc.process_line("::stop-commands::pause-logging").await;

    assert!(proc.command_failed());
}

#[tokio::test]
async fn unsecure_stop_tokens_flag_does_not_allow_set_env() {
    let (proc, _rx) = make_processor_with_flag(ALLOW_UNSECURE_STOP_TOKENS_ENV);

    proc.process_line("::set-env name=FOO::bar").await;

    assert!(proc.command_failed());
}

fn stdout(text: &[u8]) -> LogOutput {
    LogOutput::StdOut {
        message: text.to_vec().into(),
    }
}

fn stderr(text: &[u8]) -> LogOutput {
    LogOutput::StdErr {
        message: text.to_vec().into(),
    }
}

#[test]
fn assembler_joins_a_line_split_across_frames() {
    let mut assembler = LineAssembler::default();

    let first = assembler.push(stdout(b"::set-output name=x::"));
    let second = assembler.push(stdout(b"value\n"));

    assert!(first.is_empty());
    assert_eq!(second, vec!["::set-output name=x::value"]);
}

#[test]
fn assembler_splits_a_frame_with_several_lines() {
    let mut assembler = LineAssembler::default();

    let lines = assembler.push(stdout(b"one\n\ntwo\nthr"));

    assert_eq!(lines, vec!["one", "", "two"]);
    assert_eq!(assembler.finish(), vec!["thr"]);
}

#[test]
fn assembler_keeps_stdout_and_stderr_partials_apart() {
    let mut assembler = LineAssembler::default();

    assembler.push(stdout(b"out-"));
    let from_stderr = assembler.push(stderr(b"err\n"));
    let from_stdout = assembler.push(stdout(b"line\n"));

    assert_eq!(from_stderr, vec!["err"]);
    assert_eq!(from_stdout, vec!["out-line"]);
}

#[test]
fn assembler_keeps_a_utf8_character_split_across_frames() {
    let mut assembler = LineAssembler::default();
    let bytes = "caf\u{e9}\n".as_bytes();

    assembler.push(stdout(&bytes[..4]));
    let lines = assembler.push(stdout(&bytes[4..]));

    assert_eq!(lines, vec!["caf\u{e9}"]);
}

#[test]
fn assembler_strips_carriage_returns() {
    let mut assembler = LineAssembler::default();

    let lines = assembler.push(stdout(b"windows\r\n"));

    assert_eq!(lines, vec!["windows"]);
}

#[test]
fn assembler_finish_is_empty_after_complete_lines() {
    let mut assembler = LineAssembler::default();

    assembler.push(stdout(b"done\n"));

    assert!(assembler.finish().is_empty());
}
