use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::download::ActionCache;
use super::metadata::ActionMetadata;
use crate::docker::resources::JobDockerResources;
use crate::job::execute::{JobState, StepConclusion, StepResult, build_step_env, run_process};
use crate::job::expression::ExprContext;
use crate::job::logs::LogSender;
use crate::job::schema::Step;
use crate::job::workspace::Workspace;

use super::build_action_inputs;

const MAX_COMPOSITE_DEPTH: u32 = 10;

fn ykey(s: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(s.into())
}

#[allow(clippy::too_many_arguments)]
pub fn run_composite_action<'a>(
    action_dir: &'a Path,
    metadata: &'a ActionMetadata,
    step: &'a Step,
    job_state: &'a mut JobState,
    workspace: &'a Workspace,
    base_env: &'a HashMap<String, String>,
    log_sender: &'a LogSender,
    action_cache: &'a ActionCache,
    access_token: &'a str,
    depth: u32,
    cancel_token: &'a CancellationToken,
    docker_resources: Option<&'a JobDockerResources>,
    node_path: &'a Path,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<StepResult>> + Send + 'a>> {
    Box::pin(run_composite_action_inner(
        action_dir,
        metadata,
        step,
        job_state,
        workspace,
        base_env,
        log_sender,
        action_cache,
        access_token,
        depth,
        cancel_token,
        docker_resources,
        node_path,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn run_composite_action_inner(
    action_dir: &Path,
    metadata: &ActionMetadata,
    step: &Step,
    job_state: &mut JobState,
    workspace: &Workspace,
    base_env: &HashMap<String, String>,
    log_sender: &LogSender,
    action_cache: &ActionCache,
    access_token: &str,
    depth: u32,
    cancel_token: &CancellationToken,
    docker_resources: Option<&JobDockerResources>,
    node_path: &Path,
) -> Result<StepResult> {
    if depth >= MAX_COMPOSITE_DEPTH {
        bail!("composite action recursion depth limit ({MAX_COMPOSITE_DEPTH}) exceeded");
    }

    let steps = metadata
        .runs
        .steps
        .as_ref()
        .context("composite action has no steps")?;

    // Build action inputs once — they stay constant across sub-steps
    let initial_env = build_step_env(step, job_state, workspace, base_env);
    let expr_ctx = ExprContext::new(&initial_env, job_state, false, false);
    let action_inputs = build_action_inputs(metadata, step, &expr_ctx);

    let timeout = Duration::from_secs(step.timeout_in_minutes.unwrap_or(360) * 60);

    for (i, nested_step) in steps.iter().enumerate() {
        let nested_obj = nested_step
            .as_mapping()
            .context("composite step is not a mapping")?;

        // Rebuild env each iteration so PATH/env changes from previous sub-steps
        // (via ::add-path::, GITHUB_PATH, ::set-env::, GITHUB_ENV) are picked up.
        let mut composite_env = build_step_env(step, job_state, workspace, base_env);
        composite_env.extend(action_inputs.clone());

        // Evaluate `if:` condition — skip the step if it evaluates to false
        if let Some(condition) = nested_obj.get(ykey("if")).and_then(|v| v.as_str()) {
            let cond_ctx = ExprContext::new(&composite_env, job_state, false, false);
            if !crate::job::expression::evaluate_condition(Some(condition), &cond_ctx) {
                debug!(
                    composite_step = i,
                    condition, "skipping composite sub-step (condition not met)"
                );
                continue;
            }
        }

        debug!(
            composite_step = i,
            action_dir = %action_dir.display(),
            "running composite sub-step"
        );

        let result = if nested_obj.contains_key(ykey("uses")) {
            run_nested_action(
                nested_obj,
                job_state,
                workspace,
                &composite_env,
                log_sender,
                action_cache,
                access_token,
                timeout,
                depth,
                cancel_token,
                docker_resources,
                node_path,
            )
            .await?
        } else if nested_obj.contains_key(ykey("run")) {
            run_nested_script(
                nested_obj,
                job_state,
                workspace,
                &composite_env,
                log_sender,
                timeout,
                cancel_token,
                docker_resources,
            )
            .await?
        } else {
            log_sender
                .send(format!(
                    "Skipping composite step {i}: no 'run' or 'uses' key"
                ))
                .await;
            continue;
        };

        if result.conclusion == StepConclusion::Cancelled {
            return Ok(result);
        }

        if result.conclusion == StepConclusion::Failed {
            let continue_on_error = nested_obj
                .get(ykey("continue-on-error"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if !continue_on_error {
                return Ok(result);
            }
        }
    }

    Ok(StepResult {
        conclusion: StepConclusion::Succeeded,
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_nested_script(
    step_map: &serde_yaml::Mapping,
    job_state: &mut JobState,
    workspace: &Workspace,
    env: &HashMap<String, String>,
    log_sender: &LogSender,
    timeout: Duration,
    cancel_token: &CancellationToken,
    docker_resources: Option<&JobDockerResources>,
) -> Result<StepResult> {
    let script = step_map
        .get(ykey("run"))
        .and_then(|v| v.as_str())
        .context("composite step 'run' is not a string")?;

    let shell = step_map
        .get(ykey("shell"))
        .and_then(|v| v.as_str())
        .unwrap_or("bash");

    let mut step_env = env.clone();
    if let Some(serde_yaml::Value::Mapping(env_map)) = step_map.get(ykey("env")) {
        for (k, v) in env_map {
            if let (Some(key), Some(val)) = (k.as_str(), v.as_str()) {
                let env_ctx = ExprContext::new(&step_env, job_state, false, false);
                let resolved_val = crate::job::expression::resolve_template(val, &env_ctx);
                step_env.insert(key.to_string(), resolved_val);
            }
        }
    }

    // Resolve ${{ }} expressions in the script body before writing
    let script_ctx = ExprContext::new(&step_env, job_state, false, false);
    let resolved_script = crate::job::expression::resolve_template(script, &script_ctx);

    // Resolve working-directory (may contain expressions like ${{ inputs.dir || '.' }})
    let raw_workdir = step_map
        .get(ykey("working-directory"))
        .and_then(|v| v.as_str())
        .map(|d| crate::job::expression::resolve_template(d, &script_ctx));

    // Container mode: run inline via docker exec
    if let Some(resources) = docker_resources.filter(|r| r.job_container_id().is_some()) {
        let container_id = resources
            .job_container_id()
            .context("no job container for composite script step")?;

        let working_dir = match &raw_workdir {
            Some(d) if d != "." => format!("/github/workspace/{d}"),
            _ => "/github/workspace".into(),
        };

        return crate::docker::exec::docker_exec(
            resources.docker(),
            container_id,
            vec![shell.into(), "-e".into(), "-c".into(), resolved_script],
            &step_env,
            &working_dir,
            job_state,
            log_sender,
            timeout,
            cancel_token,
        )
        .await;
    }

    // Host mode
    let working_dir = match &raw_workdir {
        Some(d) if d != "." => workspace.workspace_dir().join(d),
        _ => workspace.workspace_dir().to_path_buf(),
    };

    let script_file = workspace
        .runner_temp()
        .join(format!("_composite_step_{}.sh", uuid::Uuid::new_v4()));
    std::fs::write(&script_file, &resolved_script)
        .with_context(|| format!("writing composite script to {}", script_file.display()))?;

    let result = run_process(
        shell,
        &[OsStr::new("-e"), script_file.as_os_str()],
        &step_env,
        &working_dir,
        job_state,
        log_sender,
        timeout,
        cancel_token,
    )
    .await;

    let _ = std::fs::remove_file(&script_file);
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_nested_action(
    step_map: &serde_yaml::Mapping,
    job_state: &mut JobState,
    workspace: &Workspace,
    env: &HashMap<String, String>,
    log_sender: &LogSender,
    action_cache: &ActionCache,
    access_token: &str,
    timeout: Duration,
    depth: u32,
    cancel_token: &CancellationToken,
    docker_resources: Option<&JobDockerResources>,
    node_path: &Path,
) -> Result<StepResult> {
    let uses = step_map
        .get(ykey("uses"))
        .and_then(|v| v.as_str())
        .context("composite step 'uses' is not a string")?;

    let source = super::parse_uses(uses)?;

    let mut inputs = HashMap::new();
    if let Some(serde_yaml::Value::Mapping(with_map)) = step_map.get(ykey("with")) {
        for (k, v) in with_map {
            if let (Some(key), Some(val)) = (k.as_str(), v.as_str()) {
                inputs.insert(key.to_string(), val.to_string());
            }
        }
    }

    let nested_step = Step {
        id: format!("composite_{}", uuid::Uuid::new_v4()),
        display_name: uses.to_string(),
        reference: crate::job::schema::StepReference {
            name: uses.to_string(),
            kind: crate::job::schema::StepReferenceKind::Repository,
            ..Default::default()
        },
        inputs,
        condition: None,
        timeout_in_minutes: Some(timeout.as_secs() / 60),
        continue_on_error: false,
        order: 0,
        environment: None,
        context_name: None,
    };

    // Handle inline docker://image in composite steps — skip get_action/metadata
    if let super::resolve::ActionSource::Docker { ref image } = source {
        return super::docker::run_docker_image_action(
            image,
            &nested_step,
            job_state,
            workspace,
            env,
            log_sender,
            cancel_token,
            docker_resources,
        )
        .await;
    }

    let action_dir = action_cache
        .get_action(&source, workspace.workspace_dir(), access_token)
        .await?;
    let metadata = super::metadata::load_action_metadata(&action_dir)?;

    if metadata.runs.is_node() {
        super::node::run_node_action(
            &action_dir,
            &metadata,
            "main",
            &nested_step,
            job_state,
            workspace,
            env,
            log_sender,
            cancel_token,
            docker_resources,
            node_path,
        )
        .await
    } else if metadata.runs.is_composite() {
        run_composite_action(
            &action_dir,
            &metadata,
            &nested_step,
            job_state,
            workspace,
            env,
            log_sender,
            action_cache,
            access_token,
            depth + 1,
            cancel_token,
            docker_resources,
            node_path,
        )
        .await
    } else if metadata.runs.is_docker() {
        super::docker::run_docker_metadata_action(
            &action_dir,
            &metadata,
            "main",
            &nested_step,
            job_state,
            workspace,
            env,
            log_sender,
            cancel_token,
            docker_resources,
        )
        .await
    } else {
        bail!(
            "unsupported action runtime in composite: {}",
            metadata.runs.using
        )
    }
}

#[cfg(test)]
#[path = "composite_test.rs"]
mod composite_test;
