use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::job::schema::Step;

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

    match reference.kind.as_str() {
        "repository" => {
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

            let (owner, repo) = parse_owner_repo(&reference.name)?;

            Ok(ActionSource::Remote {
                owner,
                repo,
                git_ref: git_ref.to_string(),
                path: reference.path.clone(),
            })
        }
        "containerregistry" => {
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
            parse_action_name(&reference.name)
        }
    }
}

fn parse_owner_repo(name: &str) -> Result<(String, String)> {
    let parts: Vec<&str> = name.splitn(3, '/').collect();
    if parts.len() < 2 {
        bail!("invalid action name '{name}', expected owner/repo");
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

/// Parse "owner/repo@ref" or "owner/repo/path@ref" format.
fn parse_action_name(name: &str) -> Result<ActionSource> {
    let (name_part, git_ref) = name
        .split_once('@')
        .context(format!("action name '{name}' missing @ref"))?;

    let parts: Vec<&str> = name_part.splitn(3, '/').collect();
    if parts.len() < 2 {
        bail!("invalid action name '{name}', expected owner/repo[@ref]");
    }

    let path = if parts.len() == 3 {
        Some(parts[2].to_string())
    } else {
        None
    };

    Ok(ActionSource::Remote {
        owner: parts[0].to_string(),
        repo: parts[1].to_string(),
        git_ref: git_ref.to_string(),
        path,
    })
}

#[cfg(test)]
#[path = "resolve_test.rs"]
mod resolve_test;
