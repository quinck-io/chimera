use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::job::schema::{Step, StepReferenceKind};

#[derive(Debug, Clone)]
pub enum ActionSource {
    Remote {
        owner: String,
        repo: String,
        git_ref: String,
        path: Option<String>,
    },
    Local {
        path: PathBuf,
    },
    Docker {
        image: String,
    },
}

impl ActionSource {
    /// Every part of a remote reference becomes a segment of a path in the action
    /// cache shared by all jobs, and owner, repo and ref also go into the tarball URL,
    /// so none of them may climb out of the cache or carry URL syntax.
    pub fn remote(owner: &str, repo: &str, git_ref: &str, path: Option<&str>) -> Result<Self> {
        if !is_plain_name(owner) || !is_plain_name(repo) {
            bail!("invalid action repository '{owner}/{repo}'");
        }
        if !git_ref.split('/').all(is_plain_name) {
            bail!("invalid action ref '{git_ref}'");
        }
        if let Some(path) = path
            && !is_relative_subpath(path)
        {
            bail!("invalid action path '{path}'");
        }

        Ok(Self::Remote {
            owner: owner.to_string(),
            repo: repo.to_string(),
            git_ref: git_ref.to_string(),
            path: path.map(str::to_string),
        })
    }
}

fn is_plain_name(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
}

fn is_relative_subpath(path: &str) -> bool {
    Path::new(path)
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

pub fn resolve_action(step: &Step) -> Result<ActionSource> {
    let reference = &step.reference;

    match &reference.kind {
        StepReferenceKind::Repository => {
            if reference.repository_type.as_deref() == Some("self") {
                let path = reference
                    .path
                    .as_deref()
                    .context("local action reference missing path")?;
                return Ok(ActionSource::Local {
                    path: PathBuf::from(path),
                });
            }

            let git_ref = reference
                .git_ref
                .as_deref()
                .context("remote action reference missing ref")?;

            let parts: Vec<&str> = reference.name.splitn(3, '/').collect();
            if parts.len() < 2 {
                bail!(
                    "invalid action name '{}', expected owner/repo",
                    reference.name
                );
            }
            ActionSource::remote(parts[0], parts[1], git_ref, reference.path.as_deref())
        }
        StepReferenceKind::ContainerRegistry => {
            let image = reference
                .image
                .as_deref()
                .context("container action reference missing image")?;
            Ok(ActionSource::Docker {
                image: image.to_string(),
            })
        }
        _ => {
            // Fallback: parse name as "owner/repo@ref" (or "owner/repo/path@ref")
            super::parse_uses(&reference.name)
        }
    }
}

#[cfg(test)]
#[path = "resolve_test.rs"]
mod resolve_test;
