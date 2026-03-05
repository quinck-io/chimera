use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use crate::config::{ChimeraPaths, RunnerCredentials, rsa_params_to_private_key};
use crate::github::RUNNER_VERSION;
use crate::github::auth::TokenManager;
use crate::github::broker::{BrokerClient, BrokerError, BrokerMessage};
use crate::job::JobClient;
use crate::job::execute::run_all_steps;
use crate::job::workspace::Workspace;

pub struct Runner {
    name: String,
    credentials: RunnerCredentials,
    paths: ChimeraPaths,
}

impl Runner {
    pub fn new(name: String, credentials: RunnerCredentials, paths: ChimeraPaths) -> Self {
        Self {
            name,
            credentials,
            paths,
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
        info!("entering poll loop, waiting for jobs...");

        loop {
            let result = self.poll_loop(&broker, &mut shutdown_rx).await;

            match &result {
                Ok(Some(msg)) => {
                    info!(
                        message_id = msg.message_id,
                        message_type = %msg.message_type,
                        "received job message"
                    );

                    if let Some(body) = &msg.body
                        && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body)
                    {
                        let runner_request_id = parsed
                            .get("runner_request_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        // Ack the job
                        if let Err(e) = broker.ack_job(runner_request_id).await {
                            error!(error = %e, "failed to ack message");
                        }

                        let run_service_url = parsed
                            .get("run_service_url")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        // Execute the job
                        if let Err(e) = self
                            .execute_job(
                                &client,
                                token_manager.clone(),
                                runner_request_id,
                                run_service_url,
                            )
                            .await
                        {
                            error!(error = %e, "job execution failed");
                        }
                    }

                    // After job completes, check for shutdown, otherwise loop back to polling
                    if *shutdown_rx.borrow() {
                        info!("shutdown after job completion");
                        break;
                    }
                    continue;
                }
                Ok(None) => {
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

    async fn execute_job(
        &self,
        client: &reqwest::Client,
        token_manager: Arc<TokenManager>,
        runner_request_id: &str,
        run_service_url: &str,
    ) -> Result<()> {
        info!(runner_request_id, "acquiring job");

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

        info!(
            plan_id = %manifest.plan.plan_id,
            job_id = %manifest.plan.job_id,
            steps = manifest.steps.len(),
            has_container = manifest.has_container(),
            has_services = manifest.has_services(),
            "job acquired"
        );

        // Set the job access token from the manifest
        let access_token = manifest.access_token()?.to_string();
        job_client.set_job_access_token(access_token);

        let repo = manifest
            .repository()
            .unwrap_or_else(|_| "unknown/repo".into());

        // Create workspace
        let workspace = Workspace::create(
            &self.paths.work_dir(),
            &self.paths.tmp_dir(),
            &self.paths.tool_cache_dir(),
            &self.name,
            &repo,
        )
        .context("creating workspace")?;

        // Build base environment
        let base_env = build_base_env(&manifest, &workspace, &self.name);

        // Start heartbeat
        let job_client = Arc::new(job_client);
        let (heartbeat_handle, heartbeat_cancel) =
            job_client.start_heartbeat(manifest.plan.plan_id.clone(), manifest.plan.job_id.clone());

        // Run all steps
        let conclusion = run_all_steps(&manifest, &job_client, &workspace, &base_env)
            .await
            .context("running job steps")?;

        info!(conclusion = %conclusion, "job steps completed");

        // Cancel heartbeat
        heartbeat_cancel.cancel();
        let _ = heartbeat_handle.await;

        // Complete the job
        job_client
            .complete_job(
                &manifest.plan.plan_id,
                &manifest.plan.job_id,
                &conclusion,
                &serde_json::json!({}),
            )
            .await
            .context("completing job")?;

        info!(conclusion = %conclusion, "job completed");

        // Cleanup workspace
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
                    if msg.message_type != "RunnerJobRequest" {
                        debug!(
                            message_id = msg.message_id,
                            message_type = %msg.message_type,
                            "received control message, acking and skipping"
                        );
                        if let Err(e) = broker.delete_message(msg.message_id).await {
                            warn!(error = %e, "failed to delete control message");
                        }
                        continue;
                    }
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

fn build_base_env(
    manifest: &crate::job::schema::JobManifest,
    workspace: &Workspace,
    runner_name: &str,
) -> HashMap<String, String> {
    let mut env = HashMap::new();

    env.insert("GITHUB_ACTIONS".into(), "true".into());
    env.insert(
        "GITHUB_WORKSPACE".into(),
        workspace.workspace_dir().to_string_lossy().into_owned(),
    );
    env.insert(
        "GITHUB_ENV".into(),
        workspace.env_file().to_string_lossy().into_owned(),
    );
    env.insert(
        "GITHUB_PATH".into(),
        workspace.path_file().to_string_lossy().into_owned(),
    );
    env.insert(
        "GITHUB_OUTPUT".into(),
        workspace.output_file().to_string_lossy().into_owned(),
    );
    env.insert("RUNNER_OS".into(), "Linux".into());
    env.insert("RUNNER_ARCH".into(), "X64".into());
    env.insert("RUNNER_NAME".into(), runner_name.into());
    env.insert(
        "RUNNER_TEMP".into(),
        workspace.runner_temp().to_string_lossy().into_owned(),
    );
    env.insert(
        "RUNNER_TOOL_CACHE".into(),
        workspace.tool_cache().to_string_lossy().into_owned(),
    );

    // Extract fields from context_data.github
    if let Some(github) = manifest.context_data.get("github") {
        let mappings = [
            ("workflow", "GITHUB_WORKFLOW"),
            ("run_id", "GITHUB_RUN_ID"),
            ("run_number", "GITHUB_RUN_NUMBER"),
            ("job", "GITHUB_JOB"),
            ("action", "GITHUB_ACTION"),
            ("actor", "GITHUB_ACTOR"),
            ("repository", "GITHUB_REPOSITORY"),
            ("event_name", "GITHUB_EVENT_NAME"),
            ("sha", "GITHUB_SHA"),
            ("ref", "GITHUB_REF"),
        ];

        for (json_key, env_key) in mappings {
            if let Some(val) = github.get(json_key).and_then(|v| v.as_str()) {
                env.insert(env_key.into(), val.into());
            }
        }
    }

    // Add non-secret variables
    for (key, var) in &manifest.variables {
        if !var.is_secret {
            // Convert system.xxx to env-friendly format
            let env_key = key.replace('.', "_").to_uppercase();
            env.insert(env_key, var.value.clone());
        }
    }

    // Server URL and token for actions runtime
    if let Ok(server_url) = manifest.server_url() {
        env.insert("ACTIONS_RUNTIME_URL".into(), server_url.into());
    }
    if let Ok(token) = manifest.access_token() {
        env.insert("ACTIONS_RUNTIME_TOKEN".into(), token.into());
    }

    env
}

#[cfg(test)]
#[path = "runner_test.rs"]
mod runner_test;
