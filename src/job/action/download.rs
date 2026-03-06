use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tracing::debug;

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
                bail!("Docker action '{image}' not supported yet (Phase 3)")
            }
        }
    }

    async fn download_tarball(
        &self,
        owner: &str,
        repo: &str,
        git_ref: &str,
        dest: &PathBuf,
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

        std::fs::create_dir_all(dest)
            .with_context(|| format!("creating action cache dir {}", dest.display()))?;

        extract_tarball(&bytes, dest)
            .with_context(|| format!("extracting tarball for {owner}/{repo}@{git_ref}"))?;

        Ok(())
    }
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
