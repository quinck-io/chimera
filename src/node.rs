use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

/// The Node runtimes an action can ask for through `runs.using: node<major>`.
///
/// Shipping only one is not enough: a `node24` action running on Node 20 mostly
/// works, right up until it touches something the older runtime lacks — and then
/// it fails somewhere deep inside the action, as `pnpm/action-setup` does when its
/// self-installer re-execs `process.execPath`.
const NODE_VERSIONS: &[(&str, &str)] = &[("20", "v20.18.3"), ("24", "v24.19.0")];

/// Used for actions that declare a runtime we do not ship.
pub(crate) const DEFAULT_MAJOR: &str = "20";

/// The Node binaries available to node actions, keyed by major version.
#[derive(Debug, Clone)]
pub struct NodeRuntimes {
    by_major: BTreeMap<String, PathBuf>,
    default_path: PathBuf,
}

impl NodeRuntimes {
    /// A set backed by one binary, where every declared runtime resolves to it.
    /// For callers that already have a Node they trust — tests, and hosts wanting
    /// to pin their own install rather than let the runner download one.
    pub fn single(path: PathBuf) -> Self {
        Self {
            by_major: NODE_VERSIONS
                .iter()
                .map(|(major, _)| ((*major).to_string(), path.clone()))
                .collect(),
            default_path: path,
        }
    }

    /// The binary for the runtime an action declared.
    ///
    /// Anything we do not have falls back to the default rather than failing the
    /// step — node12 and node16 actions are long past EOL and the official runner
    /// runs them on a current runtime too.
    pub fn resolve(&self, major: Option<&str>) -> &Path {
        let Some(major) = major else {
            return &self.default_path;
        };
        match self.by_major.get(major) {
            Some(path) => path,
            None => {
                warn!(
                    requested = major,
                    using = DEFAULT_MAJOR,
                    "action wants a Node runtime this runner does not ship"
                );
                &self.default_path
            }
        }
    }

    /// The same set of runtimes as seen from somewhere else in the filesystem —
    /// a container that has the externals directory bind-mounted at `root`.
    pub fn remapped(&self, root: &str) -> BTreeMap<String, String> {
        self.by_major
            .iter()
            .filter_map(|(major, path)| {
                let dir = path.parent()?.parent()?.file_name()?.to_str()?;
                Some((major.clone(), format!("{root}/{dir}/bin/node")))
            })
            .collect()
    }
}

/// Ensure every supported node binary is available for the current host platform.
pub async fn ensure_node(externals_dir: &Path) -> Result<NodeRuntimes> {
    ensure_node_for_platform(externals_dir, std::env::consts::OS, std::env::consts::ARCH).await
}

/// Ensure every supported node binary is available for Linux (container execution).
pub async fn ensure_linux_node(externals_dir: &Path) -> Result<NodeRuntimes> {
    ensure_node_for_platform(externals_dir, "linux", std::env::consts::ARCH).await
}

async fn ensure_node_for_platform(
    externals_dir: &Path,
    os: &str,
    arch: &str,
) -> Result<NodeRuntimes> {
    let mut by_major = BTreeMap::new();

    for (major, version) in NODE_VERSIONS {
        match ensure_one(externals_dir, os, arch, major, version).await {
            Ok(path) => {
                by_major.insert((*major).to_string(), path);
            }
            // A runtime we cannot fetch is only fatal if it is the one we fall back
            // to; otherwise the actions that want it degrade to the default.
            Err(e) if *major != DEFAULT_MAJOR => {
                warn!(major, error = %e, "could not provision Node runtime");
            }
            Err(e) => return Err(e),
        }
    }

    let default_path = by_major
        .get(DEFAULT_MAJOR)
        .cloned()
        .with_context(|| format!("no Node {DEFAULT_MAJOR} runtime available"))?;

    Ok(NodeRuntimes {
        by_major,
        default_path,
    })
}

async fn ensure_one(
    externals_dir: &Path,
    os: &str,
    arch: &str,
    major: &str,
    version: &str,
) -> Result<PathBuf> {
    let node_arch = match arch {
        "x86_64" | "x86" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    let node_os = match os {
        "macos" => "darwin",
        other => other,
    };

    let dir_name = format!("node{major}-{node_os}-{node_arch}");
    let node_dir = externals_dir.join(&dir_name);
    let node_bin = node_dir.join("bin").join("node");

    if node_bin.exists() {
        debug!(path = %node_bin.display(), "node binary already cached");
        return Ok(node_bin);
    }

    let url =
        format!("https://nodejs.org/dist/{version}/node-{version}-{node_os}-{node_arch}.tar.gz");

    info!(url = %url, dest = %node_dir.display(), "downloading node binary");

    let response = reqwest::get(&url)
        .await
        .with_context(|| format!("downloading node from {url}"))?;
    if !response.status().is_success() {
        anyhow::bail!("failed to download node: HTTP {}", response.status());
    }
    let bytes = response.bytes().await.context("reading node tarball")?;

    // Extract to a temp directory, then atomically rename to avoid TOCTOU races.
    // All filesystem I/O runs on the blocking threadpool to avoid starving the runtime.
    let tmp_name = format!("{dir_name}.tmp-{}", uuid::Uuid::new_v4());
    let tmp_dir = externals_dir.join(&tmp_name);
    let node_dir_owned = node_dir.clone();
    let node_bin_display = node_bin.display().to_string();

    tokio::task::spawn_blocking(move || {
        std::fs::create_dir_all(&tmp_dir)
            .with_context(|| format!("creating temp node dir {}", tmp_dir.display()))?;

        let decoder = flate2::read::GzDecoder::new(bytes.as_ref());
        let mut archive = tar::Archive::new(decoder);

        for entry in archive.entries().context("reading node tarball entries")? {
            let mut entry = entry.context("reading node tarball entry")?;
            let entry_path = entry.path().context("reading entry path")?.into_owned();

            let stripped: PathBuf = entry_path.components().skip(1).collect();
            if stripped.as_os_str().is_empty() {
                continue;
            }

            if stripped != Path::new("bin/node") {
                continue;
            }

            if stripped
                .components()
                .any(|c| matches!(c, Component::ParentDir))
            {
                warn!(path = %entry_path.display(), "skipping tarball entry with path traversal");
                continue;
            }

            let target = tmp_dir.join(&stripped);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut file = std::fs::File::create(&target)
                .with_context(|| format!("creating {}", target.display()))?;
            std::io::copy(&mut entry, &mut file)
                .with_context(|| format!("writing {}", target.display()))?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755))?;
            }
        }

        if !tmp_dir.join("bin/node").exists() {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            anyhow::bail!("node binary not found after extraction at {node_bin_display}");
        }

        match std::fs::rename(&tmp_dir, &node_dir_owned) {
            Ok(()) => {}
            Err(e) if node_dir_owned.exists() => {
                debug!(error = %e, "node dir already exists (concurrent download), using existing");
                let _ = std::fs::remove_dir_all(&tmp_dir);
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&tmp_dir);
                return Err(e).context("renaming temp node dir to final location");
            }
        }

        Ok(())
    })
    .await
    .context("node extraction task panicked")??;

    info!(path = %node_bin.display(), "node binary downloaded and cached");
    Ok(node_bin)
}

#[cfg(test)]
#[path = "node_test.rs"]
mod node_test;
