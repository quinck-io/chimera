pub mod commands;
pub mod execute;
pub mod logs;
pub mod schema;
pub mod timeline;
pub mod workspace;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::github::auth::TokenManager;
use schema::JobManifest;
use timeline::TimelineRecord;

pub struct JobClient {
    client: reqwest::Client,
    token_manager: Arc<TokenManager>,
    run_service_url: String,
    server_url: String,
    job_access_token: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateLogResponse {
    id: u64,
}

impl JobClient {
    pub fn new(
        client: reqwest::Client,
        token_manager: Arc<TokenManager>,
        run_service_url: String,
        server_url: String,
    ) -> Self {
        Self {
            client,
            token_manager,
            run_service_url,
            server_url,
            job_access_token: None,
        }
    }

    pub fn set_job_access_token(&mut self, token: String) {
        self.job_access_token = Some(token);
    }

    fn job_token(&self) -> Result<&str> {
        self.job_access_token
            .as_deref()
            .context("job access token not set")
    }

    /// POST {run_service_url}/acquirejob
    pub async fn acquire_job(&self, runner_request_id: &str) -> Result<JobManifest> {
        let token = self.token_manager.get_token().await?;
        let url = format!("{}/acquirejob", self.run_service_url.trim_end_matches('/'));

        let body = serde_json::json!({
            "jobMessageId": runner_request_id,
            "runnerOS": "Linux",
        });

        debug!(url = %url, "acquiring job");

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .json(&body)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("sending acquire job request")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            bail!("acquire job failed ({status}): {body_text}");
        }

        resp.json().await.context("parsing job manifest")
    }

    /// POST {run_service_url}/renewjob
    pub async fn renew_job(&self, plan_id: &str, job_id: &str) -> Result<()> {
        let token = self.token_manager.get_token().await?;
        let url = format!("{}/renewjob", self.run_service_url.trim_end_matches('/'));

        let body = serde_json::json!({
            "planId": plan_id,
            "jobId": job_id,
        });

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .json(&body)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("sending renew job request")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            warn!("renew job failed ({status}): {body_text}");
        }

        Ok(())
    }

    /// POST {run_service_url}/completejob
    pub async fn complete_job(
        &self,
        plan_id: &str,
        job_id: &str,
        conclusion: &str,
        outputs: &serde_json::Value,
    ) -> Result<()> {
        let token = self.token_manager.get_token().await?;
        let url = format!("{}/completejob", self.run_service_url.trim_end_matches('/'));

        let body = serde_json::json!({
            "planId": plan_id,
            "jobId": job_id,
            "conclusion": conclusion,
            "outputs": outputs,
        });

        debug!(conclusion, "completing job");

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .json(&body)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("sending complete job request")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            bail!("complete job failed ({status}): {body_text}");
        }

        Ok(())
    }

    /// Create a log resource for a step. Returns the log ID.
    pub async fn create_log(&self, plan_id: &str, log_name: &str) -> Result<u64> {
        let token = self.job_token()?;
        let url = format!(
            "{}/_apis/pipelines/workflows/{}/logs",
            self.server_url.trim_end_matches('/'),
            plan_id
        );

        let body = serde_json::json!({ "name": log_name });

        let resp = self
            .client
            .post(&url)
            .bearer_auth(token)
            .json(&body)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .context("sending create log request")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            bail!("create log failed ({status}): {body_text}");
        }

        let log_resp: CreateLogResponse =
            resp.json().await.context("parsing create log response")?;
        Ok(log_resp.id)
    }

    /// Upload log lines as text/plain.
    pub async fn upload_log_lines(&self, plan_id: &str, log_id: u64, lines: &str) -> Result<()> {
        let token = self.job_token()?;
        let url = format!(
            "{}/_apis/pipelines/workflows/{}/logs/{}",
            self.server_url.trim_end_matches('/'),
            plan_id,
            log_id
        );

        let resp = self
            .client
            .post(&url)
            .bearer_auth(token)
            .header("Content-Type", "text/plain")
            .body(lines.to_string())
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .context("sending upload log lines request")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            warn!("upload log lines failed ({status}): {body_text}");
        }

        Ok(())
    }

    /// PATCH timeline records.
    pub async fn update_timeline(
        &self,
        plan_id: &str,
        timeline_id: &str,
        records: &[TimelineRecord],
    ) -> Result<()> {
        let token = self.job_token()?;
        let url = format!(
            "{}/_apis/distributedtask/hubs/build/plans/{}/timelines/{}",
            self.server_url.trim_end_matches('/'),
            plan_id,
            timeline_id
        );

        let body = serde_json::json!({
            "value": records,
            "count": records.len(),
        });

        let resp = self
            .client
            .patch(&url)
            .bearer_auth(token)
            .json(&body)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .context("sending update timeline request")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            warn!("update timeline failed ({status}): {body_text}");
        }

        Ok(())
    }

    /// Start a heartbeat task that calls renewjob every 60s.
    /// Returns the task handle and a cancellation token.
    /// Must be called on an Arc<JobClient>.
    pub fn start_heartbeat(
        self: &Arc<Self>,
        plan_id: String,
        job_id: String,
    ) -> (JoinHandle<()>, CancellationToken) {
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let client = self.clone();

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            interval.tick().await; // skip first immediate tick

            loop {
                tokio::select! {
                    _ = interval.tick() => {},
                    _ = cancel_clone.cancelled() => {
                        debug!("heartbeat cancelled");
                        return;
                    }
                }

                if let Err(e) = client.renew_job(&plan_id, &job_id).await {
                    warn!(error = %e, "heartbeat: renew failed");
                } else {
                    debug!("heartbeat: job renewed");
                }
            }
        });

        (handle, cancel)
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod mod_test;
