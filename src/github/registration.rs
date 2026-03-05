use std::path::Path;

use anyhow::{Context, Result, bail};
use rsa::RsaPrivateKey;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use super::RUNNER_VERSION;
use crate::config::{
    ChimeraConfig, OAuthCredentials, RunnerCredentials, RunnerInfo, load_config,
    private_key_to_rsa_params, public_key_to_xml, save_config, save_runner_credentials,
};

// ---------------------------------------------------------------------------
// GitHub URL parsing
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum GitHubTarget {
    Repo { owner: String, repo: String },
    Org { org: String },
}

impl GitHubTarget {
    pub fn parse(url: &str) -> Result<Self> {
        let url = url.trim_end_matches('/');

        let path = url
            .strip_prefix("https://github.com/")
            .or_else(|| url.strip_prefix("http://github.com/"))
            .context("URL must start with https://github.com/")?;

        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        match parts.len() {
            1 => Ok(GitHubTarget::Org {
                org: parts[0].to_string(),
            }),
            2 => Ok(GitHubTarget::Repo {
                owner: parts[0].to_string(),
                repo: parts[1].to_string(),
            }),
            _ => bail!("invalid GitHub URL: expected org or owner/repo path"),
        }
    }

    pub fn api_base(&self) -> String {
        "https://api.github.com".to_string()
    }

    pub fn github_url(&self) -> String {
        match self {
            GitHubTarget::Repo { owner, repo } => {
                format!("https://github.com/{owner}/{repo}")
            }
            GitHubTarget::Org { org } => format!("https://github.com/{org}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Registration API types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct RunnerRegistrationRequest {
    url: String,
    runner_event: String,
}

#[derive(Debug, Deserialize)]
struct GitHubAuthResult {
    url: String,
    token: String,
    #[serde(flatten)]
    extra: serde_json::Value,
}

// ---------------------------------------------------------------------------
// V1 registration types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskAgentV1 {
    name: String,
    version: String,
    os_description: String,
    enabled: bool,
    status: u32,
    provisioning_state: String,
    authorization: TaskAgentAuthorizationV1,
    labels: Vec<AgentLabelV1>,
    max_parallelism: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskAgentAuthorizationV1 {
    public_key: TaskAgentPublicKeyV1,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TaskAgentPublicKeyV1 {
    exponent: String,
    modulus: String,
}

#[derive(Debug, Serialize)]
struct AgentLabelV1 {
    name: String,
    #[serde(rename = "type")]
    label_type: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskAgentResponseV1 {
    id: u64,
    name: String,
    authorization: TaskAgentAuthResponseV1,
    #[serde(default)]
    properties: std::collections::HashMap<String, PropertyValue>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskAgentAuthResponseV1 {
    authorization_url: String,
    client_id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PropertyValue {
    #[serde(rename = "$value")]
    value: serde_json::Value,
}

// ---------------------------------------------------------------------------
// V2 registration types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct RegisterRunnerRequestV2 {
    url: String,
    group_id: u64,
    name: String,
    version: String,
    updates_disabled: bool,
    ephemeral: bool,
    labels: Vec<AgentLabelV2>,
    public_key: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct AgentLabelV2 {
    name: String,
    #[serde(rename = "type")]
    label_type: String,
}

#[derive(Debug, Deserialize)]
struct RegisterRunnerResponseV2 {
    id: u64,
    name: String,
    authorization: RunnerAuthorizationV2,
}

#[derive(Debug, Deserialize)]
struct RunnerAuthorizationV2 {
    authorization_url: String,
    server_url: String,
    client_id: String,
}

// ---------------------------------------------------------------------------
// Registration result (unified from V1 or V2)
// ---------------------------------------------------------------------------

struct RegistrationResult {
    agent_id: u64,
    agent_name: String,
    server_url: String,
    server_url_v2: String,
    authorization_url: String,
    client_id: String,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub async fn register(
    url: &str,
    token: &str,
    name: &str,
    labels: &[String],
    root: &Path,
) -> Result<()> {
    let target = GitHubTarget::parse(url)?;
    let client = reqwest::Client::builder()
        .user_agent(format!("chimera/{RUNNER_VERSION}"))
        .build()
        .context("building HTTP client")?;

    info!("authenticating with GitHub...");
    let auth = github_auth(&client, &target, token).await?;
    debug!(tenant_url = %auth.url, extra = %auth.extra, "got tenant credentials");

    info!("generating RSA key pair...");
    let private_key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)
        .context("generating RSA-2048 key pair")?;

    info!(name = name, "registering runner...");
    let result = register_v2_or_v1(&client, &target, &auth, name, &private_key, labels).await?;

    info!(
        agent_id = result.agent_id,
        name = %result.agent_name,
        "runner registered successfully"
    );

    let rsa_params = private_key_to_rsa_params(&private_key);
    let creds = RunnerCredentials {
        info: RunnerInfo {
            agent_id: result.agent_id,
            agent_name: result.agent_name,
            pool_id: 1,
            server_url: result.server_url,
            server_url_v2: result.server_url_v2,
            git_hub_url: target.github_url(),
            work_folder: "_work".into(),
            use_v2_flow: true,
        },
        oauth: OAuthCredentials {
            scheme: "OAuth".into(),
            client_id: result.client_id,
            authorization_url: result.authorization_url,
        },
        rsa_params,
    };

    let runners_dir = root.join("runners");
    save_runner_credentials(&runners_dir, name, &creds).context("saving runner credentials")?;

    let config_path = root.join("config.toml");
    let mut config = if config_path.exists() {
        load_config(&config_path).unwrap_or_default()
    } else {
        ChimeraConfig::default()
    };

    if !config.runners.contains(&name.to_string()) {
        config.runners.push(name.to_string());
    }
    save_config(&config_path, &config).context("saving config")?;

    info!(
        "runner '{}' registered. Credentials saved to {}",
        name,
        runners_dir.join(name).display()
    );

    Ok(())
}

pub async fn unregister(name: &str, root: &Path) -> Result<()> {
    let runners_dir = root.join("runners");
    let runner_dir = runners_dir.join(name);

    if !runner_dir.exists() {
        bail!("runner '{}' not found at {}", name, runner_dir.display());
    }

    std::fs::remove_dir_all(&runner_dir)
        .with_context(|| format!("removing runner directory {}", runner_dir.display()))?;

    let config_path = root.join("config.toml");
    if config_path.exists() {
        let mut config = load_config(&config_path).unwrap_or_default();
        config.runners.retain(|r| r != name);
        save_config(&config_path, &config)?;
    }

    info!("runner '{}' unregistered and local files removed", name);
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal: registration flow
// ---------------------------------------------------------------------------

async fn register_v2_or_v1(
    client: &reqwest::Client,
    target: &GitHubTarget,
    auth: &GitHubAuthResult,
    name: &str,
    private_key: &RsaPrivateKey,
    labels: &[String],
) -> Result<RegistrationResult> {
    let public_key_xml = public_key_to_xml(private_key);

    match try_register_v2(client, target, auth, name, &public_key_xml, labels).await {
        Ok(result) => return Ok(result),
        Err(e) => {
            debug!(error = %e, "V2 registration not available, trying V1");
        }
    }

    register_v1(client, auth, name, private_key, labels).await
}

async fn try_register_v2(
    client: &reqwest::Client,
    target: &GitHubTarget,
    auth: &GitHubAuthResult,
    name: &str,
    public_key_xml: &str,
    labels: &[String],
) -> Result<RegistrationResult> {
    let url = format!("{}/actions/runners/register", target.api_base());

    let mut all_labels = vec![
        AgentLabelV2 {
            name: "self-hosted".into(),
            label_type: "system".into(),
        },
        AgentLabelV2 {
            name: "Linux".into(),
            label_type: "system".into(),
        },
        AgentLabelV2 {
            name: "X64".into(),
            label_type: "system".into(),
        },
    ];
    for label in labels {
        all_labels.push(AgentLabelV2 {
            name: label.clone(),
            label_type: "custom".into(),
        });
    }

    let body = RegisterRunnerRequestV2 {
        url: target.github_url(),
        group_id: 1,
        name: name.to_string(),
        version: RUNNER_VERSION.to_string(),
        updates_disabled: true,
        ephemeral: false,
        labels: all_labels,
        public_key: public_key_xml.to_string(),
    };

    let resp = client
        .post(&url)
        .bearer_auth(&auth.token)
        .json(&body)
        .send()
        .await
        .context("sending V2 runner registration request")?;

    let status = resp.status();
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        bail!("V2 runner registration failed ({status}): {body_text}");
    }

    let runner: RegisterRunnerResponseV2 = resp
        .json()
        .await
        .context("parsing V2 runner registration response")?;

    Ok(RegistrationResult {
        agent_id: runner.id,
        agent_name: runner.name,
        server_url: auth.url.clone(),
        server_url_v2: runner.authorization.server_url,
        authorization_url: runner.authorization.authorization_url,
        client_id: runner.authorization.client_id,
    })
}

async fn register_v1(
    client: &reqwest::Client,
    auth: &GitHubAuthResult,
    name: &str,
    private_key: &RsaPrivateKey,
    labels: &[String],
) -> Result<RegistrationResult> {
    let tenant_url = auth.url.trim_end_matches('/');
    info!(
        tenant_url = tenant_url,
        "using V1 registration via pipelines API"
    );

    use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
    use rsa::traits::PublicKeyParts;
    let modulus = BASE64.encode(private_key.n().to_bytes_be());
    let exponent = BASE64.encode(private_key.e().to_bytes_be());
    debug!(
        modulus_len = private_key.n().to_bytes_be().len(),
        modulus_first8 = %&modulus[..8],
        exponent = %exponent,
        "public key for registration"
    );

    let mut all_labels = vec![
        AgentLabelV1 {
            name: "self-hosted".into(),
            label_type: "system".into(),
        },
        AgentLabelV1 {
            name: "Linux".into(),
            label_type: "system".into(),
        },
        AgentLabelV1 {
            name: "X64".into(),
            label_type: "system".into(),
        },
    ];
    for label in labels {
        all_labels.push(AgentLabelV1 {
            name: label.clone(),
            label_type: "custom".into(),
        });
    }

    let agent_body = TaskAgentV1 {
        name: name.to_string(),
        version: RUNNER_VERSION.to_string(),
        os_description: "Linux".to_string(),
        enabled: true,
        status: 0,
        provisioning_state: "Provisioned".to_string(),
        authorization: TaskAgentAuthorizationV1 {
            public_key: TaskAgentPublicKeyV1 { exponent, modulus },
        },
        labels: all_labels,
        max_parallelism: 1,
    };

    let register_url = format!(
        "{}/_apis/distributedtask/pools/1/agents?api-version=6.0-preview",
        tenant_url
    );

    let resp = client
        .post(&register_url)
        .bearer_auth(&auth.token)
        .json(&agent_body)
        .send()
        .await
        .context("sending V1 agent registration request")?;

    let status = resp.status();
    let body_text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("V1 agent registration failed ({status}): {body_text}");
    }

    debug!(raw_response = %body_text, "V1 registration response");

    let agent: TaskAgentResponseV1 =
        serde_json::from_str(&body_text).context("parsing V1 agent registration response")?;

    let broker_url = agent
        .properties
        .get("ServerUrlV2")
        .and_then(|v| v.value.as_str())
        .unwrap_or("https://broker.actions.githubusercontent.com")
        .trim_end_matches('/')
        .to_string();
    debug!(broker_url = %broker_url, "broker URL from registration properties");

    let server_url = agent
        .properties
        .get("ServerUrl")
        .and_then(|v| v.value.as_str())
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| tenant_url.to_string());

    Ok(RegistrationResult {
        agent_id: agent.id,
        agent_name: agent.name,
        server_url,
        server_url_v2: broker_url,
        authorization_url: agent.authorization.authorization_url,
        client_id: agent.authorization.client_id,
    })
}

async fn github_auth(
    client: &reqwest::Client,
    target: &GitHubTarget,
    token: &str,
) -> Result<GitHubAuthResult> {
    let url = format!("{}/actions/runner-registration", target.api_base());

    let body = RunnerRegistrationRequest {
        url: target.github_url(),
        runner_event: "register".into(),
    };

    let resp = client
        .post(&url)
        .header("Authorization", format!("RemoteAuth {token}"))
        .json(&body)
        .send()
        .await
        .context("sending registration auth request")?;

    let status = resp.status();
    if !status.is_success() {
        let body_text = resp.text().await.unwrap_or_default();
        bail!("registration auth failed ({status}): {body_text}");
    }

    resp.json()
        .await
        .context("parsing registration auth response")
}

#[cfg(test)]
#[path = "registration_test.rs"]
mod registration_test;
