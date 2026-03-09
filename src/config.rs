use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use rsa::BigUint;
use rsa::RsaPrivateKey;
use rsa::traits::PrivateKeyParts;
use rsa::traits::PublicKeyParts;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct ChimeraConfig {
    pub daemon: Option<DaemonConfig>,
    #[serde(default)]
    pub runners: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DaemonConfig {
    #[serde(default = "default_log_format")]
    pub log_format: String,
}

fn default_log_format() -> String {
    "text".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunnerInfo {
    pub agent_id: u64,
    pub agent_name: String,
    pub pool_id: u64,
    pub server_url: String,
    pub server_url_v2: String,
    pub git_hub_url: String,
    pub work_folder: String,
    #[serde(default = "default_true")]
    pub use_v2_flow: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OAuthCredentials {
    pub scheme: String,
    pub client_id: String,
    pub authorization_url: String,
}

/// RSA private key parameters in .NET-compatible base64 format.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RsaParameters {
    pub d: String,
    pub dp: String,
    pub dq: String,
    pub exponent: String,
    #[serde(rename = "inverseQ")]
    pub inverse_q: String,
    pub modulus: String,
    pub p: String,
    pub q: String,
}

/// All credential data for a single runner, loaded from three JSON files.
#[derive(Debug, Clone)]
pub struct RunnerCredentials {
    pub info: RunnerInfo,
    pub oauth: OAuthCredentials,
    pub rsa_params: RsaParameters,
}

#[derive(Debug, Clone)]
pub struct ChimeraPaths {
    pub root: PathBuf,
}

impl ChimeraPaths {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    pub fn runners_dir(&self) -> PathBuf {
        self.root.join("runners")
    }

    pub fn runner_dir(&self, name: &str) -> PathBuf {
        self.runners_dir().join(name)
    }

    pub fn work_dir(&self) -> PathBuf {
        self.root.join("work")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    pub fn tool_cache_dir(&self) -> PathBuf {
        self.root.join("tool-cache")
    }

    pub fn actions_dir(&self) -> PathBuf {
        self.root.join("actions")
    }

    pub fn externals_dir(&self) -> PathBuf {
        self.root.join("externals")
    }
}

pub fn default_root() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".chimera"))
        .unwrap_or_else(|| PathBuf::from("/tmp/chimera"))
}

pub fn load_config(path: &Path) -> Result<ChimeraConfig> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading config from {}", path.display()))?;
    let config: ChimeraConfig =
        toml::from_str(&text).with_context(|| format!("parsing config from {}", path.display()))?;
    Ok(config)
}

pub fn save_config(path: &Path, config: &ChimeraConfig) -> Result<()> {
    let text = toml::to_string_pretty(config).context("serializing config")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating config directory {}", parent.display()))?;
    }
    std::fs::write(path, text).with_context(|| format!("writing config to {}", path.display()))?;
    Ok(())
}

pub fn load_runner_credentials(runners_dir: &Path, name: &str) -> Result<RunnerCredentials> {
    let dir = runners_dir.join(name);

    let info: RunnerInfo = load_json(&dir.join("runner.json"))?;
    let oauth: OAuthCredentials = load_json(&dir.join("credentials.json"))?;
    let rsa_params: RsaParameters = load_json(&dir.join("rsa_params.json"))?;

    Ok(RunnerCredentials {
        info,
        oauth,
        rsa_params,
    })
}

pub fn save_runner_credentials(
    runners_dir: &Path,
    name: &str,
    creds: &RunnerCredentials,
) -> Result<()> {
    let dir = runners_dir.join(name);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating runner directory {}", dir.display()))?;

    save_json(&dir.join("runner.json"), &creds.info)?;
    save_json(&dir.join("credentials.json"), &creds.oauth)?;
    save_json(&dir.join("rsa_params.json"), &creds.rsa_params)?;

    Ok(())
}

fn load_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = serde_json::to_string_pretty(value).context("serializing JSON")?;
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))
}

pub fn rsa_params_to_private_key(params: &RsaParameters) -> Result<RsaPrivateKey> {
    let n = decode_biguint(&params.modulus, "modulus")?;
    let e = decode_biguint(&params.exponent, "exponent")?;
    let d = decode_biguint(&params.d, "d")?;
    let p = decode_biguint(&params.p, "p")?;
    let q = decode_biguint(&params.q, "q")?;

    let primes = vec![p, q];
    let key = RsaPrivateKey::from_components(n, e, d, primes)
        .context("constructing RSA private key from parameters")?;

    key.validate().context("validating RSA private key")?;
    Ok(key)
}

pub fn private_key_to_rsa_params(key: &RsaPrivateKey) -> anyhow::Result<RsaParameters> {
    let primes = key.primes();

    let dp = key.dp().context("RSA key missing dp component")?;
    let dq = key.dq().context("RSA key missing dq component")?;
    let qi = key.qinv().context("RSA key missing qinv component")?;
    let qi_uint = qi.to_biguint().context("RSA key qinv is negative")?;

    Ok(RsaParameters {
        d: encode_biguint(key.d()),
        dp: encode_biguint(dp),
        dq: encode_biguint(dq),
        exponent: encode_biguint(key.e()),
        inverse_q: encode_biguint(&qi_uint),
        modulus: encode_biguint(key.n()),
        p: encode_biguint(&primes[0]),
        q: encode_biguint(&primes[1]),
    })
}

fn decode_biguint(b64: &str, field: &str) -> Result<BigUint> {
    let bytes = BASE64
        .decode(b64)
        .with_context(|| format!("decoding base64 for RSA field '{field}'"))?;
    Ok(BigUint::from_bytes_be(&bytes))
}

fn encode_biguint(n: &BigUint) -> String {
    BASE64.encode(n.to_bytes_be())
}

/// Format the RSA public key as XML (for the GitHub registration API).
pub fn public_key_to_xml(key: &RsaPrivateKey) -> String {
    let modulus = BASE64.encode(key.n().to_bytes_be());
    let exponent = BASE64.encode(key.e().to_bytes_be());
    format!(
        "<RSAKeyValue><Modulus>{modulus}</Modulus><Exponent>{exponent}</Exponent></RSAKeyValue>"
    )
}

#[cfg(test)]
#[path = "config_test.rs"]
mod config_test;
