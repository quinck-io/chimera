use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

const NODE_VERSION: &str = "v20.18.3";

/// Ensure a node binary is available for the current host platform.
/// Returns the path to the node binary (e.g., `externals/node20-darwin-arm64/bin/node`).
pub async fn ensure_node(externals_dir: &Path) -> Result<PathBuf> {
    ensure_node_for_platform(externals_dir, std::env::consts::OS, std::env::consts::ARCH).await
}

/// Ensure a Linux node binary is available (for container execution).
/// Returns the path to the node binary.
pub async fn ensure_linux_node(externals_dir: &Path) -> Result<PathBuf> {
    ensure_node_for_platform(externals_dir, "linux", std::env::consts::ARCH).await
}

async fn ensure_node_for_platform(externals_dir: &Path, os: &str, arch: &str) -> Result<PathBuf> {
    let node_arch = match arch {
        "x86_64" | "x86" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    let node_os = match os {
        "macos" => "darwin",
        other => other,
    };

    let dir_name = format!("node20-{node_os}-{node_arch}");
    let node_dir = externals_dir.join(&dir_name);
    let node_bin = node_dir.join("bin").join("node");

    if node_bin.exists() {
        debug!(path = %node_bin.display(), "node binary already cached");
        return Ok(node_bin);
    }

    let url = format!(
        "https://nodejs.org/dist/{NODE_VERSION}/node-{NODE_VERSION}-{node_os}-{node_arch}.tar.gz"
    );

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
