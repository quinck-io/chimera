use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::RUNNER_VERSION;
use super::auth::TokenManager;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

const BROKER_PROTOCOL_VERSION: &str = "3.0.0";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub enum MessageType {
    RunnerJobRequest,
    JobCancellation,
    Unknown(String),
}

impl std::fmt::Display for MessageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RunnerJobRequest => write!(f, "RunnerJobRequest"),
            Self::JobCancellation => write!(f, "JobCancellation"),
            Self::Unknown(s) => write!(f, "{s}"),
        }
    }
}

impl<'de> Deserialize<'de> for MessageType {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "RunnerJobRequest" => Ok(Self::RunnerJobRequest),
            "JobCancellation" => Ok(Self::JobCancellation),
            _ => Ok(Self::Unknown(s)),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrokerMessage {
    pub message_id: u64,
    pub message_type: MessageType,
    pub body: Option<String>,
}

#[derive(Deserialize)]
struct JobRequestBody {
    runner_request_id: String,
    run_service_url: String,
}

#[derive(Deserialize)]
struct CancellationBody {
    #[serde(rename = "jobId")]
    job_id: String,
}

impl BrokerMessage {
    /// Parse the body of a RunnerJobRequest message into (runner_request_id, run_service_url).
    pub fn parse_job_request(&self) -> Result<(String, String)> {
        let body = self.body.as_deref().context("job message has no body")?;
        let req: JobRequestBody = serde_json::from_str(body).context("parsing job request body")?;
        Ok((req.runner_request_id, req.run_service_url))
    }

    /// Parse the body of a JobCancellation message into the job ID.
    pub fn parse_cancellation_job_id(&self) -> Result<String> {
        let body = self
            .body
            .as_deref()
            .context("cancellation message has no body")?;
        let parsed: CancellationBody =
            serde_json::from_str(body).context("parsing cancellation body")?;
        Ok(parsed.job_id)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BrokerError {
    #[error("broker error: {0}")]
    ServerError(String),

    #[error("unauthorized (401)")]
    Unauthorized,

    #[error("poll timeout")]
    Timeout,

    #[error("connection error: {0}")]
    Connection(String),
}

// ---------------------------------------------------------------------------
// Session request/response
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionRequest {
    session_id: String,
    owner_name: String,
    agent: SessionAgent,
    use_fips_encryption: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionAgent {
    id: u64,
    name: String,
    version: String,
    os_description: String,
    ephemeral: bool,
    status: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateSessionResponse {
    session_id: String,
}

// ---------------------------------------------------------------------------
// BrokerClient
// ---------------------------------------------------------------------------

pub struct BrokerClient {
    client: reqwest::Client,
    server_url: String,
    session_id: String,
    token_manager: Arc<TokenManager>,
}

impl BrokerClient {
    /// Create a client with a pre-existing session.
    pub fn new(
        client: reqwest::Client,
        server_url: String,
        session_id: String,
        token_manager: Arc<TokenManager>,
    ) -> Self {
        Self {
            client,
            server_url,
            session_id,
            token_manager,
        }
    }

    /// Create a new broker session and return a connected client.
    pub async fn connect(
        client: reqwest::Client,
        server_url: &str,
        token_manager: Arc<TokenManager>,
        agent_id: u64,
        agent_name: &str,
    ) -> Result<Self> {
        let token = token_manager
            .get_token()
            .await
            .context("getting token for session")?;

        let session_id = uuid::Uuid::new_v4().to_string();
        let owner_name = format!(
            "{} (PID: {})",
            hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|_| "unknown".into()),
            std::process::id()
        );

        let body = CreateSessionRequest {
            session_id,
            owner_name,
            agent: SessionAgent {
                id: agent_id,
                name: agent_name.to_string(),
                version: RUNNER_VERSION.to_string(),
                os_description: format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
                ephemeral: true,
                status: 0,
            },
            use_fips_encryption: false,
        };

        debug!(agent_id, agent_name, "creating broker session");

        let url = format!("{}/session", server_url.trim_end_matches('/'));
        let resp = client
            .post(&url)
            .bearer_auth(&token)
            .json(&body)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .context("sending create session request")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            bail!("create session failed ({status}): {body_text}");
        }

        let session: CreateSessionResponse = resp
            .json()
            .await
            .context("parsing create session response")?;

        debug!(session_id = %session.session_id, "broker session created");

        Ok(Self {
            client,
            server_url: server_url.to_string(),
            session_id: session.session_id,
            token_manager,
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn server_url(&self) -> &str {
        &self.server_url
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn token_manager(&self) -> &TokenManager {
        &self.token_manager
    }

    pub fn token_manager_arc(&self) -> Arc<TokenManager> {
        self.token_manager.clone()
    }

    /// Single poll request. Returns Some(message) on 200, None on 202.
    pub async fn poll_message(&self) -> Result<Option<BrokerMessage>> {
        let token = self
            .token_manager
            .get_token()
            .await
            .context("getting token for poll")?;

        let url = format!(
            "{}/message?sessionId={}&status=Online&runnerVersion={}&disableUpdate=true",
            self.server_url.trim_end_matches('/'),
            self.session_id,
            BROKER_PROTOCOL_VERSION,
        );

        let resp = self
            .client
            .get(&url)
            .bearer_auth(&token)
            .timeout(Duration::from_secs(55))
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    BrokerError::Timeout
                } else {
                    BrokerError::Connection(e.to_string())
                }
            })
            .context("sending poll request")?;

        let status = resp.status();
        debug!(status = %status, "poll response");

        match status.as_u16() {
            202 => Ok(None),
            200 => {
                let msg: BrokerMessage = resp.json().await.context("parsing broker message")?;
                Ok(Some(msg))
            }
            401 => Err(BrokerError::Unauthorized.into()),
            s if (500..600).contains(&s) => {
                let body = resp.text().await.unwrap_or_default();
                Err(BrokerError::ServerError(format!("{s}: {body}")).into())
            }
            other => {
                let body = resp.text().await.unwrap_or_default();
                bail!("unexpected poll status {other}: {body}");
            }
        }
    }

    /// Acknowledge a job message via POST /acknowledge (V2 broker protocol).
    pub async fn ack_job(&self, runner_request_id: &str) -> Result<()> {
        let token = self.token_manager.get_token().await?;

        let url = format!(
            "{}/acknowledge?sessionId={}&runnerVersion={}&status=Online&disableUpdate=true",
            self.server_url.trim_end_matches('/'),
            self.session_id,
            BROKER_PROTOCOL_VERSION,
        );

        let body = serde_json::json!({
            "runnerRequestId": runner_request_id,
        });

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&token)
            .json(&body)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .context("sending ack request")?;

        let status = resp.status();
        if !status.is_success() {
            let resp_body = resp.text().await.unwrap_or_default();
            tracing::warn!(runner_request_id, status = %status, "ack failed: {resp_body}");
        } else {
            debug!(runner_request_id, "job acknowledged");
        }

        Ok(())
    }

    /// Delete the broker session.
    pub async fn disconnect(&self) -> Result<()> {
        let token = self.token_manager.get_token().await.unwrap_or_default();

        let url = format!("{}/session", self.server_url.trim_end_matches('/'));

        debug!(session_id = %self.session_id, "deleting broker session");

        let resp = self
            .client
            .delete(&url)
            .bearer_auth(&token)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .context("sending delete session request")?;

        let status = resp.status();
        if !status.is_success() && status.as_u16() != 404 {
            let body_text = resp.text().await.unwrap_or_default();
            bail!("delete session failed ({status}): {body_text}");
        }

        debug!("broker session deleted");
        Ok(())
    }
}

#[cfg(test)]
#[path = "broker_test.rs"]
mod broker_test;
