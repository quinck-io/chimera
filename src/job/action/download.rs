use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use tracing::{debug, warn};

use super::resolve::ActionSource;

pub struct ActionCache {
    cache_dir: PathBuf,
    client: reqwest::Client,
}

impl ActionCache {
    pub fn new(cache_dir: PathBuf, client: reqwest::Client) -> Self {
        Self { cache_dir, client }
    }

    pub async fn get_action(
        &self,
        source: &ActionSource,
        workspace_dir: &Path,
        access_token: &str,
    ) -> Result<PathBuf> {
        match source {
            ActionSource::Remote {
                owner,
                repo,
                git_ref,
                path,
            } => {
                let cache_path = self.cache_dir.join(owner).join(repo).join(git_ref);

                if !cache_path.exists() {
                    self.download_tarball(owner, repo, git_ref, &cache_path, access_token)
                        .await?;
                } else {
                    debug!(owner, repo, git_ref, "action cache hit");
                }

                if let Some(subpath) = path {
                    Ok(cache_path.join(subpath))
                } else {
                    Ok(cache_path)
                }
            }
            ActionSource::Local { path } => Ok(workspace_dir.join(path)),
            ActionSource::Docker { image } => {
                bail!("Docker action '{image}' should be handled before get_action is called")
            }
        }
    }

    async fn download_tarball(
        &self,
        owner: &str,
        repo: &str,
        git_ref: &str,
        dest: &Path,
        access_token: &str,
    ) -> Result<()> {
        let url = format!("https://api.github.com/repos/{owner}/{repo}/tarball/{git_ref}");
        debug!(%url, "downloading action tarball");

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("token {access_token}"))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "chimera")
            .send()
            .await
            .with_context(|| format!("requesting tarball for {owner}/{repo}@{git_ref}"))?;

        if !response.status().is_success() {
            bail!(
                "failed to download {owner}/{repo}@{git_ref}: HTTP {}",
                response.status()
            );
        }

        let bytes = response
            .bytes()
            .await
            .context("reading tarball response body")?;

        // Extract to a temp directory, then atomically rename to avoid TOCTOU races.
        // All filesystem I/O runs on the blocking threadpool to avoid starving the runtime.
        let tmp_name = format!(
            "{}.tmp-{}",
            dest.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("action"),
            uuid::Uuid::new_v4()
        );
        let tmp_dir = dest.parent().context("dest has no parent")?.join(&tmp_name);
        let dest = dest.to_path_buf();
        let owner = owner.to_string();
        let repo = repo.to_string();
        let git_ref = git_ref.to_string();

        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&tmp_dir)
                .with_context(|| format!("creating temp action dir {}", tmp_dir.display()))?;

            extract_tarball(&bytes, &tmp_dir)
                .with_context(|| format!("extracting tarball for {owner}/{repo}@{git_ref}"))?;

            match std::fs::rename(&tmp_dir, &dest) {
                Ok(()) => {}
                Err(e) if dest.exists() => {
                    debug!(error = %e, "action cache dir already exists (concurrent download), using existing");
                    let _ = std::fs::remove_dir_all(&tmp_dir);
                }
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&tmp_dir);
                    return Err(e).context("renaming temp action dir to final location");
                }
            }

            Ok(())
        })
        .await
        .context("extract task panicked")?
    }
}

/// Returns true if the path contains `..` components that could escape the destination.
fn has_path_traversal(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::ParentDir))
}

fn extract_tarball(data: &[u8], dest: &Path) -> Result<()> {
    let decoder = flate2::read::GzDecoder::new(data);
    let mut archive = tar::Archive::new(decoder);

    for entry in archive.entries().context("reading tarball entries")? {
        let mut entry = entry.context("reading tarball entry")?;
        let entry_path = entry.path().context("reading entry path")?.into_owned();

        // Strip the first path component (GitHub adds a prefix like "owner-repo-sha/")
        let stripped: PathBuf = entry_path.components().skip(1).collect();
        if stripped.as_os_str().is_empty() {
            continue;
        }

        if has_path_traversal(&stripped) {
            warn!(path = %entry_path.display(), "skipping tarball entry with path traversal");
            continue;
        }

        let target = dest.join(&stripped);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }

        if entry.header().entry_type().is_dir() {
            std::fs::create_dir_all(&target)?;
        } else {
            let mut file = std::fs::File::create(&target)
                .with_context(|| format!("creating {}", target.display()))?;
            std::io::copy(&mut entry, &mut file)
                .with_context(|| format!("writing {}", target.display()))?;
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "download_test.rs"]
mod download_test;
