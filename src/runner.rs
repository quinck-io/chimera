mod env;

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
use crate::job::action::ActionCache;
use crate::job::execute::run_all_steps;
use crate::job::workspace::Workspace;

use env::build_base_env;

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

            match result {
                Ok(Some(msg)) => {
                    info!(
                        message_id = msg.message_id,
                        message_type = %msg.message_type,
                        "received job message"
                    );

                    self.handle_job_message(&msg, &broker, &client, token_manager.clone())
                        .await;

                    if *shutdown_rx.borrow() {
                        info!("shutdown after job completion");
                        break;
                    }
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

        if let Err(e) = self
            .execute_job(client, token_manager, &runner_request_id, &run_service_url)
            .await
        {
            error!(error = %e, cause = ?e, "job execution failed");
        }
    }

    async fn execute_job(
        &self,
        client: &reqwest::Client,
        token_manager: Arc<TokenManager>,
        runner_request_id: &str,
        run_service_url: &str,
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
        info!(
            plan_id = %manifest.plan.plan_id,
            job_id = %manifest.plan.job_id,
            steps = manifest.steps.len(),
            has_container = manifest.has_container(),
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

        // Set the job access token and URLs from the manifest's SystemVssConnection
        let access_token = manifest.access_token()?.to_string();
        let server_url = manifest.server_url()?.to_string();
        job_client.set_job_access_token(access_token);
        job_client.set_server_url(server_url);
        if let Ok(pipelines_url) = manifest.pipelines_url() {
            job_client.set_pipelines_url(pipelines_url.to_string());
        }
        if let Some(results_url) = manifest.results_endpoint() {
            info!(results_url, "using Results twirp API for timeline/logs");
            job_client.set_results_url(results_url.to_string());
        } else {
            info!("no results_endpoint, using legacy VSS API for timeline/logs");
        }

        let repo = manifest
            .repository()
            .unwrap_or_else(|_| "unknown/repo".into());

        let workspace = Workspace::create(
            &self.paths.work_dir(),
            &self.paths.tmp_dir(),
            &self.paths.tool_cache_dir(),
            &self.name,
            &repo,
        )
        .context("creating workspace")?;

        let base_env = build_base_env(&manifest, &workspace, &self.name);

        let action_cache = ActionCache::new(self.paths.actions_dir(), client.clone());
        let github_token = manifest.github_token().unwrap_or("").to_string();

        // Start heartbeat
        let job_client = Arc::new(job_client);
        let (heartbeat_handle, heartbeat_cancel) =
            job_client.start_heartbeat(manifest.plan.plan_id.clone(), manifest.plan.job_id.clone());

        let (conclusion, job_outputs) = run_all_steps(
            &manifest,
            &job_client,
            &workspace,
            &base_env,
            &self.name,
            &action_cache,
            &github_token,
        )
        .await
        .context("running job steps")?;

        info!(conclusion = %conclusion, "job steps completed");

        heartbeat_cancel.cancel();
        let _ = heartbeat_handle.await;

        let outputs_payload = outputs_to_variable_values(&job_outputs);
        debug!(job_outputs = ?job_outputs, outputs_payload = %outputs_payload, "completing job");
        job_client
            .complete_job(
                &manifest.plan.plan_id,
                &manifest.plan.job_id,
                &conclusion,
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
                    if msg.message_type != "RunnerJobRequest" {
                        debug!(
                            message_id = msg.message_id,
                            message_type = %msg.message_type,
                            "received control message, skipping"
                        );
                        // Control messages (JobCancellation, BrokerMigration, etc)
                        // are ephemeral — don't try to delete them. Brief pause to
                        // avoid tight-looping when broker keeps resending.
                        let delay = if cfg!(test) { 10 } else { 2000 };
                        tokio::time::sleep(Duration::from_millis(delay)).await;
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
