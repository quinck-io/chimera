use std::path::PathBuf;

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
            let (owner, repo) = (parts[0].to_string(), parts[1].to_string());

            Ok(ActionSource::Remote {
                owner,
                repo,
                git_ref: git_ref.to_string(),
                path: reference.path.clone(),
            })
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
