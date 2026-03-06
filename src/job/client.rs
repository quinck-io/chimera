use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::manifest;
use super::schema::JobManifest;
use super::timeline::TimelineRecord;
use crate::github::auth::TokenManager;
use crate::utils::format_results_timestamp;

pub struct JobClient {
    client: reqwest::Client,
    token_manager: Arc<TokenManager>,
    run_service_url: String,
    server_url: String,
    pipelines_url: Option<String>,
    results_url: Option<String>,
    job_access_token: Option<String>,
    change_id: AtomicI64,
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
            pipelines_url: None,
            results_url: None,
            job_access_token: None,
            change_id: AtomicI64::new(0),
        }
    }

    pub fn set_job_access_token(&mut self, token: String) {
        self.job_access_token = Some(token);
    }

    pub fn set_server_url(&mut self, url: String) {
        self.server_url = url;
    }

    pub fn set_pipelines_url(&mut self, url: String) {
        self.pipelines_url = Some(url);
    }

    pub fn set_results_url(&mut self, url: String) {
        self.results_url = Some(url);
    }

    pub fn has_results_url(&self) -> bool {
        self.results_url.is_some()
    }

    fn pipelines_base_url(&self) -> &str {
        self.pipelines_url.as_deref().unwrap_or(&self.server_url)
    }

    fn results_base_url(&self) -> Result<&str> {
        self.results_url.as_deref().context("results URL not set")
    }

    fn job_token(&self) -> Result<&str> {
        self.job_access_token
            .as_deref()
            .context("job access token not set")
    }

    // ─── Run Service APIs (OAuth token) ─────────────────────────────

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
        let body_text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!("acquire job failed ({status}): {body_text}");
        }

        debug!(manifest_length = body_text.len(), "received job manifest");

        let raw: serde_json::Value =
            serde_json::from_str(&body_text).context("parsing raw job manifest JSON")?;
        let normalized = manifest::normalize_manifest(&raw);

        debug!(normalized = %normalized, "normalized manifest");

        serde_json::from_value(normalized).with_context(|| {
            let preview = if body_text.len() > 2000 {
                format!("{}...(truncated)", &body_text[..2000])
            } else {
                body_text.clone()
            };
            format!("deserializing normalized manifest: {preview}")
        })
    }

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

    pub async fn complete_job(
        &self,
        plan_id: &str,
        job_id: &str,
        conclusion: &str,
        outputs: &serde_json::Value,
        step_results: &[CompletionStepResult],
    ) -> Result<()> {
        let token = self.job_token()?;
        let url = format!("{}/completejob", self.server_url.trim_end_matches('/'));

        let body = serde_json::json!({
            "planId": plan_id,
            "jobId": job_id,
            "conclusion": conclusion,
            "outputs": outputs,
            "stepResults": step_results,
        });

        debug!(conclusion, "completing job");

        let resp = self
            .client
            .post(&url)
            .bearer_auth(token)
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

    // ─── Results Twirp API (job access token) ───────────────────────

    pub async fn update_steps(
        &self,
        plan_id: &str,
        job_id: &str,
        steps: &[ResultsStep],
    ) -> Result<()> {
        let token = self.job_token()?;
        let base = self.results_base_url()?.trim_end_matches('/');
        let url = format!(
            "{base}/twirp/github.actions.results.api.v1.WorkflowStepUpdateService/WorkflowStepsUpdate"
        );

        let change_order = self.change_id.fetch_add(1, Ordering::Relaxed) + 1;

        let body = serde_json::json!({
            "steps": steps,
            "change_order": change_order,
            "workflow_run_backend_id": plan_id,
            "workflow_job_run_backend_id": job_id,
        });

        let resp = self
            .client
            .post(&url)
            .bearer_auth(token)
            .json(&body)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .context("sending update steps request")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            warn!("update steps failed ({status}): {body_text}");
        }

        Ok(())
    }

    pub async fn upload_step_log(
        &self,
        plan_id: &str,
        job_id: &str,
        step_id: &str,
        content: &str,
        line_count: i64,
    ) -> Result<()> {
        let token = self.job_token()?;
        let base = self.results_base_url()?.trim_end_matches('/');

        let signed_url = self
            .get_signed_url(
                token,
                &format!(
                    "{base}/twirp/results.services.receiver.Receiver/GetStepLogsSignedBlobURL"
                ),
                &serde_json::json!({
                    "workflow_run_backend_id": plan_id,
                    "workflow_job_run_backend_id": job_id,
                    "step_backend_id": step_id,
                }),
            )
            .await?;

        self.upload_to_blob(&signed_url, content).await?;

        self.post_log_metadata(
            token,
            &format!("{base}/twirp/results.services.receiver.Receiver/CreateStepLogsMetadata"),
            &serde_json::json!({
                "workflow_run_backend_id": plan_id,
                "workflow_job_run_backend_id": job_id,
                "step_backend_id": step_id,
                "uploaded_at": format_results_timestamp(Utc::now()),
                "line_count": line_count,
            }),
        )
        .await
    }

    pub async fn upload_job_log(
        &self,
        plan_id: &str,
        job_id: &str,
        content: &str,
        line_count: i64,
    ) -> Result<()> {
        let token = self.job_token()?;
        let base = self.results_base_url()?.trim_end_matches('/');

        let signed_url = self
            .get_signed_url(
                token,
                &format!("{base}/twirp/results.services.receiver.Receiver/GetJobLogsSignedBlobURL"),
                &serde_json::json!({
                    "workflow_run_backend_id": plan_id,
                    "workflow_job_run_backend_id": job_id,
                }),
            )
            .await?;

        self.upload_to_blob(&signed_url, content).await?;

        self.post_log_metadata(
            token,
            &format!("{base}/twirp/results.services.receiver.Receiver/CreateJobLogsMetadata"),
            &serde_json::json!({
                "workflow_run_backend_id": plan_id,
                "workflow_job_run_backend_id": job_id,
                "uploaded_at": format_results_timestamp(Utc::now()),
                "line_count": line_count,
            }),
        )
        .await
    }

    /// Get a signed URL from the Results API for blob upload.
    async fn get_signed_url(
        &self,
        token: &str,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<SignedUrlResponse> {
        let resp = self
            .client
            .post(url)
            .bearer_auth(token)
            .json(body)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .context("getting signed URL")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            bail!("get signed URL failed ({status}): {body_text}");
        }

        let signed: SignedUrlResponse = resp.json().await.context("parsing signed URL response")?;
        if signed.logs_url.is_empty() {
            bail!("empty logs_url in signed URL response");
        }

        Ok(signed)
    }

    /// Upload content to an Azure Blob Storage signed URL (create + append + seal).
    async fn upload_to_blob(&self, signed: &SignedUrlResponse, content: &str) -> Result<()> {
        let is_azure = signed.blob_storage_type == "BLOB_STORAGE_TYPE_AZURE";

        // Create the append blob
        let mut create_req = self.client.put(&signed.logs_url).body("");
        if is_azure {
            create_req = create_req
                .header("x-ms-blob-type", "AppendBlob")
                .header("Content-Length", "0");
        }
        let create_resp = create_req
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .context("creating append blob")?;
        if !create_resp.status().is_success() {
            let body = create_resp.text().await.unwrap_or_default();
            warn!("create append blob failed: {body}");
        }

        // Upload content and seal
        let upload_url = format!("{}&comp=appendblock&seal=true", signed.logs_url);
        let mut upload_req = self.client.put(&upload_url).body(content.to_string());
        if is_azure {
            upload_req = upload_req
                .header("x-ms-blob-sealed", "true")
                .header("Content-Length", content.len().to_string());
        }
        let upload_resp = upload_req
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .context("uploading blob content")?;
        if !upload_resp.status().is_success() {
            let body = upload_resp.text().await.unwrap_or_default();
            warn!("upload blob content failed: {body}");
        }

        Ok(())
    }

    /// Post log metadata to the Results API after blob upload.
    async fn post_log_metadata(
        &self,
        token: &str,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<()> {
        let resp = self
            .client
            .post(url)
            .bearer_auth(token)
            .json(body)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .context("creating log metadata")?;

        if !resp.status().is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            warn!("create log metadata failed: {body_text}");
        }

        Ok(())
    }

    // ─── Legacy VSS APIs (kept for fallback) ────────────────────────

    pub async fn create_log(&self, plan_id: &str, log_name: &str) -> Result<u64> {
        let token = self.job_token()?;
        let base = self.pipelines_base_url().trim_end_matches('/');
        let url = format!("{base}/_apis/pipelines/workflows/{plan_id}/logs");

        let body = serde_json::json!({ "path": format!("logs/{log_name}") });

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

    pub async fn upload_log_lines(&self, plan_id: &str, log_id: u64, lines: &str) -> Result<()> {
        let token = self.job_token()?;
        let base = self.pipelines_base_url().trim_end_matches('/');
        let url = format!("{base}/_apis/pipelines/workflows/{plan_id}/logs/{log_id}");

        let resp = self
            .client
            .post(&url)
            .bearer_auth(token)
            .header("Content-Type", "application/octet-stream")
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

    pub async fn update_timeline(
        &self,
        plan_id: &str,
        timeline_id: &str,
        records: &[TimelineRecord],
    ) -> Result<()> {
        let token = self.job_token()?;
        let base = self.pipelines_base_url().trim_end_matches('/');
        let url =
            format!("{base}/_apis/pipelines/workflows/{plan_id}/timelines/{timeline_id}/records");

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

    // ─── Heartbeat ──────────────────────────────────────────────────

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
            interval.tick().await;

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

// ─── Types ──────────────────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
pub struct SignedUrlResponse {
    #[serde(default)]
    pub logs_url: String,
    #[serde(default)]
    pub blob_storage_type: String,
}

/// Step state for Results twirp WorkflowStepsUpdate API.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ResultsStep {
    pub external_id: String,
    pub number: u32,
    pub name: String,
    pub status: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    pub conclusion: i32,
}

pub const STATUS_IN_PROGRESS: i32 = 1;
pub const STATUS_COMPLETED: i32 = 3;
pub const CONCLUSION_UNKNOWN: i32 = 0;
pub const CONCLUSION_SUCCESS: i32 = 2;
pub const CONCLUSION_FAILURE: i32 = 3;
pub const CONCLUSION_CANCELLED: i32 = 4;
pub const CONCLUSION_SKIPPED: i32 = 5;

/// Step result included in the /completejob request body.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompletionStepResult {
    pub external_id: String,
    pub number: u32,
    pub name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

#[cfg(test)]
#[path = "client_test.rs"]
mod client_test;
