use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use tracing::{debug, warn};

use super::resolve::ActionSource;

const GITHUB_API_URL: &str = "https://api.github.com";

/// Downloaded actions, shared by every job on the machine.
///
/// Entries are stored by commit rather than by ref. A moving ref like `v4` or `main`
/// is resolved again for each job, so a job always runs the commit the ref points
/// to now, and the cache never pins an action to its first download.
pub struct ActionCache {
    cache_dir: PathBuf,
    client: reqwest::Client,
    api_url: String,
    /// Refs resolved during this job, so an action's pre, main and post steps all
    /// run the same commit even if its ref moves while the job is running.
    resolved: Mutex<HashMap<String, String>>,
}

impl ActionCache {
    pub fn new(cache_dir: PathBuf, client: reqwest::Client) -> Self {
        Self::with_api_url(cache_dir, client, GITHUB_API_URL.into())
    }

    fn with_api_url(cache_dir: PathBuf, client: reqwest::Client, api_url: String) -> Self {
        Self {
            cache_dir,
            client,
            api_url,
            resolved: Mutex::new(HashMap::new()),
        }
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
                let commit = self
                    .resolve_commit(owner, repo, git_ref, access_token)
                    .await?;
                let cache_path = self.cache_dir.join(owner).join(repo).join(&commit);

                if !cache_path.exists() {
                    self.download_tarball(owner, repo, &commit, &cache_path, access_token)
                        .await?;
                } else {
                    debug!(owner, repo, git_ref, commit, "action cache hit");
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

    /// The commit `git_ref` points to right now. A full commit SHA is used as is.
    async fn resolve_commit(
        &self,
        owner: &str,
        repo: &str,
        git_ref: &str,
        access_token: &str,
    ) -> Result<String> {
        if is_commit_sha(git_ref) {
            return Ok(git_ref.to_ascii_lowercase());
        }

        let action = format!("{owner}/{repo}@{git_ref}");
        if let Some(commit) = self.lock_resolved().get(&action) {
            return Ok(commit.clone());
        }

        let url = format!("{}/repos/{owner}/{repo}/commits/{git_ref}", self.api_url);
        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("token {access_token}"))
            .header("Accept", "application/vnd.github.sha")
            .header("User-Agent", "chimera")
            .send()
            .await
            .with_context(|| format!("resolving {action}"))?;

        if !response.status().is_success() {
            bail!("failed to resolve {action}: HTTP {}", response.status());
        }

        let body = response
            .text()
            .await
            .with_context(|| format!("reading the commit {action} resolves to"))?;
        let commit = body.trim();
        if !is_commit_sha(commit) {
            bail!("resolving {action} returned '{commit}' instead of a commit SHA");
        }
        let commit = commit.to_ascii_lowercase();

        debug!(%action, %commit, "resolved action ref");
        self.lock_resolved().insert(action, commit.clone());
        Ok(commit)
    }

    fn lock_resolved(&self) -> std::sync::MutexGuard<'_, HashMap<String, String>> {
        self.resolved
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    async fn download_tarball(
        &self,
        owner: &str,
        repo: &str,
        git_ref: &str,
        dest: &Path,
        access_token: &str,
    ) -> Result<()> {
        let url = format!("{}/repos/{owner}/{repo}/tarball/{git_ref}", self.api_url);
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

fn is_commit_sha(git_ref: &str) -> bool {
    git_ref.len() == 40 && git_ref.chars().all(|c| c.is_ascii_hexdigit())
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
            entry
                .unpack(&target)
                .with_context(|| format!("writing {}", target.display()))?;
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "download_test.rs"]
mod download_test;
