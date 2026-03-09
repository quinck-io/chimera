use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use super::*;
use crate::job::action::download::ActionCache;
use crate::job::action::metadata::{ActionInput, ActionMetadata, ActionRuns};
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

fn make_composite_metadata(steps_yaml: &str) -> ActionMetadata {
    let steps: Vec<serde_yaml::Value> = serde_yaml::from_str(steps_yaml).unwrap();
    ActionMetadata {
        name: Some("Composite Test".into()),
        inputs: HashMap::new(),

        runs: ActionRuns {
            using: crate::job::action::metadata::ActionRuntime::Composite,
            main: None,
            pre: None,
            post: None,
            steps: Some(steps),
        },
    }
}

fn make_step() -> Step {
    Step {
        id: "1".into(),
        display_name: "Composite".into(),
        reference: StepReference {
            name: "test/composite".into(),
            kind: crate::job::schema::StepReferenceKind::Repository,
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

#[tokio::test]
async fn nested_script_steps_execute() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_test_workspace(&tmp);
    let action_dir = tmp.path().join("action");
    std::fs::create_dir_all(&action_dir).unwrap();

    let metadata = make_composite_metadata(
        r#"
- run: echo "step one"
  shell: bash
- run: echo "step two"
  shell: bash
"#,
    );

    let step = make_step();
    let mut state = JobState::new(
        Arc::new(RwLock::new(Vec::new())),
        HashMap::new(),
        serde_json::json!({}),
    );
    let masks = Arc::new(RwLock::new(Vec::new()));
    let logger = StepLogger::results(masks);
    let cache = ActionCache::new(tmp.path().join("cache"), reqwest::Client::new());
    let base_env = HashMap::new();

    let result = run_composite_action(
        &action_dir,
        &metadata,
        &step,
        &mut state,
        &ws,
        &base_env,
        logger.sender(),
        &cache,
        "fake-token",
        0,
        &CancellationToken::new(),
        None,
        Path::new("node"),
    )
    .await
    .unwrap();

    assert_eq!(result.conclusion, StepConclusion::Succeeded);
}

#[tokio::test]
async fn failure_propagates() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_test_workspace(&tmp);
    let action_dir = tmp.path().join("action");
    std::fs::create_dir_all(&action_dir).unwrap();

    let metadata = make_composite_metadata(
        r#"
- run: exit 1
  shell: bash
- run: echo "should not run"
  shell: bash
"#,
    );

    let step = make_step();
    let mut state = JobState::new(
        Arc::new(RwLock::new(Vec::new())),
        HashMap::new(),
        serde_json::json!({}),
    );
    let masks = Arc::new(RwLock::new(Vec::new()));
    let logger = StepLogger::results(masks);
    let cache = ActionCache::new(tmp.path().join("cache"), reqwest::Client::new());
    let base_env = HashMap::new();

    let result = run_composite_action(
        &action_dir,
        &metadata,
        &step,
        &mut state,
        &ws,
        &base_env,
        logger.sender(),
        &cache,
        "fake-token",
        0,
        &CancellationToken::new(),
        None,
        Path::new("node"),
    )
    .await
    .unwrap();

    assert_eq!(result.conclusion, StepConclusion::Failed);
}

#[tokio::test]
async fn inputs_available_as_env() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_test_workspace(&tmp);
    let action_dir = tmp.path().join("action");
    std::fs::create_dir_all(&action_dir).unwrap();

    let mut metadata = make_composite_metadata(
        r#"
- run: test "$INPUT_NAME" = "world"
  shell: bash
"#,
    );
    metadata
        .inputs
        .insert("name".into(), ActionInput { default: None });

    let mut step = make_step();
    step.inputs.insert("name".into(), "world".into());

    let mut state = JobState::new(
        Arc::new(RwLock::new(Vec::new())),
        HashMap::new(),
        serde_json::json!({}),
    );
    let masks = Arc::new(RwLock::new(Vec::new()));
    let logger = StepLogger::results(masks);
    let cache = ActionCache::new(tmp.path().join("cache"), reqwest::Client::new());
    let base_env = HashMap::new();

    let result = run_composite_action(
        &action_dir,
        &metadata,
        &step,
        &mut state,
        &ws,
        &base_env,
        logger.sender(),
        &cache,
        "fake-token",
        0,
        &CancellationToken::new(),
        None,
        Path::new("node"),
    )
    .await
    .unwrap();

    assert_eq!(result.conclusion, StepConclusion::Succeeded);
}

#[tokio::test]
async fn recursion_depth_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = make_test_workspace(&tmp);
    let action_dir = tmp.path().join("action");
    std::fs::create_dir_all(&action_dir).unwrap();

    let metadata = make_composite_metadata(
        r#"
- run: echo "hi"
  shell: bash
"#,
    );

    let step = make_step();
    let mut state = JobState::new(
        Arc::new(RwLock::new(Vec::new())),
        HashMap::new(),
        serde_json::json!({}),
    );
    let masks = Arc::new(RwLock::new(Vec::new()));
    let logger = StepLogger::results(masks);
    let cache = ActionCache::new(tmp.path().join("cache"), reqwest::Client::new());
    let base_env = HashMap::new();

    let result = run_composite_action(
        &action_dir,
        &metadata,
        &step,
        &mut state,
        &ws,
        &base_env,
        logger.sender(),
        &cache,
        "fake-token",
        10, // Already at limit
        &CancellationToken::new(),
        None,
        Path::new("node"),
    )
    .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("recursion depth"));
}
