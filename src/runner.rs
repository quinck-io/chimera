mod env;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::sync::watch;
use tokio::task::JoinHandle;
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
use crate::job::workspace::Workspace;

use env::{build_base_env, build_container_env};

const CONTROL_MSG_DELAY: Duration = Duration::from_millis(2000);
const CANCEL_POLL_DELAY: Duration = Duration::from_millis(2000);
const CANCEL_POLL_ERROR_DELAY: Duration = Duration::from_millis(5000);

pub struct Runner {
    name: String,
    credentials: RunnerCredentials,
    paths: ChimeraPaths,
    state: Option<Arc<DaemonState>>,
}

impl Runner {
    pub fn with_state(
        name: String,
        credentials: RunnerCredentials,
        paths: ChimeraPaths,
        state: Arc<DaemonState>,
    ) -> Self {
        Self {
            name,
            credentials,
            paths,
            state: Some(state),
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

        let workspace = Workspace::create(
            &self.paths.work_dir(),
            &self.paths.tmp_dir(),
            &self.paths.tool_cache_dir(),
            &self.name,
            &repo,
        )
        .context("creating workspace")?;

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

            // The workflow files dir is one level above workspace_dir (where _env, _path etc live)
            let workflow_files_path = workspace
                .workspace_dir()
                .parent()
                .context("workspace has no parent")?;

            resources
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
                .context("setting up Docker resources")?;
            Some(resources)
        } else {
            None
        };

        // Choose env builder based on execution mode
        let mut base_env = if manifest.has_container() {
            build_container_env(&manifest, &workspace, &self.name)
        } else {
            build_base_env(&manifest, &workspace, &self.name)
        };

        // Inject service container addresses into environment for discoverability
        if let Some(ref resources) = docker_resources {
            for (alias, ip) in resources.service_addresses() {
                let env_key = format!("SERVICE_{}_HOST", alias.to_uppercase().replace('-', "_"));
                base_env.insert(env_key, ip.clone());
            }
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
        let job_client = Arc::new(job_client);
        let (heartbeat_handle, heartbeat_cancel) =
            job_client.start_heartbeat(manifest.plan.plan_id.clone(), manifest.plan.job_id.clone());

        let job_result = run_all_steps(
            &manifest,
            &job_client,
            &workspace,
            &base_env,
            &self.name,
            &action_cache,
            &github_token,
            cancel_token.clone(),
            docker_resources.as_ref(),
            &node_path,
            live_feed.as_ref().map(|f| f.sender()),
        )
        .await;

        // Close the live feed so remaining lines are flushed over WebSocket
        if let Some(feed) = live_feed {
            feed.close().await;
        }

        // ALWAYS cleanup Docker resources, even if job_result is Err
        if let Some(ref mut resources) = docker_resources {
            resources.cleanup().await;
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

        if let Err(e) = workspace.cleanup() {
            warn!(error = %e, "workspace cleanup failed");
        }

        Ok(())
    }

    async fn poll_loop(
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

/// Spawn a background task that polls the broker for cancellation messages.
/// When a `JobCancellation` arrives, it triggers the token.
fn spawn_cancel_poller(broker: &BrokerClient, cancel_token: CancellationToken) -> JoinHandle<()> {
    let client = broker.client().clone();
    let server_url = broker.server_url().to_string();
    let session_id = broker.session_id().to_string();
    let token_manager = broker.token_manager_arc();

    tokio::spawn(async move {
        let poller = BrokerClient::new(client, server_url, session_id, token_manager);
        loop {
            if cancel_token.is_cancelled() {
                return;
            }

            let poll_result = tokio::select! {
                result = poller.poll_message() => result,
                _ = cancel_token.cancelled() => return,
            };

            match poll_result {
                Ok(Some(msg)) if msg.message_type == MessageType::JobCancellation => {
                    let job_id = msg
                        .parse_cancellation_job_id()
                        .unwrap_or_else(|_| "unknown".into());
                    info!(job_id, "received job cancellation from broker");
                    cancel_token.cancel();
                    return;
                }
                Ok(_) => {
                    // Not a cancellation — brief pause to avoid tight loop
                    let delay = if cfg!(test) {
                        Duration::from_millis(10)
                    } else {
                        CANCEL_POLL_DELAY
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = cancel_token.cancelled() => return,
                    }
                }
                Err(_) => {
                    // Poll error — brief pause then retry
                    let delay = if cfg!(test) {
                        Duration::from_millis(10)
                    } else {
                        CANCEL_POLL_ERROR_DELAY
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(delay) => {}
                        _ = cancel_token.cancelled() => return,
                    }
                }
            }
        }
    })
}

/// Convert job outputs to VariableValue dictionary format for the completejob API.
fn outputs_to_variable_values(
    outputs: &std::collections::HashMap<String, String>,
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in outputs {
        map.insert(k.clone(), serde_json::json!({"value": v}));
    }
    serde_json::Value::Object(map)
}

#[cfg(test)]
#[path = "runner_test.rs"]
mod runner_test;
