use super::*;
use crate::github::auth::TokenManager;
use crate::job::action::ActionCache;
use crate::job::client::JobConclusion;
use crate::job::schema::{StepReference, StepReferenceKind};
use rsa::RsaPrivateKey;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn setup_execute() -> (tempfile::TempDir, Workspace, Arc<JobClient>, MockServer) {
    let tmp = tempfile::tempdir().unwrap();
    let work_dir = tmp.path().join("work");
    let tmp_dir = tmp.path().join("tmp");
    let tool_cache = tmp.path().join("tool-cache");

    let ws = Workspace::create(
        &work_dir,
        &tmp_dir,
        &tool_cache,
        "test-runner",
        "owner/repo",
    )
    .unwrap();

    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "test-token",
            "expires_in": 7200
        })))
        .mount(&mock_server)
        .await;

    // Mock log create
    Mock::given(method("POST"))
        .and(path_regex(r"/_apis/pipelines/workflows/.*/logs$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 1})))
        .mount(&mock_server)
        .await;

    // Mock log upload
    Mock::given(method("POST"))
        .and(path_regex(r"/_apis/pipelines/workflows/.*/logs/\d+"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;

    // Mock timeline update
    Mock::given(method("PATCH"))
        .and(path_regex(
            r"/_apis/distributedtask/hubs/build/plans/.*/timelines/.*",
        ))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;

    let private_key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let tm = Arc::new(TokenManager::new(
        reqwest::Client::new(),
        format!("{}/oauth2/token", mock_server.uri()),
        private_key,
        "test-client".into(),
    ));

    let mut job_client = JobClient::new(
        reqwest::Client::new(),
        tm,
        mock_server.uri(),
        mock_server.uri(),
    );
    job_client.set_job_access_token("test-job-token".into());

    (tmp, ws, Arc::new(job_client), mock_server)
}

fn make_step(id: &str, script: &str) -> Step {
    Step {
        id: id.into(),
        display_name: format!("Run {script}"),
        reference: StepReference {
            name: "script".into(),
            kind: StepReferenceKind::Script,
            ..Default::default()
        },
        inputs: HashMap::from([("script".into(), script.into())]),
        condition: None,
        timeout_in_minutes: None,
        continue_on_error: false,
        order: 1,
        environment: None,
        context_name: None,
    }
}

#[tokio::test]
async fn echo_step_stdout_captured() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = Arc::new(RwLock::new(Vec::new()));
    let logger = StepLogger::legacy(client, "plan", "step", masks, None).await;

    let step = make_step("1", "echo hello world");
    let mut state = JobState::new(
        Arc::new(RwLock::new(Vec::new())),
        HashMap::new(),
        serde_json::json!({}),
    );
    let base_env = HashMap::new();

    let result = run_host_step(
        &step,
        &mut state,
        &ws,
        &base_env,
        logger.sender(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.conclusion, StepConclusion::Succeeded);

    drop(logger);
}

#[tokio::test]
async fn nonzero_exit_returns_failed() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = Arc::new(RwLock::new(Vec::new()));
    let logger = StepLogger::legacy(client, "plan", "step", masks, None).await;

    let step = make_step("1", "exit 1");
    let mut state = JobState::new(
        Arc::new(RwLock::new(Vec::new())),
        HashMap::new(),
        serde_json::json!({}),
    );
    let base_env = HashMap::new();

    let result = run_host_step(
        &step,
        &mut state,
        &ws,
        &base_env,
        logger.sender(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.conclusion, StepConclusion::Failed);

    drop(logger);
}

#[tokio::test]
async fn set_env_updates_job_state() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = Arc::new(RwLock::new(Vec::new()));
    let logger = StepLogger::legacy(client, "plan", "step", masks, None).await;

    let step = make_step("1", "echo '::set-env name=MY_KEY::my_val'");
    let mut state = JobState::new(
        Arc::new(RwLock::new(Vec::new())),
        HashMap::new(),
        serde_json::json!({}),
    );
    let base_env = HashMap::new();

    run_host_step(
        &step,
        &mut state,
        &ws,
        &base_env,
        logger.sender(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(state.env.get("MY_KEY").unwrap(), "my_val");

    drop(logger);
}

#[tokio::test]
async fn add_path_updates_path() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = Arc::new(RwLock::new(Vec::new()));
    let logger = StepLogger::legacy(client, "plan", "step", masks, None).await;

    let step = make_step("1", "echo '::add-path::/opt/custom/bin'");
    let mut state = JobState::new(
        Arc::new(RwLock::new(Vec::new())),
        HashMap::new(),
        serde_json::json!({}),
    );
    let base_env = HashMap::new();

    run_host_step(
        &step,
        &mut state,
        &ws,
        &base_env,
        logger.sender(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(state.path_prepends.contains(&"/opt/custom/bin".to_string()));

    drop(logger);
}

#[tokio::test]
async fn set_output_populates_outputs() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = Arc::new(RwLock::new(Vec::new()));
    let logger = StepLogger::legacy(client, "plan", "step", masks, None).await;

    let step = make_step("1", "echo '::set-output name=result::42'");
    let mut state = JobState::new(
        Arc::new(RwLock::new(Vec::new())),
        HashMap::new(),
        serde_json::json!({}),
    );
    let base_env = HashMap::new();

    run_host_step(
        &step,
        &mut state,
        &ws,
        &base_env,
        logger.sender(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(state.outputs.get("result").unwrap(), "42");

    drop(logger);
}

#[tokio::test]
async fn env_propagation_across_steps() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = Arc::new(RwLock::new(Vec::new()));
    let logger = StepLogger::legacy(client, "plan", "step", masks, None).await;

    let step1 = make_step("1", "echo '::set-env name=STEP1_VAR::hello'");
    let mut state = JobState::new(
        Arc::new(RwLock::new(Vec::new())),
        HashMap::new(),
        serde_json::json!({}),
    );
    let base_env = HashMap::new();

    run_host_step(
        &step1,
        &mut state,
        &ws,
        &base_env,
        logger.sender(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();

    let step2 = make_step("2", "test \"$STEP1_VAR\" = \"hello\"");
    let result = run_host_step(
        &step2,
        &mut state,
        &ws,
        &base_env,
        logger.sender(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.conclusion, StepConclusion::Succeeded);

    drop(logger);
}

#[tokio::test]
async fn continue_on_error_works() {
    let (tmp, ws, client, _mock) = setup_execute().await;

    let manifest_json = r#"{
        "plan": { "planId": "p", "jobId": "j", "timelineId": "t" },
        "steps": [
            {
                "id": "s1",
                "displayName": "Failing step",
                "reference": { "name": "script", "type": "script" },
                "inputs": { "script": "exit 1" },
                "condition": null,
                "timeoutInMinutes": null,
                "continueOnError": true,
                "order": 1,
                "environment": null
            },
            {
                "id": "s2",
                "displayName": "Should still run",
                "reference": { "name": "script", "type": "script" },
                "inputs": { "script": "echo still running" },
                "condition": null,
                "timeoutInMinutes": null,
                "continueOnError": false,
                "order": 2,
                "environment": null
            }
        ],
        "variables": {},
        "resources": { "endpoints": [] },
        "contextData": {},
        "jobContainer": null,
        "serviceContainers": null
    }"#;

    let manifest: crate::job::schema::JobManifest = serde_json::from_str(manifest_json).unwrap();
    let base_env = HashMap::new();
    let action_cache = ActionCache::new(tmp.path().join("actions"), reqwest::Client::new());

    let result = run_all_steps(
        &manifest,
        &client,
        &ws,
        &base_env,
        "test-runner",
        &action_cache,
        "fake-token",
        CancellationToken::new(),
        None,
        Path::new("node"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.0, JobConclusion::Succeeded);
}

#[tokio::test]
async fn failure_stops_remaining_steps() {
    let (tmp, ws, client, _mock) = setup_execute().await;

    let manifest_json = r#"{
        "plan": { "planId": "p", "jobId": "j", "timelineId": "t" },
        "steps": [
            {
                "id": "s1",
                "displayName": "Failing step",
                "reference": { "name": "script", "type": "script" },
                "inputs": { "script": "exit 1" },
                "condition": null,
                "timeoutInMinutes": null,
                "continueOnError": false,
                "order": 1,
                "environment": null
            },
            {
                "id": "s2",
                "displayName": "Should be skipped",
                "reference": { "name": "script", "type": "script" },
                "inputs": { "script": "echo should not run" },
                "condition": null,
                "timeoutInMinutes": null,
                "continueOnError": false,
                "order": 2,
                "environment": null
            }
        ],
        "variables": {},
        "resources": { "endpoints": [] },
        "contextData": {},
        "jobContainer": null,
        "serviceContainers": null
    }"#;

    let manifest: crate::job::schema::JobManifest = serde_json::from_str(manifest_json).unwrap();
    let base_env = HashMap::new();
    let action_cache = ActionCache::new(tmp.path().join("actions"), reqwest::Client::new());

    let result = run_all_steps(
        &manifest,
        &client,
        &ws,
        &base_env,
        "test-runner",
        &action_cache,
        "fake-token",
        CancellationToken::new(),
        None,
        Path::new("node"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.0, JobConclusion::Failed);
}

#[tokio::test]
async fn secrets_from_context_data_resolved() {
    let (tmp, ws, client, _mock) = setup_execute().await;

    let manifest_json = r#"{
        "plan": { "planId": "p", "jobId": "j", "timelineId": "t" },
        "steps": [
            {
                "id": "s1",
                "displayName": "Use secret",
                "reference": { "name": "script", "type": "script" },
                "inputs": { "script": "test -n \"$MY_SECRET\"" },
                "condition": null,
                "timeoutInMinutes": null,
                "continueOnError": false,
                "order": 1,
                "environment": { "MY_SECRET": "${{ secrets.SECRET }}" }
            }
        ],
        "variables": {},
        "resources": { "endpoints": [] },
        "contextData": {
            "secrets": {
                "SECRET": "super-secret-value"
            }
        },
        "jobContainer": null,
        "serviceContainers": null
    }"#;

    let manifest: crate::job::schema::JobManifest = serde_json::from_str(manifest_json).unwrap();
    let base_env = HashMap::new();
    let action_cache = ActionCache::new(tmp.path().join("actions"), reqwest::Client::new());

    let result = run_all_steps(
        &manifest,
        &client,
        &ws,
        &base_env,
        "test-runner",
        &action_cache,
        "fake-token",
        CancellationToken::new(),
        None,
        Path::new("node"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.0, JobConclusion::Succeeded);
}

#[tokio::test]
async fn cancel_token_returns_cancelled_between_steps() {
    let (tmp, ws, client, _mock) = setup_execute().await;

    let manifest_json = r#"{
        "plan": { "planId": "p", "jobId": "j", "timelineId": "t" },
        "steps": [
            {
                "id": "s1",
                "displayName": "First step",
                "reference": { "name": "script", "type": "script" },
                "inputs": { "script": "echo step1" },
                "condition": null,
                "timeoutInMinutes": null,
                "continueOnError": false,
                "order": 1,
                "environment": null
            },
            {
                "id": "s2",
                "displayName": "Should be cancelled",
                "reference": { "name": "script", "type": "script" },
                "inputs": { "script": "echo step2" },
                "condition": null,
                "timeoutInMinutes": null,
                "continueOnError": false,
                "order": 2,
                "environment": null
            }
        ],
        "variables": {},
        "resources": { "endpoints": [] },
        "contextData": {},
        "jobContainer": null,
        "serviceContainers": null
    }"#;

    let manifest: crate::job::schema::JobManifest = serde_json::from_str(manifest_json).unwrap();
    let base_env = HashMap::new();
    let action_cache = ActionCache::new(tmp.path().join("actions"), reqwest::Client::new());

    let cancel_token = CancellationToken::new();
    // Cancel immediately — step 1 may run but the conclusion should be "cancelled"
    cancel_token.cancel();

    let result = run_all_steps(
        &manifest,
        &client,
        &ws,
        &base_env,
        "test-runner",
        &action_cache,
        "fake-token",
        cancel_token,
        None,
        Path::new("node"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.0, JobConclusion::Cancelled);
}

#[tokio::test]
async fn cancel_token_kills_running_process() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = Arc::new(RwLock::new(Vec::new()));
    let logger = StepLogger::legacy(client, "plan", "step", masks, None).await;

    let step = make_step("1", "sleep 60");
    let mut state = JobState::new(
        Arc::new(RwLock::new(Vec::new())),
        HashMap::new(),
        serde_json::json!({}),
    );
    let base_env = HashMap::new();

    let cancel_token = CancellationToken::new();
    let cancel_clone = cancel_token.clone();

    // Cancel after a brief delay
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        cancel_clone.cancel();
    });

    let start = std::time::Instant::now();
    let result = run_host_step(
        &step,
        &mut state,
        &ws,
        &base_env,
        logger.sender(),
        &cancel_token,
    )
    .await
    .unwrap();

    // Should return quickly (well under 60s)
    assert!(start.elapsed().as_secs() < 5);
    assert_eq!(result.conclusion, StepConclusion::Cancelled);

    drop(logger);
}

#[test]
fn step_is_script_detection() {
    let script_step = make_step("1", "echo hi");
    assert!(script_step.is_script());

    let action_step = Step {
        id: "2".into(),
        display_name: "Checkout".into(),
        reference: StepReference {
            name: "actions/checkout@v4".into(),
            kind: StepReferenceKind::Unknown("action".into()),
            ..Default::default()
        },
        inputs: HashMap::new(),
        condition: None,
        timeout_in_minutes: None,
        continue_on_error: false,
        order: 1,
        environment: None,
        context_name: None,
    };
    assert!(!action_step.is_script());
}

#[test]
fn build_job_context_no_docker() {
    let ctx = build_job_context(None);
    assert_eq!(ctx["status"], "success");
    assert!(ctx.get("container").is_none());
    assert!(ctx.get("services").is_none());
}

#[test]
fn update_job_status_transitions() {
    let mut data = serde_json::json!({
        "job": { "status": "success" }
    });

    // Initially success
    update_job_status(&mut data, false, false);
    assert_eq!(data["job"]["status"], "success");

    // After failure
    update_job_status(&mut data, true, false);
    assert_eq!(data["job"]["status"], "failure");

    // Cancelled takes priority
    update_job_status(&mut data, true, true);
    assert_eq!(data["job"]["status"], "cancelled");

    // Cancelled without failure
    update_job_status(&mut data, false, true);
    assert_eq!(data["job"]["status"], "cancelled");

    // Back to success
    update_job_status(&mut data, false, false);
    assert_eq!(data["job"]["status"], "success");
}
