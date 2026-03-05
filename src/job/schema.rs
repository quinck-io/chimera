use std::collections::HashMap;

use anyhow::{Context, bail};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobManifest {
    pub plan: Plan,
    pub steps: Vec<Step>,
    #[serde(default)]
    pub variables: HashMap<String, JobVariable>,
    pub resources: JobResources,
    pub context_data: serde_json::Value,
    pub job_container: Option<serde_json::Value>,
    pub service_containers: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub plan_id: String,
    pub job_id: String,
    pub timeline_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub id: String,
    pub display_name: String,
    pub reference: StepReference,
    #[serde(default)]
    pub inputs: HashMap<String, String>,
    pub condition: Option<String>,
    pub timeout_in_minutes: Option<u64>,
    #[serde(default)]
    pub continue_on_error: bool,
    #[serde(default)]
    pub order: u32,
    pub environment: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepReference {
    pub name: String,
    pub r#type: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobVariable {
    pub value: String,
    #[serde(default)]
    pub is_secret: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobResources {
    pub endpoints: Vec<ServiceEndpoint>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceEndpoint {
    pub name: String,
    pub url: String,
    pub authorization: Option<EndpointAuthorization>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointAuthorization {
    pub scheme: String,
    pub parameters: HashMap<String, String>,
}

impl JobManifest {
    /// Find the SystemVssConnection endpoint URL (used for logs/timeline API).
    pub fn server_url(&self) -> anyhow::Result<&str> {
        self.resources
            .endpoints
            .iter()
            .find(|e| e.name == "SystemVssConnection")
            .map(|e| e.url.as_str())
            .context("no SystemVssConnection endpoint in manifest")
    }

    /// Extract the AccessToken from the SystemVssConnection endpoint.
    pub fn access_token(&self) -> anyhow::Result<&str> {
        let endpoint = self
            .resources
            .endpoints
            .iter()
            .find(|e| e.name == "SystemVssConnection")
            .context("no SystemVssConnection endpoint in manifest")?;

        let auth = endpoint
            .authorization
            .as_ref()
            .context("SystemVssConnection has no authorization")?;

        if auth.scheme != "OAuth" {
            bail!("unexpected auth scheme '{}', expected OAuth", auth.scheme);
        }

        auth.parameters
            .get("AccessToken")
            .map(|s| s.as_str())
            .context("no AccessToken in SystemVssConnection authorization")
    }

    /// Extract the repository full name (e.g. "owner/repo") from context_data.
    pub fn repository(&self) -> anyhow::Result<String> {
        if let Some(github) = self.context_data.get("github")
            && let Some(repo) = github.get("repository").and_then(|v| v.as_str())
        {
            return Ok(repo.to_string());
        }
        bail!("no github.repository in context_data")
    }

    /// Whether the job should run in a container (Phase 3).
    pub fn has_container(&self) -> bool {
        self.job_container.is_some()
    }

    /// Whether the job has service containers (Phase 3).
    pub fn has_services(&self) -> bool {
        self.service_containers
            .as_ref()
            .is_some_and(|s| !s.is_empty())
    }
}

#[cfg(test)]
#[path = "schema_test.rs"]
mod schema_test;
