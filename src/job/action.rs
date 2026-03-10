pub mod composite;
pub mod docker;
pub mod download;
pub mod metadata;
pub mod node;
pub mod resolve;

use std::collections::HashMap;

use metadata::ActionMetadata;

use crate::job::expression::ExprContext;
use crate::job::schema::Step;

pub use download::ActionCache;
pub use metadata::load_action_metadata;
pub use resolve::{ActionSource, resolve_action};

/// Build INPUT_<NAME> env vars for an action step.
/// Step inputs override action.yml defaults; defaults are expression-resolved.
pub fn build_action_inputs(
    metadata: &ActionMetadata,
    step: &Step,
    expr_ctx: &ExprContext,
) -> HashMap<String, String> {
    let mut inputs = HashMap::new();
    for (name, input_def) in &metadata.inputs {
        let upper_name = format!("INPUT_{}", name.to_uppercase().replace(' ', "_"));
        if let Some(value) = step.inputs.get(name) {
            inputs.insert(upper_name, value.clone());
        } else if let Some(default) = &input_def.default {
            let resolved = crate::job::expression::resolve_expression(default, expr_ctx);
            inputs.insert(upper_name, resolved);
        }
    }
    inputs
}

/// Parse a `uses:` string from a composite action step or workflow.
/// Handles `owner/repo@ref`, `owner/repo/path@ref`, `./local`, `docker://image`.
pub fn parse_uses(uses: &str) -> anyhow::Result<ActionSource> {
    if let Some((name_part, git_ref)) = uses.split_once('@') {
        let parts: Vec<&str> = name_part.splitn(3, '/').collect();
        if parts.len() < 2 {
            anyhow::bail!("invalid uses '{uses}', expected owner/repo@ref");
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
