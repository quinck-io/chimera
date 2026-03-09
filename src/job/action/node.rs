use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::metadata::ActionMetadata;
use crate::docker::resources::JobDockerResources;
use crate::job::execute::{JobState, StepResult, build_step_env, run_process};
use crate::job::expression::ExprContext;
use crate::job::logs::LogSender;
use crate::job::schema::Step;
use crate::job::workspace::Workspace;

use super::build_action_inputs;

#[allow(clippy::too_many_arguments)]
pub async fn run_node_action(
    action_dir: &Path,
    metadata: &ActionMetadata,
    entry_point: &str,
    step: &Step,
    job_state: &mut JobState,
    workspace: &Workspace,
    base_env: &HashMap<String, String>,
    log_sender: &LogSender,
    cancel_token: &CancellationToken,
    docker_resources: Option<&JobDockerResources>,
    node_path: &Path,
) -> Result<StepResult> {
    let script_file = match entry_point {
        "pre" => metadata
            .runs
            .pre
            .as_deref()
            .context("action has no pre script")?,
        "post" => metadata
            .runs
            .post
            .as_deref()
            .context("action has no post script")?,
        _ => metadata
            .runs
            .main
            .as_deref()
            .context("action has no main script")?,
    };

    let script_path = action_dir.join(script_file);

    let mut env = build_step_env(step, job_state, workspace, base_env);

    let expr_ctx = ExprContext::new(&env, job_state, false, false);
    env.extend(build_action_inputs(metadata, step, &expr_ctx));

    // Set action-specific env vars
    if let Some(name) = &metadata.name {
        env.insert("GITHUB_ACTION".into(), name.clone());
    }

    // For post steps: inject STATE_<name> env vars from saved state
    if entry_point == "post" {
        let action_ctx = step
            .context_name
            .as_deref()
            .unwrap_or("")
            .replace("_post", "");
        if let Some(states) = job_state.action_states.get(&action_ctx) {
            for (k, v) in states {
                env.insert(format!("STATE_{}", k), v.clone());
            }
        }
    }

    let timeout = Duration::from_secs(step.timeout_in_minutes.unwrap_or(360) * 60);

    // Container mode: run via docker exec with remapped paths
    if let Some(resources) = docker_resources.filter(|r| r.job_container_id().is_some()) {
        let container_id = resources
            .job_container_id()
            .context("no job container for action step")?;

        let container_action_dir = resources
            .remap_to_container_path(action_dir)
            .context("cannot remap action dir to container path")?;
        let container_script = format!("{}/{}", container_action_dir, script_file);

        env.insert("GITHUB_ACTION_PATH".into(), container_action_dir);

        debug!(
            action_dir = %action_dir.display(),
            container_script = %container_script,
            script = script_file,
            entry_point,
            "running node action in container"
        );

        let result = crate::docker::exec::docker_exec(
            resources.docker(),
            container_id,
            vec![resources.node_path().into(), container_script],
            &env,
            "/github/workspace",
            job_state,
            log_sender,
            timeout,
            cancel_token,
        )
        .await;

        rekey_action_state(job_state, step);
        return result;
    }

    // Host mode
    env.insert(
        "GITHUB_ACTION_PATH".into(),
        action_dir.to_string_lossy().into_owned(),
    );

    debug!(
        action_dir = %action_dir.display(),
        script = script_file,
        entry_point,
        "running node action"
    );

    let script_path_str = script_path.to_string_lossy();
    let node_path_str = node_path.to_string_lossy();
    let result = run_process(
        &node_path_str,
        &[OsStr::new(script_path_str.as_ref())],
        &env,
        workspace.workspace_dir(),
        job_state,
        log_sender,
        timeout,
        cancel_token,
    )
    .await;

    rekey_action_state(job_state, step);
    result
}

/// Re-key saved state from the empty-key bucket into the correct action-keyed bucket.
fn rekey_action_state(job_state: &mut JobState, step: &Step) {
    if let Some(unnamed_state) = job_state.action_states.remove("") {
        let key = step.context_name.as_deref().unwrap_or(&step.id);
        job_state
            .action_states
            .entry(key.to_string())
            .or_default()
            .extend(unnamed_state);
    }
}

#[cfg(test)]
#[path = "node_test.rs"]
mod node_test;
