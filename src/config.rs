use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use rsa::BigUint;
use rsa::RsaPrivateKey;
use rsa::traits::PrivateKeyParts;
use rsa::traits::PublicKeyParts;
use serde::{Deserialize, Serialize};

use crate::cache::config::CacheConfig;

#[derive(Debug, Deserialize, Serialize, Default)]
pub struct ChimeraConfig {
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub cache: CacheConfig,
    #[serde(default)]
    pub runners: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DaemonConfig {
    #[serde(default = "default_log_format")]
    pub log_format: String,
    #[serde(default = "default_shutdown_timeout")]
    pub shutdown_timeout_secs: u64,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            log_format: default_log_format(),
            shutdown_timeout_secs: default_shutdown_timeout(),
        }
    }
}

fn default_shutdown_timeout() -> u64 {
    300
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

    pub fn pid_file(&self) -> PathBuf {
        self.root.join("chimera.pid")
    }

    pub fn state_file(&self) -> PathBuf {
        self.root.join("state.json")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    pub fn cache_entries_dir(&self) -> PathBuf {
        self.cache_dir().join("entries")
    }

    pub fn cache_data_dir(&self) -> PathBuf {
        self.cache_dir().join("data")
    }

    pub fn cache_tmp_dir(&self) -> PathBuf {
        self.cache_dir().join("tmp")
    }
}

pub fn default_root() -> PathBuf {
    dirs::home_dir()
        .map(|h| h.join(".chimera"))
        .unwrap_or_else(|| PathBuf::from("/tmp/chimera"))
}

pub fn load_config(path: &Path) -> Result<ChimeraConfig> {
    if !path.exists() {
        let config = ChimeraConfig::default();
        save_config(path, &config)
            .with_context(|| format!("writing default config to {}", path.display()))?;
        return Ok(config);
    }
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

const RUNNER_INFO_FILE: &str = "runner.json";
const OAUTH_FILE: &str = "credentials.json";
const RSA_PARAMS_FILE: &str = "rsa_params.json";

/// Only `rsa_params.json` is secret: it holds the RSA private key, and anyone who can read it
/// can impersonate the runner and receive job secrets. The other files are identifiers, kept
/// owner-only too so the whole runner directory has one simple rule.
const CREDENTIAL_FILES: [&str; 3] = [RUNNER_INFO_FILE, OAUTH_FILE, RSA_PARAMS_FILE];

const PRIVATE_DIR_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

pub fn load_runner_credentials(runners_dir: &Path, name: &str) -> Result<RunnerCredentials> {
    let dir = runners_dir.join(name);
    // Hardening only: older installs loaded fine without it, so a failure (e.g. files owned
    // by another user) must not stop the runner.
    if let Err(e) = restrict_credentials_permissions(&dir) {
        tracing::warn!(error = %format!("{e:#}"), "could not restrict credential permissions");
    }

    let info: RunnerInfo = load_json(&dir.join(RUNNER_INFO_FILE))?;
    let oauth: OAuthCredentials = load_json(&dir.join(OAUTH_FILE))?;
    let rsa_params: RsaParameters = load_json(&dir.join(RSA_PARAMS_FILE))?;

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
    create_private_dir(&dir)?;
    // An existing directory keeps its mode, and older versions created it with the umask default.
    restrict_to_owner(&dir, PRIVATE_DIR_MODE)?;

    save_private_json(&dir.join(RUNNER_INFO_FILE), &creds.info)?;
    save_private_json(&dir.join(OAUTH_FILE), &creds.oauth)?;
    save_private_json(&dir.join(RSA_PARAMS_FILE), &creds.rsa_params)?;

    Ok(())
}

/// Tightens permissions of credentials written by older versions, which used the umask default.
fn restrict_credentials_permissions(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    restrict_to_owner(dir, PRIVATE_DIR_MODE)?;
    for file in CREDENTIAL_FILES {
        let path = dir.join(file);
        if path.exists() {
            restrict_to_owner(&path, PRIVATE_FILE_MODE)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE_DIR_MODE)
        .create(dir)
        .with_context(|| format!("creating runner directory {}", dir.display()))
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating runner directory {}", dir.display()))
}

/// Removes any permission bits outside `allowed`, returning whether something was too open.
#[cfg(unix)]
fn restrict_to_owner(path: &Path, allowed: u32) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    let current = std::fs::metadata(path)
        .with_context(|| format!("reading permissions of {}", path.display()))?
        .permissions()
        .mode()
        & 0o777;
    if current & !allowed == 0 {
        return Ok(false);
    }
    let restricted = current & allowed;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(restricted))
        .with_context(|| format!("restricting permissions of {}", path.display()))?;
    tracing::warn!(
        path = %path.display(),
        from = format!("{current:o}"),
        to = format!("{restricted:o}"),
        "credential permissions were too open, restricted to owner"
    );
    Ok(true)
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path, _allowed: u32) -> Result<bool> {
    Ok(false)
}

fn load_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Writes JSON readable only by the owner. The content goes to a fresh owner-only temp file
/// that is then renamed over the target, so an older, wider file never receives the secret
/// and a crash never leaves a truncated credential behind.
fn save_private_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    use std::io::Write;

    let text = serde_json::to_string_pretty(value).context("serializing JSON")?;
    let tmp = path.with_extension("json.tmp");
    remove_stale_file(&tmp)?;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(PRIVATE_FILE_MODE);
    }
    let mut file = options
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    file.write_all(text.as_bytes())
        .with_context(|| format!("writing {}", tmp.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))
}

/// A previous crash may have left a temp file, and `create_new` would refuse to reuse it.
fn remove_stale_file(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("removing stale {}", path.display()))
        }
        _ => Ok(()),
    }
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
