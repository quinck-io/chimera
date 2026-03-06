use std::collections::HashMap;

use anyhow::{Context, bail};
use serde::Deserialize;

use crate::utils::deserialize_nullable_bool;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobManifest {
    #[serde(default)]
    pub plan: Plan,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(default)]
    pub variables: HashMap<String, JobVariable>,
    #[serde(default)]
    pub resources: JobResources,
    #[serde(default)]
    pub context_data: serde_json::Value,
    pub job_container: Option<serde_json::Value>,
    pub service_containers: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    pub mask: Vec<serde_json::Value>,
    #[serde(default)]
    pub file_table: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    #[serde(default)]
    pub plan_id: String,
    #[serde(default)]
    pub job_id: String,
    #[serde(default)]
    pub timeline_id: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobResources {
    #[serde(default)]
    pub endpoints: Vec<ServiceEndpoint>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub reference: StepReference,
    #[serde(default)]
    pub inputs: HashMap<String, String>,
    pub condition: Option<String>,
    pub timeout_in_minutes: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_nullable_bool")]
    pub continue_on_error: bool,
    #[serde(default)]
    pub order: u32,
    pub environment: Option<HashMap<String, String>>,
}

impl Step {
    /// Whether this step is a `run:` script (vs an action reference).
    pub fn is_script(&self) -> bool {
        self.reference.kind == "script"
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepReference {
    #[serde(default)]
    pub name: String,
    /// "script" for `run:` steps, "action" for action steps.
    /// Deserialized from the JSON `type` field.
    #[serde(default, rename = "type")]
    pub kind: String,
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
pub struct ServiceEndpoint {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub url: String,
    pub authorization: Option<EndpointAuthorization>,
    #[serde(default)]
    pub data: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointAuthorization {
    #[serde(default)]
    pub scheme: String,
    #[serde(default)]
    pub parameters: HashMap<String, String>,
}

impl JobManifest {
    pub fn server_url(&self) -> anyhow::Result<&str> {
        self.find_vss_endpoint()
            .map(|e| e.url.as_str())
            .context("no SystemVssConnection endpoint in manifest")
    }

    pub fn access_token(&self) -> anyhow::Result<&str> {
        let endpoint = self
            .find_vss_endpoint()
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

    pub fn pipelines_url(&self) -> anyhow::Result<&str> {
        let endpoint = self
            .find_vss_endpoint()
            .context("no SystemVssConnection endpoint in manifest")?;

        endpoint
            .data
            .get("PipelinesServiceUrl")
            .map(|s| s.as_str())
            .context("no PipelinesServiceUrl in SystemVssConnection data")
    }

    pub fn repository(&self) -> anyhow::Result<String> {
        if let Some(github) = self.context_data.get("github")
            && let Some(repo) = github.get("repository").and_then(|v| v.as_str())
        {
            return Ok(repo.to_string());
        }
        bail!("no github.repository in context_data")
    }

    pub fn has_container(&self) -> bool {
        self.job_container.is_some()
    }

    pub fn has_services(&self) -> bool {
        self.service_containers
            .as_ref()
            .is_some_and(|s| !s.is_empty())
    }

    pub fn results_endpoint(&self) -> Option<&str> {
        self.variables
            .get("system.github.results_endpoint")
            .map(|v| v.value.as_str())
    }

    pub fn mask_regexes(&self) -> &[serde_json::Value] {
        &self.mask
    }

    pub fn file_table(&self) -> &[String] {
        &self.file_table
    }

    fn find_vss_endpoint(&self) -> Option<&ServiceEndpoint> {
        self.resources
            .endpoints
            .iter()
            .find(|e| e.name == "SystemVssConnection")
    }
}

#[cfg(test)]
#[path = "schema_test.rs"]
mod schema_test;
