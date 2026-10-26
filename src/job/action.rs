pub mod composite;
pub mod docker;
pub mod download;
pub mod metadata;
pub mod node;
pub mod resolve;

use std::collections::HashMap;

use anyhow::Context;
use metadata::ActionMetadata;

use crate::job::expression::ExprContext;
use crate::job::schema::Step;

pub use download::ActionCache;
pub use metadata::load_action_metadata;
pub use resolve::{ActionSource, resolve_action};

/// Which of an action's entry points a step runs, mirroring the official runner's
/// `ActionRunStage`. The stage is fixed when the step is created and never inferred
/// from the step's name, since a user may well name a step `cache_post`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionStage {
    Pre,
    Main,
    Post,
}

impl ActionStage {
    pub fn entry_point(self) -> &'static str {
        match self {
            Self::Pre => "pre",
            Self::Main => "main",
            Self::Post => "post",
        }
    }

    pub fn display_name(self, main_display_name: &str) -> String {
        match self {
            Self::Pre => format!("Pre {main_display_name}"),
            Self::Main => main_display_name.to_string(),
            Self::Post => format!("Post {main_display_name}"),
        }
    }
}

/// Build INPUT_<NAME> env vars for an action step.
///
/// `step.inputs` is the step's `with:` already evaluated, once, when the step
/// started, so it is taken verbatim: evaluating it again would run any `${{ }}`
/// that a value merely contains (a PR title, a step output), letting it read
/// `secrets`. Only the action.yml defaults, which nothing has evaluated yet, are
/// evaluated here.
pub fn build_action_inputs(
    metadata: &ActionMetadata,
    step: &Step,
    expr_ctx: &ExprContext,
) -> anyhow::Result<HashMap<String, String>> {
    let mut inputs = HashMap::new();
    for (name, input_def) in &metadata.inputs {
        let upper_name = format!("INPUT_{}", name.to_uppercase().replace(' ', "_"));
        if let Some(value) = step.inputs.get(name) {
            inputs.insert(upper_name, value.clone());
        } else if let Some(default) = &input_def.default {
            let resolved = crate::job::expression::resolve_template(default, expr_ctx)
                .with_context(|| format!("evaluating the default of input '{name}'"))?;
            inputs.insert(upper_name, resolved);
        }
    }
    Ok(inputs)
}

/// The warning the official runner logs when a local action declares a `pre`: a
/// local action is only on disk once a step checks it out, too late for its `pre`
/// step, so that is never run.
pub fn local_pre_warning(source: &ActionSource, metadata: &ActionMetadata) -> Option<String> {
    let ActionSource::Local { path } = source else {
        return None;
    };
    metadata.runs.has_pre().then(|| {
        format!(
            "##[warning]`pre` execution is not supported for local action from '{}'",
            path.display()
        )
    })
}

/// Parse a `uses:` string from a composite action step or workflow.
/// Handles `owner/repo@ref`, `owner/repo/path@ref`, `./local`, `docker://image`.
pub fn parse_uses(uses: &str) -> anyhow::Result<ActionSource> {
    if let Some((name_part, git_ref)) = uses.split_once('@') {
        let parts: Vec<&str> = name_part.splitn(3, '/').collect();
        if parts.len() < 2 {
            anyhow::bail!("invalid uses '{uses}', expected owner/repo@ref");
        }
        ActionSource::remote(parts[0], parts[1], git_ref, parts.get(2).copied())
    } else if uses.starts_with("./") {
        Ok(ActionSource::Local { path: uses.into() })
    } else if uses.starts_with("docker://") {
        Ok(ActionSource::Docker {
            image: uses.strip_prefix("docker://").unwrap().to_string(),
        })
    } else {
        anyhow::bail!("cannot parse uses '{uses}'")
    }
}

#[cfg(test)]
#[path = "action_test.rs"]
mod action_test;
