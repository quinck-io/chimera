use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use chimera::docker::client as docker_client;
use chimera::docker::container::{JobContainerSpec, ServiceContainerSpec};
use chimera::docker::resources::{JobDockerResources, SetupParams};
use chimera::github::auth::TokenManager;
use chimera::job::action::ActionCache;
use chimera::job::client::JobClient;
use chimera::job::execute::run_all_steps;
use chimera::job::schema::JobManifest;
use chimera::job::workspace::Workspace;
use chimera::runner::env::{build_base_env, build_container_env};

use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Everything needed to run integration tests against the execution engine.
pub struct TestEnv {
    pub workspace: Workspace,
    pub job_client: Arc<JobClient>,
    pub mock_server: MockServer,
    pub tmp: tempfile::TempDir,
    actions_dir: std::path::PathBuf,
}

impl TestEnv {
    pub async fn setup() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let work_dir = tmp.path().join("work");
        let tmp_dir = tmp.path().join("tmp");
        let tool_cache = tmp.path().join("tool-cache");
        let actions_dir = tmp.path().join("actions");

        let ws = Workspace::create(
            &work_dir,
            &tmp_dir,
            &tool_cache,
            "test-runner",
            "owner/repo",
        )
        .unwrap();

        let mock_server = MockServer::start().await;
        mount_default_mocks(&mock_server).await;

        let job_client = create_job_client(&mock_server).await;

        Self {
            workspace: ws,
            job_client,
            mock_server,
            tmp,
            actions_dir,
        }
    }

    /// Run a manifest in host mode and return (conclusion, outputs).
    pub async fn run(
        &self,
        manifest: &JobManifest,
    ) -> anyhow::Result<(chimera::job::client::JobConclusion, HashMap<String, String>)> {
        let mut base_env = build_base_env(manifest, &self.workspace, "test-runner");

        // In real host-mode runs, PATH is inherited from the process environment.
        // We must include it explicitly because build_step_env only looks at the
        // HashMap, not the inherited process env.
        if let Ok(path) = std::env::var("PATH") {
            base_env.entry("PATH".into()).or_insert(path);
        }

        let action_cache = ActionCache::new(self.actions_dir.clone(), reqwest::Client::new());

        run_all_steps(
            manifest,
            &self.job_client,
            &self.workspace,
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
    }

    /// Run a manifest in container mode with Docker resources.
    pub async fn run_with_docker(
        &self,
        manifest: &JobManifest,
        docker_resources: &JobDockerResources,
    ) -> anyhow::Result<(chimera::job::client::JobConclusion, HashMap<String, String>)> {
        let base_env = build_container_env(manifest, &self.workspace, "test-runner");
        let action_cache = ActionCache::new(self.actions_dir.clone(), reqwest::Client::new());

        let node_path = Path::new(docker_resources.node_path());

        run_all_steps(
            manifest,
            &self.job_client,
            &self.workspace,
            &base_env,
            "test-runner",
            &action_cache,
            "fake-token",
            CancellationToken::new(),
            Some(docker_resources),
            node_path,
            None,
        )
        .await
    }
}

/// Set up Docker resources for a job. Returns resources that must be cleaned up.
pub async fn setup_docker(
    tmp: &tempfile::TempDir,
    workspace: &Workspace,
    job_container: Option<&JobContainerSpec>,
    services: &[ServiceContainerSpec],
) -> JobDockerResources {
    let docker = docker_client::connect(None).unwrap();
    docker_client::ping(&docker).await.unwrap();

    let job_id = uuid::Uuid::new_v4().to_string();
    let mut resources = JobDockerResources::new(docker);

    let workflow_dir = tmp.path().join("workflow");
    let externals_dir = tmp.path().join("externals");
    std::fs::create_dir_all(&workflow_dir).unwrap();
    std::fs::create_dir_all(&externals_dir).unwrap();

    resources
        .setup(&SetupParams {
            runner_name: "test-runner",
            job_id: &job_id,
            job_container,
            services,
            workspace_host_path: workspace.workspace_dir(),
            workflow_files_host_path: &workflow_dir,
            runner_temp_host_path: workspace.runner_temp(),
            actions_host_path: &tmp.path().join("actions"),
            tool_cache_host_path: workspace.tool_cache(),
            externals_dir: &externals_dir,
        })
        .await
        .unwrap();

    resources
}

async fn mount_default_mocks(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path_regex("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "test-token",
            "expires_in": 7200
        })))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path_regex(r"/_apis/pipelines/workflows/.*/logs$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": 1})))
        .mount(server)
        .await;

    Mock::given(method("POST"))
        .and(path_regex(r"/_apis/pipelines/workflows/.*/logs/\d+"))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;

    Mock::given(method("PATCH"))
        .and(path_regex(
            r"/_apis/distributedtask/hubs/build/plans/.*/timelines/.*",
        ))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
}

async fn create_job_client(mock_server: &MockServer) -> Arc<JobClient> {
    let private_key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let tm = Arc::new(TokenManager::new(
        reqwest::Client::new(),
        format!("{}/oauth2/token", mock_server.uri()),
        private_key,
        "test-client".into(),
    ));

    Arc::new(JobClient::new(
        reqwest::Client::new(),
        tm,
        mock_server.uri(),
        mock_server.uri(),
    ))
}

// ─── Manifest / Step builders ────────────────────────────────────────

pub fn manifest_with_steps(steps: Vec<serde_json::Value>, server_url: &str) -> JobManifest {
    manifest_with_steps_and_context(steps, server_url, serde_json::json!({}))
}

pub fn manifest_with_steps_and_context(
    steps: Vec<serde_json::Value>,
    server_url: &str,
    context_data: serde_json::Value,
) -> JobManifest {
    let manifest_json = serde_json::json!({
        "plan": { "planId": "p", "jobId": "j", "timelineId": "t" },
        "steps": steps,
        "variables": {},
        "resources": {
            "endpoints": [{
                "name": "SystemVssConnection",
                "url": server_url,
                "authorization": {
                    "scheme": "OAuth",
                    "parameters": { "AccessToken": "test-access-token" }
                },
                "data": { "PipelinesServiceUrl": server_url }
            }]
        },
        "contextData": context_data,
        "jobContainer": null,
        "serviceContainers": null
    });
    serde_json::from_value(manifest_json).unwrap()
}

pub fn script_step(id: &str, script: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "displayName": format!("Run: {id}"),
        "reference": { "name": "script", "type": "script" },
        "inputs": { "script": script },
        "condition": null,
        "timeoutInMinutes": null,
        "continueOnError": false,
        "order": 1,
        "environment": null,
        "contextName": id
    })
}

pub fn script_step_continue(id: &str, script: &str) -> serde_json::Value {
    let mut step = script_step(id, script);
    step["continueOnError"] = serde_json::json!(true);
    step
}

pub fn script_step_if(id: &str, script: &str, condition: &str) -> serde_json::Value {
    let mut step = script_step(id, script);
    step["condition"] = serde_json::json!(condition);
    step
}

pub fn script_step_env(id: &str, script: &str, env: HashMap<String, String>) -> serde_json::Value {
    let mut step = script_step(id, script);
    step["environment"] = serde_json::to_value(env).unwrap();
    step
}
