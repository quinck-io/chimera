use super::*;
use crate::github::auth::TokenManager;
use crate::job::schema::StepReference;
use rsa::RsaPrivateKey;
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
            r#type: "script".into(),
        },
        inputs: HashMap::from([("script".into(), script.into())]),
        condition: None,
        timeout_in_minutes: None,
        continue_on_error: false,
        order: 1,
        environment: None,
    }
}

#[tokio::test]
async fn echo_step_stdout_captured() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = Arc::new(RwLock::new(Vec::new()));
    let (sender, handle) = logs::start_log_upload(client, "plan".into(), 1, masks.clone());

    let step = make_step("1", "echo hello world");
    let mut state = JobState::new(masks);
    let base_env = HashMap::new();

    let result = run_host_step(&step, &mut state, &ws, &base_env, &sender)
        .await
        .unwrap();
    assert_eq!(result.conclusion, StepConclusion::Succeeded);

    drop(sender);
    handle.await.unwrap();
}

#[tokio::test]
async fn nonzero_exit_returns_failed() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = Arc::new(RwLock::new(Vec::new()));
    let (sender, handle) = logs::start_log_upload(client, "plan".into(), 1, masks.clone());

    let step = make_step("1", "exit 1");
    let mut state = JobState::new(masks);
    let base_env = HashMap::new();

    let result = run_host_step(&step, &mut state, &ws, &base_env, &sender)
        .await
        .unwrap();
    assert_eq!(result.conclusion, StepConclusion::Failed);

    drop(sender);
    handle.await.unwrap();
}

#[tokio::test]
async fn set_env_updates_job_state() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = Arc::new(RwLock::new(Vec::new()));
    let (sender, handle) = logs::start_log_upload(client, "plan".into(), 1, masks.clone());

    let step = make_step("1", "echo '::set-env name=MY_KEY::my_val'");
    let mut state = JobState::new(masks);
    let base_env = HashMap::new();

    run_host_step(&step, &mut state, &ws, &base_env, &sender)
        .await
        .unwrap();
    assert_eq!(state.env.get("MY_KEY").unwrap(), "my_val");

    drop(sender);
    handle.await.unwrap();
}

#[tokio::test]
async fn add_path_updates_path() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = Arc::new(RwLock::new(Vec::new()));
    let (sender, handle) = logs::start_log_upload(client, "plan".into(), 1, masks.clone());

    let step = make_step("1", "echo '::add-path::/opt/custom/bin'");
    let mut state = JobState::new(masks);
    let base_env = HashMap::new();

    run_host_step(&step, &mut state, &ws, &base_env, &sender)
        .await
        .unwrap();
    assert!(state.path_prepends.contains(&"/opt/custom/bin".to_string()));

    drop(sender);
    handle.await.unwrap();
}

#[tokio::test]
async fn set_output_populates_outputs() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = Arc::new(RwLock::new(Vec::new()));
    let (sender, handle) = logs::start_log_upload(client, "plan".into(), 1, masks.clone());

    let step = make_step("1", "echo '::set-output name=result::42'");
    let mut state = JobState::new(masks);
    let base_env = HashMap::new();

    run_host_step(&step, &mut state, &ws, &base_env, &sender)
        .await
        .unwrap();
    assert_eq!(state.outputs.get("result").unwrap(), "42");

    drop(sender);
    handle.await.unwrap();
}

#[tokio::test]
async fn env_propagation_across_steps() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = Arc::new(RwLock::new(Vec::new()));
    let (sender, handle) = logs::start_log_upload(client, "plan".into(), 1, masks.clone());

    // Step 1: set env
    let step1 = make_step("1", "echo '::set-env name=STEP1_VAR::hello'");
    let mut state = JobState::new(masks);
    let base_env = HashMap::new();

    run_host_step(&step1, &mut state, &ws, &base_env, &sender)
        .await
        .unwrap();

    // Step 2: use the env
    let step2 = make_step("2", "test \"$STEP1_VAR\" = \"hello\"");
    let result = run_host_step(&step2, &mut state, &ws, &base_env, &sender)
        .await
        .unwrap();
    assert_eq!(result.conclusion, StepConclusion::Succeeded);

    drop(sender);
    handle.await.unwrap();
}

#[tokio::test]
async fn continue_on_error_works() {
    let (_tmp, ws, client, _mock) = setup_execute().await;

    // TODO better way?
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

    let result = run_all_steps(&manifest, &client, &ws, &base_env)
        .await
        .unwrap();
    assert_eq!(result, "success");
}

#[tokio::test]
async fn failure_stops_remaining_steps() {
    let (_tmp, ws, client, _mock) = setup_execute().await;

    // TODO better way?
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

    let result = run_all_steps(&manifest, &client, &ws, &base_env)
        .await
        .unwrap();
    assert_eq!(result, "failure");
}
