use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::config::{ChimeraPaths, RunnerCredentials, rsa_params_to_private_key};
use crate::daemon::{DaemonState, JobInfo, RunnerPhase};
use crate::docker::resources::{JobDockerResources, SetupParams};
use crate::github::RUNNER_VERSION;
use crate::github::auth::TokenManager;
use crate::github::broker::{BrokerClient, BrokerError, BrokerMessage, MessageType};
use crate::job::JobClient;
use crate::job::action::ActionCache;
use crate::job::client::JobConclusion;
use crate::job::execute::run_all_steps;
use crate::job::live_feed::LiveFeed;
use crate::job::schema::JobManifest;
use crate::job::workspace::Workspace;

use super::cancel::spawn_cancel_poller;
use super::env::{build_base_env, build_container_env};
use super::report::{outputs_to_variable_values, report_setup_failure};

const CONTROL_MSG_DELAY: Duration = Duration::from_millis(2000);

pub struct Runner {
    pub(super) name: String,
    pub(super) credentials: RunnerCredentials,
    pub(super) paths: ChimeraPaths,
    pub(super) state: Option<Arc<DaemonState>>,
    pub(super) cache_port: u16,
}

impl Runner {
    pub fn with_state(
        name: String,
        credentials: RunnerCredentials,
        paths: ChimeraPaths,
        state: Arc<DaemonState>,
        cache_port: u16,
    ) -> Self {
        Self {
            name,
            credentials,
            paths,
            state: Some(state),
            cache_port,
        }
    }

    async fn report_phase(&self, phase: RunnerPhase) {
        if let Some(ref state) = self.state {
            state.set_phase(&self.name, phase).await;
        }
    }

    async fn report_running(&self, repo: &str, job_id: &str) {
        if let Some(ref state) = self.state {
            state
                .set_running(
                    &self.name,
                    JobInfo {
                        repo: repo.to_string(),
                        job_id: job_id.to_string(),
                        started_at: Utc::now(),
                    },
                )
                .await;
        }
    }

    pub async fn start(self, mut shutdown_rx: watch::Receiver<bool>) -> Result<()> {
        info!(runner = %self.name, "starting runner");

        let private_key = rsa_params_to_private_key(&self.credentials.rsa_params)
            .context("reconstructing RSA private key")?;

        let client = reqwest::Client::builder()
            .user_agent(format!("chimera/{RUNNER_VERSION}"))
            .build()
            .context("building HTTP client")?;

        let token_manager = Arc::new(TokenManager::new(
            client.clone(),
            self.credentials.oauth.authorization_url.clone(),
            private_key,
            self.credentials.oauth.client_id.clone(),
        ));

        token_manager
            .get_token()
            .await
            .context("getting initial OAuth token")?;
        info!("authenticated successfully");

        let broker = BrokerClient::connect(
            client.clone(),
            &self.credentials.info.server_url_v2,
            token_manager.clone(),
            self.credentials.info.agent_id,
            &self.credentials.info.agent_name,
        )
        .await
        .context("creating broker session")?;

        info!(session_id = %broker.session_id(), "broker session created");
        self.report_phase(RunnerPhase::Idle).await;
        info!("entering poll loop, waiting for jobs...");

        loop {
            let result = self.poll_loop(&broker, &mut shutdown_rx).await;

            match result {
                Ok(Some(msg)) => {
                    info!(
                        message_id = msg.message_id,
                        message_type = %msg.message_type,
                        "received job message"
                    );

                    self.handle_job_message(&msg, &broker, &client, token_manager.clone())
                        .await;
                    self.report_phase(RunnerPhase::Idle).await;

                    if *shutdown_rx.borrow() {
                        self.report_phase(RunnerPhase::Stopping).await;
                        info!("shutdown after job completion");
                        break;
                    }
                }
                Ok(None) => {
                    self.report_phase(RunnerPhase::Stopping).await;
                    info!("poll loop exited (shutdown)");
                    break;
                }
                Err(e) => {
                    error!(error = %e, "poll loop error");
                    break;
                }
            }
        }

        if let Err(e) = broker.disconnect().await {
            error!(error = %e, "failed to delete session");
        } else {
            info!("session deleted");
        }

        Ok(())
    }

    async fn handle_job_message(
        &self,
        msg: &BrokerMessage,
        broker: &BrokerClient,
        client: &reqwest::Client,
        token_manager: Arc<TokenManager>,
    ) {
        let (runner_request_id, run_service_url) = match msg.parse_job_request() {
            Ok(pair) => pair,
            Err(e) => {
                warn!(error = %e, "failed to parse job request");
                return;
            }
        };

        debug!(%runner_request_id, %run_service_url, "parsed job request");

        if let Err(e) = broker.ack_job(&runner_request_id).await {
            error!(error = %e, "failed to ack job");
        }

        let cancel_token = CancellationToken::new();
        let poller_handle = spawn_cancel_poller(broker, cancel_token.clone());

        let result = self
            .execute_job(
                client,
                token_manager,
                &runner_request_id,
                &run_service_url,
                cancel_token.clone(),
            )
            .await;

        // Stop the cancel poller regardless of how the job ended
        cancel_token.cancel();
        let _ = poller_handle.await;

        if let Err(e) = result {
            error!(error = %e, cause = ?e, "job execution failed");
        }
    }

    async fn execute_job(
        &self,
        client: &reqwest::Client,
        token_manager: Arc<TokenManager>,
        runner_request_id: &str,
        run_service_url: &str,
        cancel_token: CancellationToken,
    ) -> Result<()> {
        info!(runner_request_id, run_service_url, "acquiring job");

        let mut job_client = JobClient::new(
            client.clone(),
            token_manager,
            run_service_url.to_string(),
            self.credentials.info.server_url.clone(),
        );

        let manifest = job_client
            .acquire_job(runner_request_id)
            .await
            .context("acquiring job manifest")?;

        let var_names: Vec<&str> = manifest.variables.keys().map(|s| s.as_str()).collect();
        let container_image = manifest
            .job_container
            .as_ref()
            .map(|c| c.image.as_str())
            .unwrap_or("none");
        info!(
            plan_id = %manifest.plan.plan_id,
            job_id = %manifest.plan.job_id,
            steps = manifest.steps.len(),
            has_container = manifest.has_container(),
            container_image,
            has_services = manifest.has_services(),
            mask_regexes = manifest.mask_regexes().len(),
            files = ?manifest.file_table(),
            variables = ?var_names,
            "job acquired"
        );

        for ep in &manifest.resources.endpoints {
            let data_keys: Vec<&str> = ep.data.keys().map(|s| s.as_str()).collect();
            debug!(endpoint = %ep.name, url = %ep.url, data_keys = ?data_keys, "manifest endpoint");
        }

        job_client
            .configure_from_manifest(&manifest)
            .context("configuring job client from manifest")?;

        let repo = manifest
            .repository()
            .unwrap_or_else(|_| "unknown/repo".into());

        self.report_running(&repo, &manifest.plan.job_id).await;

        // From this point, any failure must report back to GitHub via complete_job.
        // Otherwise GitHub hangs waiting for a completion that never comes.
        let job_client = Arc::new(job_client);
        let result = self
            .run_job(&manifest, &job_client, client, cancel_token, &repo)
            .await;

        if let Err(ref e) = result {
            error!(error = %e, cause = ?e, "job failed, reporting failure to GitHub");

            if let Err(report_err) = report_setup_failure(&job_client, &manifest, e).await {
                error!(error = %report_err, "failed to report setup failure to GitHub");
            }
        }

        result
    }

    async fn run_job(
        &self,
        manifest: &JobManifest,
        job_client: &Arc<JobClient>,
        client: &reqwest::Client,
        cancel_token: CancellationToken,
        repo: &str,
    ) -> Result<()> {
        let workspace = Workspace::create(
            &self.paths.work_dir(),
            &self.paths.tmp_dir(),
            &self.paths.tool_cache_dir(),
            &self.name,
            repo,
        )
        .context("creating workspace")?;

        // Write the event payload so actions can read it via GITHUB_EVENT_PATH
        let event_data = manifest
            .context_data
            .get("github")
            .and_then(|g| g.get("event"))
            .cloned()
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
        workspace
            .write_event_file(&event_data)
            .context("writing event payload")?;

        // Ensure a node binary is available for the host platform.
        // Used by node actions in host mode; container mode downloads its own Linux binary.
        let node_path = crate::node::ensure_node(&self.paths.externals_dir())
            .await
            .context("ensuring node binary")?;

        // Set up Docker resources if the job needs containers or services
        let mut docker_resources = if manifest.has_container() || manifest.has_services() {
            let docker = crate::docker::client::connect(None)?;
            crate::docker::client::ping(&docker).await?;
            let mut resources = JobDockerResources::new(docker);

            let services = manifest.service_containers.as_deref().unwrap_or_default();

            let workflow_files_path = workspace
                .workspace_dir()
                .parent()
                .context("workspace has no parent")?;

            if let Err(e) = resources
                .setup(&SetupParams {
                    runner_name: &self.name,
                    job_id: &manifest.plan.job_id,
                    job_container: manifest.job_container.as_ref(),
                    services,
                    workspace_host_path: workspace.workspace_dir(),
                    workflow_files_host_path: workflow_files_path,
                    runner_temp_host_path: workspace.runner_temp(),
                    actions_host_path: &self.paths.actions_dir(),
                    tool_cache_host_path: workspace.tool_cache(),
                    externals_dir: &self.paths.externals_dir(),
                })
                .await
            {
                resources.cleanup().await;
                return Err(e.context("setting up Docker resources"));
            }
            Some(resources)
        } else {
            None
        };

        // Run the job body — cleanup is guaranteed to run regardless of outcome
        let result = self
            .run_job_body(
                manifest,
                job_client,
                client,
                cancel_token,
                repo,
                &workspace,
                &node_path,
                &mut docker_resources,
            )
            .await;

        if let Some(ref mut resources) = docker_resources {
            resources.cleanup().await;
        }

        if let Err(e) = workspace.cleanup() {
            warn!(error = %e, "workspace cleanup failed");
        }

        result
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_job_body(
        &self,
        manifest: &JobManifest,
        job_client: &Arc<JobClient>,
        client: &reqwest::Client,
        cancel_token: CancellationToken,
        repo: &str,
        workspace: &Workspace,
        node_path: &std::path::Path,
        docker_resources: &mut Option<JobDockerResources>,
    ) -> Result<()> {
        // Choose env builder based on execution mode
        let mut base_env = if manifest.has_container() {
            build_container_env(manifest, workspace, &self.name)
        } else {
            build_base_env(manifest, workspace, &self.name)
        };

        // Merge the Docker image's default PATH so tools installed via ENV in
        // the Dockerfile (e.g. rust:1-bookworm sets /usr/local/cargo/bin) are available.
        if let Some(resources) = docker_resources.as_ref() {
            if let Some(image_path) = resources.image_env().get("PATH") {
                let chimera_default =
                    "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
                if image_path != chimera_default {
                    base_env.insert("PATH".into(), image_path.clone());
                }
            }

            // Inject service container addresses into environment for discoverability
            for (alias, ip) in resources.service_addresses() {
                let env_key = format!("SERVICE_{}_HOST", alias.to_uppercase().replace('-', "_"));
                base_env.insert(env_key, ip.clone());
            }
        }

        // Inject ACTIONS_CACHE_URL for actions/cache support, with scope prefix
        let git_ref = manifest
            .context_data
            .get("github")
            .and_then(|g| g.get("ref"))
            .and_then(|v| v.as_str())
            .unwrap_or("refs/heads/main");
        let default_branch = manifest
            .context_data
            .get("github")
            .and_then(|g| g.get("event"))
            .and_then(|e| e.get("repository"))
            .and_then(|r| r.get("default_branch"))
            .and_then(|v| v.as_str())
            .unwrap_or("main");
        let default_ref = format!("refs/heads/{default_branch}");

        let scope_repo = crate::cache::server::encode_scope(repo);
        let scope_ref = crate::cache::server::encode_scope(git_ref);
        let scope_default = crate::cache::server::encode_scope(&default_ref);

        if manifest.has_container() {
            // On macOS, Docker Desktop runs in a Linux VM so the bridge gateway IP
            // doesn't route to the macOS host. Use host.docker.internal instead.
            let cache_host = if cfg!(target_os = "macos") {
                "host.docker.internal".to_string()
            } else {
                docker_resources
                    .as_ref()
                    .and_then(|r| r.host_gateway_ip())
                    .unwrap_or("172.17.0.1")
                    .to_string()
            };
            base_env.insert(
                "ACTIONS_CACHE_URL".into(),
                format!(
                    "http://{cache_host}:{}/cache/{scope_repo}/{scope_ref}/{scope_default}/",
                    self.cache_port
                ),
            );
        } else {
            base_env.insert(
                "ACTIONS_CACHE_URL".into(),
                format!(
                    "http://localhost:{}/cache/{scope_repo}/{scope_ref}/{scope_default}/",
                    self.cache_port
                ),
            );
        }

        let action_cache = ActionCache::new(self.paths.actions_dir(), client.clone());
        let github_token = manifest.github_token().unwrap_or("").to_string();

        // Connect to the WebSocket live console feed for real-time log streaming
        let live_feed = match (manifest.feed_stream_url(), manifest.access_token()) {
            (Some(feed_url), Ok(token)) => {
                debug!("connecting live console feed");
                LiveFeed::connect(feed_url, token).await
            }
            _ => None,
        };

        // Start heartbeat
        let (heartbeat_handle, heartbeat_cancel) =
            job_client.start_heartbeat(manifest.plan.plan_id.clone(), manifest.plan.job_id.clone());

        let job_result = run_all_steps(
            manifest,
            job_client,
            workspace,
            &base_env,
            &self.name,
            &action_cache,
            &github_token,
            cancel_token.clone(),
            docker_resources.as_ref(),
            node_path,
            live_feed.as_ref().map(|f| f.sender()),
        )
        .await;

        // Close the live feed so remaining lines are flushed over WebSocket
        if let Some(feed) = live_feed {
            feed.close().await;
        }

        let (mut conclusion, job_outputs) = job_result.context("running job steps")?;

        if cancel_token.is_cancelled() && conclusion != JobConclusion::Cancelled {
            conclusion = JobConclusion::Cancelled;
        }

        info!(conclusion = %conclusion, "job steps completed");

        heartbeat_cancel.cancel();
        let _ = heartbeat_handle.await;

        let outputs_payload = outputs_to_variable_values(&job_outputs);
        debug!(job_outputs = ?job_outputs, outputs_payload = %outputs_payload, "completing job");
        job_client
            .complete_job(
                &manifest.plan.plan_id,
                &manifest.plan.job_id,
                conclusion,
                &outputs_payload,
                &[],
            )
            .await
            .context("completing job")?;

        info!(conclusion = %conclusion, "job completed");

        Ok(())
    }

    pub(super) async fn poll_loop(
        &self,
        broker: &BrokerClient,
        shutdown_rx: &mut watch::Receiver<bool>,
    ) -> Result<Option<BrokerMessage>> {
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(30);

        loop {
            if *shutdown_rx.borrow() {
                info!("shutdown signal received, exiting poll loop");
                return Ok(None);
            }

            let poll_result = tokio::select! {
                result = broker.poll_message() => result,
                _ = shutdown_rx.changed() => {
                    info!("shutdown signal received, cancelling poll");
                    return Ok(None);
                }
            };

            match poll_result {
                Ok(Some(msg)) => {
                    if msg.message_type != MessageType::RunnerJobRequest {
                        debug!(
                            message_id = msg.message_id,
                            message_type = %msg.message_type,
                            "received control message, skipping"
                        );
                        // Control messages (JobCancellation, BrokerMigration, etc)
                        // are ephemeral — don't try to delete them. Brief pause to
                        // avoid tight-looping when broker keeps resending.
                        let delay = if cfg!(test) {
                            Duration::from_millis(10)
                        } else {
                            CONTROL_MSG_DELAY
                        };
                        tokio::time::sleep(delay).await;
                        continue;
                    }

                    // V2 broker: job messages are acknowledged via /acknowledge,
                    // not deleted. No delete needed here.
                    info!(
                        message_id = msg.message_id,
                        message_type = %msg.message_type,
                        "received job message"
                    );
                    return Ok(Some(msg));
                }
                Ok(None) => {
                    backoff = Duration::from_secs(1);
                    continue;
                }
                Err(e) => {
                    if e.downcast_ref::<BrokerError>()
                        .is_some_and(|be| matches!(be, BrokerError::Unauthorized))
                    {
                        warn!("got 401, refreshing token");
                        broker.token_manager().invalidate().await;
                        let retry = tokio::select! {
                            result = broker.poll_message() => result,
                            _ = shutdown_rx.changed() => {
                                info!("shutdown signal received during token retry");
                                return Ok(None);
                            }
                        };
                        match retry {
                            Ok(result) => return Ok(result),
                            Err(retry_err) => {
                                warn!(error = %retry_err, "retry after token refresh also failed");
                                return Err(retry_err);
                            }
                        }
                    }

                    warn!(error = %e, backoff_secs = backoff.as_secs(), "poll error, backing off");

                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        _ = shutdown_rx.changed() => {
                            return Ok(None);
                        }
                    }

                    backoff = (backoff * 2).min(max_backoff);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "instance_test.rs"]
mod instance_test;
