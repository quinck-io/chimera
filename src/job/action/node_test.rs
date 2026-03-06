use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use super::*;
use crate::job::execute::{JobState, StepConclusion};
use crate::job::logs::StepLogger;
use crate::job::schema::{Step, StepReference};
use crate::job::workspace::Workspace;
use tokio_util::sync::CancellationToken;

fn make_test_workspace(tmp: &tempfile::TempDir) -> Workspace {
    Workspace::create(
        &tmp.path().join("work"),
        &tmp.path().join("tmp"),
        &tmp.path().join("tool-cache"),
        "test-runner",
        "owner/repo",
    )
    .unwrap()
}

fn make_action_step(name: &str) -> Step {
    Step {
        id: "1".into(),
        display_name: name.into(),
        reference: StepReference {
            name: "test-owner/test-action".into(),
            kind: "repository".into(),
            git_ref: Some("v1".into()),
            ..Default::default()
        },
        inputs: HashMap::new(),
        condition: None,
        timeout_in_minutes: None,
        continue_on_error: false,
        order: 1,
        environment: None,
        context_name: None,
    }
}

fn make_node_metadata(main_script: &str) -> ActionMetadata {
    ActionMetadata {
        name: Some("Test Action".into()),
        inputs: HashMap::new(),

        runs: crate::job::action::metadata::ActionRuns {
            using: "node20".into(),
            main: Some(main_script.into()),
            pre: None,
            post: None,
            steps: None,
        },
    }
}

#[tokio::test]
async fn node_action_executes_script() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_test_workspace(&tmp);

    let action_dir = tmp.path().join("action");
    std::fs::create_dir_all(&action_dir).unwrap();
    std::fs::write(
        action_dir.join("index.js"),
        "console.log('hello from action');",
    )
    .unwrap();

    let metadata = make_node_metadata("index.js");
    let step = make_action_step("Test");
    let mut state = JobState::new(
        Arc::new(RwLock::new(Vec::new())),
        HashMap::new(),
        serde_json::json!({}),
    );
    let masks = Arc::new(RwLock::new(Vec::new()));
    let logger = StepLogger::results(masks);
    let base_env = HashMap::new();

    let result = run_node_action(
        &action_dir,
        &metadata,
        "main",
        &step,
        &mut state,
        &ws,
        &base_env,
        logger.sender(),
        &CancellationToken::new(),
    )
    .await;

    // May fail if node is not installed — skip gracefully
    match result {
        Ok(r) => assert_eq!(r.conclusion, StepConclusion::Succeeded),
        Err(e) if e.to_string().contains("spawning node") => {
            eprintln!("skipping test: node not found on PATH");
        }
        Err(e) => panic!("unexpected error: {e}"),
    }
}

#[tokio::test]
async fn input_env_vars_set() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_test_workspace(&tmp);

    let action_dir = tmp.path().join("action");
    std::fs::create_dir_all(&action_dir).unwrap();
    // Script that checks INPUT_TOKEN is set and prints it
    std::fs::write(
        action_dir.join("index.js"),
        r#"
if (process.env.INPUT_TOKEN !== 'my-secret') {
    process.exit(1);
}
"#,
    )
    .unwrap();

    let mut metadata = make_node_metadata("index.js");
    metadata.inputs.insert(
        "token".into(),
        crate::job::action::metadata::ActionInput { default: None },
    );

    let mut step = make_action_step("Test");
    step.inputs.insert("token".into(), "my-secret".into());

    let mut state = JobState::new(
        Arc::new(RwLock::new(Vec::new())),
        HashMap::new(),
        serde_json::json!({}),
    );
    let masks = Arc::new(RwLock::new(Vec::new()));
    let logger = StepLogger::results(masks);
    let base_env = HashMap::new();

    let result = run_node_action(
        &action_dir,
        &metadata,
        "main",
        &step,
        &mut state,
        &ws,
        &base_env,
        logger.sender(),
        &CancellationToken::new(),
    )
    .await;

    match result {
        Ok(r) => assert_eq!(r.conclusion, StepConclusion::Succeeded),
        Err(e) if e.to_string().contains("spawning node") => {
            eprintln!("skipping test: node not found on PATH");
        }
        Err(e) => panic!("unexpected error: {e}"),
    }
}

#[tokio::test]
async fn defaults_used_when_no_step_input() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_test_workspace(&tmp);

    let action_dir = tmp.path().join("action");
    std::fs::create_dir_all(&action_dir).unwrap();
    std::fs::write(
        action_dir.join("index.js"),
        r#"
if (process.env.INPUT_FLAVOR !== 'vanilla') {
    process.exit(1);
}
"#,
    )
    .unwrap();

    let mut metadata = make_node_metadata("index.js");
    metadata.inputs.insert(
        "flavor".into(),
        crate::job::action::metadata::ActionInput {
            default: Some("vanilla".into()),
        },
    );

    let step = make_action_step("Test");
    let mut state = JobState::new(
        Arc::new(RwLock::new(Vec::new())),
        HashMap::new(),
        serde_json::json!({}),
    );
    let masks = Arc::new(RwLock::new(Vec::new()));
    let logger = StepLogger::results(masks);
    let base_env = HashMap::new();

    let result = run_node_action(
        &action_dir,
        &metadata,
        "main",
        &step,
        &mut state,
        &ws,
        &base_env,
        logger.sender(),
        &CancellationToken::new(),
    )
    .await;

    match result {
        Ok(r) => assert_eq!(r.conclusion, StepConclusion::Succeeded),
        Err(e) if e.to_string().contains("spawning node") => {
            eprintln!("skipping test: node not found on PATH");
        }
        Err(e) => panic!("unexpected error: {e}"),
    }
}

#[tokio::test]
async fn nonzero_exit_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_test_workspace(&tmp);

    let action_dir = tmp.path().join("action");
    std::fs::create_dir_all(&action_dir).unwrap();
    std::fs::write(action_dir.join("index.js"), "process.exit(1);").unwrap();

    let metadata = make_node_metadata("index.js");
    let step = make_action_step("Test");
    let mut state = JobState::new(
        Arc::new(RwLock::new(Vec::new())),
        HashMap::new(),
        serde_json::json!({}),
    );
    let masks = Arc::new(RwLock::new(Vec::new()));
    let logger = StepLogger::results(masks);
    let base_env = HashMap::new();

    let result = run_node_action(
        &action_dir,
        &metadata,
        "main",
        &step,
        &mut state,
        &ws,
        &base_env,
        logger.sender(),
        &CancellationToken::new(),
    )
    .await;

    match result {
        Ok(r) => assert_eq!(r.conclusion, StepConclusion::Failed),
        Err(e) if e.to_string().contains("spawning node") => {
            eprintln!("skipping test: node not found on PATH");
        }
        Err(e) => panic!("unexpected error: {e}"),
    }
}
