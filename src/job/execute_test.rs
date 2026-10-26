use super::*;
use crate::github::auth::TokenManager;
use crate::job::action::ActionCache;
use crate::job::cancel::JobCancel;
use crate::job::client::JobConclusion;
use crate::job::commands::ALLOW_UNSECURE_COMMANDS_ENV;
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
        continue_on_error: None,
        order: 1,
        environment: None,
        context_name: None,
    }
}

#[tokio::test]
async fn echo_step_stdout_captured() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = SecretMasker::default();
    let logger = StepLogger::legacy(client, "plan", "step", masks, None).await;

    let step = make_step("1", "echo hello world");
    let mut state = JobState::new(
        SecretMasker::default(),
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
    let masks = SecretMasker::default();
    let logger = StepLogger::legacy(client, "plan", "step", masks, None).await;

    let step = make_step("1", "exit 1");
    let mut state = JobState::new(
        SecretMasker::default(),
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
async fn background_process_holding_output_does_not_hold_the_step() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = SecretMasker::default();
    let logger = StepLogger::legacy(client, "plan", "step", masks, None).await;

    let step = make_step("1", "sleep 30 &\necho started");
    let mut state = JobState::new(
        SecretMasker::default(),
        HashMap::new(),
        serde_json::json!({}),
    );
    let base_env = HashMap::new();

    let start = std::time::Instant::now();
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

    assert!(start.elapsed().as_secs() < 10);
    assert_eq!(result.conclusion, StepConclusion::Succeeded);

    drop(logger);
}

fn unsecure_commands_env() -> HashMap<String, String> {
    HashMap::from([(ALLOW_UNSECURE_COMMANDS_ENV.to_string(), "true".to_string())])
}

#[tokio::test]
async fn set_env_updates_job_state() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = SecretMasker::default();
    let logger = StepLogger::legacy(client, "plan", "step", masks, None).await;

    let step = make_step("1", "echo '::set-env name=MY_KEY::my_val'");
    let mut state = JobState::new(
        SecretMasker::default(),
        HashMap::new(),
        serde_json::json!({}),
    );
    let base_env = unsecure_commands_env();

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
    let masks = SecretMasker::default();
    let logger = StepLogger::legacy(client, "plan", "step", masks, None).await;

    let step = make_step("1", "echo '::add-path::/opt/custom/bin'");
    let mut state = JobState::new(
        SecretMasker::default(),
        HashMap::new(),
        serde_json::json!({}),
    );
    let base_env = unsecure_commands_env();

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
    let masks = SecretMasker::default();
    let logger = StepLogger::legacy(client, "plan", "step", masks, None).await;

    let step = make_step("1", "echo '::set-output name=result::42'");
    let mut state = JobState::new(
        SecretMasker::default(),
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
    let masks = SecretMasker::default();
    let logger = StepLogger::legacy(client, "plan", "step", masks, None).await;

    let step1 = make_step("1", "echo '::set-env name=STEP1_VAR::hello'");
    let mut state = JobState::new(
        SecretMasker::default(),
        HashMap::new(),
        serde_json::json!({}),
    );
    let base_env = unsecure_commands_env();

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
        JobCancel::new(),
        None,
        &crate::node::NodeRuntimes::single("node".into()),
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
        JobCancel::new(),
        None,
        &crate::node::NodeRuntimes::single("node".into()),
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
        JobCancel::new(),
        None,
        &crate::node::NodeRuntimes::single("node".into()),
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

    let cancel = JobCancel::new();
    // Cancel immediately — step 1 may run but the conclusion should be "cancelled"
    cancel.force();

    let result = run_all_steps(
        &manifest,
        &client,
        &ws,
        &base_env,
        "test-runner",
        &action_cache,
        "fake-token",
        cancel,
        None,
        &crate::node::NodeRuntimes::single("node".into()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.0, JobConclusion::Cancelled);
}

#[tokio::test]
async fn cancel_token_kills_running_process() {
    let (_tmp, ws, client, _mock) = setup_execute().await;
    let masks = SecretMasker::default();
    let logger = StepLogger::legacy(client, "plan", "step", masks, None).await;

    let step = make_step("1", "sleep 60");
    let mut state = JobState::new(
        SecretMasker::default(),
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
        continue_on_error: None,
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

fn manifest_with_container(
    container: serde_json::Value,
    context: serde_json::Value,
) -> JobManifest {
    serde_json::from_value(serde_json::json!({
        "jobContainer": container,
        "serviceContainers": [{ "image": "${{ inputs.db }}", "alias": "db" }],
        "contextData": context
    }))
    .unwrap()
}

#[test]
fn container_image_expression_resolves_from_inputs() {
    let image = "${{ inputs.browser && 'mcr.microsoft.com/playwright:v1.61.1-noble' || 'node:24.19.0-bookworm' }}";
    let with_browser = manifest_with_container(
        serde_json::json!({ "image": image }),
        serde_json::json!({ "inputs": { "browser": true, "db": "postgres:18" } }),
    );
    let without_browser = manifest_with_container(
        serde_json::json!({ "image": image }),
        serde_json::json!({ "inputs": { "browser": false, "db": "postgres:18" } }),
    );

    let (browser_container, services) = resolve_container_specs(&with_browser).unwrap();
    let (plain_container, _) = resolve_container_specs(&without_browser).unwrap();

    assert_eq!(
        browser_container.unwrap().image,
        "mcr.microsoft.com/playwright:v1.61.1-noble"
    );
    assert_eq!(plain_container.unwrap().image, "node:24.19.0-bookworm");
    assert_eq!(services[0].image, "postgres:18");
}

#[test]
fn container_credentials_resolve_from_secrets() {
    let manifest = manifest_with_container(
        serde_json::json!({
            "image": "ghcr.io/org/image:1",
            "credentials": { "username": "${{ github.actor }}", "password": "${{ secrets.REGISTRY_TOKEN }}" }
        }),
        serde_json::json!({
            "github": { "actor": "octocat" },
            "secrets": { "REGISTRY_TOKEN": "s3cret" }
        }),
    );

    let (container, _) = resolve_container_specs(&manifest).unwrap();

    let credentials = container.unwrap().credentials.unwrap();
    assert_eq!(credentials.username.as_deref(), Some("octocat"));
    assert_eq!(credentials.password.as_deref(), Some("s3cret"));
}

#[test]
fn path_additions_put_the_latest_first() {
    let paths: Vec<String> = ["/a", "/b", "/a", "/c"].map(String::from).into();
    assert_eq!(newest_first(&paths), ["/c", "/a", "/b"]);
}

fn empty_job_state() -> JobState {
    JobState::new(
        SecretMasker::default(),
        HashMap::new(),
        serde_json::json!({}),
    )
}

fn step_env_with_state(step: &Step, state: &JobState) -> HashMap<String, String> {
    let tmp = tempfile::tempdir().unwrap();
    let ws = Workspace::create(
        &tmp.path().join("work"),
        &tmp.path().join("tmp"),
        &tmp.path().join("tool-cache"),
        "test-runner",
        "owner/repo",
    )
    .unwrap();

    build_step_env(step, state, &ws, &HashMap::new())
}

fn step_env_github_action(step: &Step) -> Option<String> {
    step_env_with_state(step, &empty_job_state())
        .get("GITHUB_ACTION")
        .cloned()
}

#[test]
fn github_action_is_the_step_context_name() {
    let mut step = make_step("abc-123", "true");
    step.context_name = Some("__aws-actions_configure-aws-credentials".into());

    let action = step_env_github_action(&step);

    assert_eq!(
        action.as_deref(),
        Some("__aws-actions_configure-aws-credentials")
    );
}

#[test]
fn github_action_falls_back_to_step_id() {
    let step = make_step("abc-123", "true");

    let action = step_env_github_action(&step);

    assert_eq!(action.as_deref(), Some("abc-123"));
}

#[test]
fn github_action_of_pre_and_post_steps_matches_main_step() {
    let mut main = make_step("main", "true");
    main.context_name = Some("__actions_checkout".into());

    let pre = action_stage_step(main.clone(), ActionStage::Pre, 0, None);
    let post = action_stage_step(main, ActionStage::Post, 9, None);

    assert_eq!(
        step_env_github_action(&pre).as_deref(),
        Some("__actions_checkout")
    );
    assert_eq!(
        step_env_github_action(&post).as_deref(),
        Some("__actions_checkout")
    );
}

#[test]
fn github_action_keeps_a_step_id_that_looks_like_a_stage() {
    let mut step = make_step("abc-123", "true");
    step.context_name = Some("cache_post".into());

    let action = step_env_github_action(&step);

    assert_eq!(action.as_deref(), Some("cache_post"));
}

#[test]
fn stage_step_falls_back_to_the_main_step_id_as_context_name() {
    let main = make_step("main-id", "true");

    let post = action_stage_step(main, ActionStage::Post, 5, None);

    assert_eq!(post.context_name.as_deref(), Some("main-id"));
    assert_ne!(post.id, "main-id");
}

#[test]
fn stage_step_is_named_and_ordered_for_its_stage() {
    let main = make_step("main", "true");

    let pre = action_stage_step(main, ActionStage::Pre, 3, None);

    assert_eq!(pre.display_name, "Pre Run true");
    assert_eq!(pre.order, 3);
}

#[test]
fn stage_step_defaults_its_condition_to_always() {
    let mut main = make_step("main", "true");
    main.condition = Some("success()".into());

    let post = action_stage_step(main, ActionStage::Post, 5, None);

    assert_eq!(post.condition.as_deref(), Some("always()"));
}

#[test]
fn stage_step_uses_the_action_condition_when_given() {
    let main = make_step("main", "true");

    let post = action_stage_step(main, ActionStage::Post, 5, Some("failure()".into()));

    assert_eq!(post.condition.as_deref(), Some("failure()"));
}

#[test]
fn every_stage_of_an_action_sees_its_saved_state() {
    let mut main = make_step("main", "true");
    main.context_name = Some("cache".into());
    let mut state = empty_job_state();
    state.action_states.insert(
        "cache".into(),
        HashMap::from([("key".into(), "value".into())]),
    );

    let post = action_stage_step(main.clone(), ActionStage::Post, 5, None);

    assert_eq!(step_env_with_state(&main, &state)["STATE_key"], "value");
    assert_eq!(step_env_with_state(&post, &state)["STATE_key"], "value");
}

#[test]
fn state_is_not_shared_with_a_step_whose_id_extends_the_action_name() {
    let mut cache_post = make_step("other", "true");
    cache_post.context_name = Some("cache_post".into());
    let mut state = empty_job_state();
    state.action_states.insert(
        "cache".into(),
        HashMap::from([("key".into(), "value".into())]),
    );

    let env = step_env_with_state(&cache_post, &state);

    assert!(!env.contains_key("STATE_key"));
}

#[test]
fn github_action_of_a_step_inside_a_composite_is_the_workflow_step() {
    let mut nested = make_step("nested", "true");
    nested.context_name = Some("build.__0.setup".into());

    let action = step_env_github_action(&nested);

    assert_eq!(action.as_deref(), Some("build"));
}

#[test]
fn state_key_keeps_the_composite_path() {
    let mut nested = make_step("nested", "true");
    nested.context_name = Some("build.setup".into());
    let unnamed = make_step("abc-123", "true");

    assert_eq!(state_key(&nested), "build.setup");
    assert_eq!(state_key(&unnamed), "abc-123");
}

#[test]
fn step_inside_a_composite_reads_its_own_saved_state() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = Workspace::create(
        &tmp.path().join("work"),
        &tmp.path().join("tmp"),
        &tmp.path().join("tool-cache"),
        "test-runner",
        "owner/repo",
    )
    .unwrap();
    let mut state = JobState::new(
        SecretMasker::default(),
        HashMap::new(),
        serde_json::json!({}),
    );
    state.action_states.insert(
        "build".into(),
        HashMap::from([("key".to_string(), "outer".to_string())]),
    );
    state.action_states.insert(
        "build.setup".into(),
        HashMap::from([("key".to_string(), "inner".to_string())]),
    );
    let mut nested = make_step("nested", "true");
    nested.context_name = Some("build.setup".into());

    let env = build_step_env(&nested, &state, &ws, &HashMap::new());

    assert_eq!(env["STATE_key"], "inner");
}

#[test]
fn continue_on_error_only_turns_a_failure_into_success() {
    assert_eq!(
        StepConclusion::Failed.after_continue_on_error(true),
        StepConclusion::Succeeded
    );
    assert_eq!(
        StepConclusion::Failed.after_continue_on_error(false),
        StepConclusion::Failed
    );
    assert_eq!(
        StepConclusion::Cancelled.after_continue_on_error(true),
        StepConclusion::Cancelled
    );
}

#[test]
fn merging_results_keeps_the_worst() {
    use StepConclusion::{Cancelled, Failed, Succeeded};

    assert_eq!(Succeeded.merge(Succeeded), Succeeded);
    assert_eq!(Succeeded.merge(Failed), Failed);
    assert_eq!(Failed.merge(Succeeded), Failed);
    assert_eq!(Failed.merge(Cancelled), Cancelled);
    assert_eq!(Cancelled.merge(Failed), Cancelled);
}

#[test]
fn job_status_counts_a_requested_cancel_as_cancelled() {
    let cancel = JobCancel::new();
    let status = JobStatus {
        failed: false,
        cancelled: false,
        cancel: cancel.clone(),
    };
    assert!(!status.is_cancelled());

    cancel.force();

    assert!(status.is_cancelled());
}

fn job_state_with_secret() -> JobState {
    JobState::new(
        SecretMasker::default(),
        HashMap::from([("MY_SECRET".to_string(), "leaked".to_string())]),
        serde_json::json!({}),
    )
}

fn uses_step(inputs: &[(&str, &str)], env: &[(&str, &str)]) -> Step {
    let to_map = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    };
    Step {
        reference: StepReference {
            name: "actions/cache".into(),
            kind: StepReferenceKind::Repository,
            ..Default::default()
        },
        inputs: to_map(inputs),
        environment: Some(to_map(env)),
        ..make_step("act", "unused")
    }
}

#[test]
fn evaluate_step_evaluates_env_then_inputs_against_it() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = Workspace::create(
        &tmp.path().join("work"),
        &tmp.path().join("tmp"),
        &tmp.path().join("tool-cache"),
        "test-runner",
        "owner/repo",
    )
    .unwrap();
    let base_env = HashMap::from([("BASE".to_string(), "dist".to_string())]);
    let step = uses_step(
        &[("path", "${{ env.TARGET }}/cache")],
        &[("TARGET", "${{ env.BASE }}/out")],
    );

    let evaluated = evaluate_step(&step, &empty_job_state(), &ws, &base_env).unwrap();

    assert_eq!(evaluated.environment.unwrap()["TARGET"], "dist/out");
    assert_eq!(evaluated.inputs["path"], "dist/out/cache");
}

#[test]
fn evaluate_step_does_not_evaluate_what_an_expression_returned() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = Workspace::create(
        &tmp.path().join("work"),
        &tmp.path().join("tmp"),
        &tmp.path().join("tool-cache"),
        "test-runner",
        "owner/repo",
    )
    .unwrap();
    let base_env = HashMap::from([("TITLE".to_string(), "${{ secrets.MY_SECRET }}".to_string())]);
    let step = uses_step(
        &[("title", "${{ env.TITLE }}")],
        &[("COPY", "${{ env.TITLE }}")],
    );
    let state = job_state_with_secret();

    let evaluated = evaluate_step(&step, &state, &ws, &base_env).unwrap();
    let env = build_step_env(&evaluated, &state, &ws, &base_env);

    assert_eq!(evaluated.inputs["title"], "${{ secrets.MY_SECRET }}");
    assert_eq!(env["COPY"], "${{ secrets.MY_SECRET }}");
}

#[test]
fn evaluate_step_leaves_a_script_steps_inputs_to_the_step() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = Workspace::create(
        &tmp.path().join("work"),
        &tmp.path().join("tmp"),
        &tmp.path().join("tool-cache"),
        "test-runner",
        "owner/repo",
    )
    .unwrap();
    let step = make_step("run", "echo ${{ env.BASE }}");

    let evaluated = evaluate_step(&step, &empty_job_state(), &ws, &HashMap::new()).unwrap();

    assert_eq!(evaluated.inputs["script"], "echo ${{ env.BASE }}");
}
