use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bollard::Docker;
use bollard::container::{Config, CreateContainerOptions, LogsOptions};
use bollard::models::{EndpointSettings, HostConfig};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::build_action_inputs;
use super::metadata::ActionMetadata;
use crate::docker::exec::process_output_line;
use crate::docker::resources::{JobDockerResources, stop_and_remove};
use crate::job::execute::{JobState, StepConclusion, StepResult, build_step_env};
use crate::job::expression::ExprContext;
use crate::job::logs::LogSender;
use crate::job::schema::Step;
use crate::job::workspace::Workspace;

/// Case 1: Inline `docker://image` — no action.yml, image/entrypoint/args from step inputs.
#[allow(clippy::too_many_arguments)]
pub async fn run_docker_image_action(
    image: &str,
    step: &Step,
    job_state: &mut JobState,
    workspace: &Workspace,
    base_env: &HashMap<String, String>,
    log_sender: &LogSender,
    cancel_token: &CancellationToken,
    docker_resources: Option<&JobDockerResources>,
) -> Result<StepResult> {
    let env = build_step_env(step, job_state, workspace, base_env);
    let expr_ctx = ExprContext::new(&env, job_state, false, false);

    let entrypoint = step.inputs.get("entrypoint").cloned();
    let args = step
        .inputs
        .get("args")
        .map(|a| {
            let resolved = crate::job::expression::resolve_expression(a, &expr_ctx);
            split_shell_args(&resolved)
        })
        .unwrap_or_default();

    debug!(image, ?entrypoint, ?args, "running inline docker action");

    let result = run_docker_container(RunDockerParams {
        image,
        entrypoint: entrypoint.as_deref(),
        args: &args,
        env: &env,
        step,
        job_state,
        workspace,
        log_sender,
        cancel_token,
        docker_resources,
        action_dir: None,
    })
    .await;

    rekey_action_state(job_state, step);
    result
}

/// Case 2: Repo action with `runs.using: docker` — has action.yml with Docker fields.
#[allow(clippy::too_many_arguments)]
pub async fn run_docker_metadata_action(
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
) -> Result<StepResult> {
    let image = resolve_image(metadata)?;
    let (entrypoint, args) = match resolve_entry_point(metadata, entry_point) {
        Some(pair) => pair,
        None => {
            return Ok(StepResult {
                conclusion: StepConclusion::Succeeded,
            });
        }
    };

    let mut env = build_step_env(step, job_state, workspace, base_env);
    let expr_ctx = ExprContext::new(&env, job_state, false, false);
    env.extend(build_action_inputs(metadata, step, &expr_ctx));
    merge_action_env(&mut env, metadata, job_state);
    inject_post_state(&mut env, entry_point, step, job_state);

    let resolved_args = resolve_args(&args, &env, job_state);

    debug!(
        image,
        entry_point,
        ?entrypoint,
        ?resolved_args,
        "running docker metadata action"
    );

    let result = run_docker_container(RunDockerParams {
        image,
        entrypoint: entrypoint.as_deref(),
        args: &resolved_args,
        env: &env,
        step,
        job_state,
        workspace,
        log_sender,
        cancel_token,
        docker_resources,
        action_dir: Some(action_dir),
    })
    .await;

    rekey_action_state(job_state, step);
    result
}

// ── Metadata helpers ────────────────────────────────────────────

fn resolve_image(metadata: &ActionMetadata) -> Result<&str> {
    let raw = metadata
        .runs
        .image
        .as_deref()
        .context("docker action has no image field")?;

    if raw == "Dockerfile" || raw.ends_with("/Dockerfile") {
        bail!("building from Dockerfile is not supported, only pre-built images are allowed");
    }

    Ok(raw.strip_prefix("docker://").unwrap_or(raw))
}

/// Route entry_point ("pre"/"main"/"post") to the matching entrypoint + args.
/// Returns `None` when a pre/post entrypoint is absent (caller should skip).
fn resolve_entry_point(
    metadata: &ActionMetadata,
    entry_point: &str,
) -> Option<(Option<String>, Vec<String>)> {
    match entry_point {
        "pre" => metadata
            .runs
            .pre_entrypoint
            .as_ref()
            .map(|ep| (Some(ep.clone()), Vec::new())),
        "post" => metadata
            .runs
            .post_entrypoint
            .as_ref()
            .map(|ep| (Some(ep.clone()), Vec::new())),
        _ => Some((
            metadata.runs.entrypoint.clone(),
            metadata.runs.args.clone().unwrap_or_default(),
        )),
    }
}

fn merge_action_env(
    env: &mut HashMap<String, String>,
    metadata: &ActionMetadata,
    job_state: &JobState,
) {
    if let Some(action_env) = &metadata.runs.env {
        for (k, v) in action_env {
            let ctx = ExprContext::new(env, job_state, false, false);
            let resolved = crate::job::expression::resolve_expression(v, &ctx);
            env.insert(k.clone(), resolved);
        }
    }
}

fn inject_post_state(
    env: &mut HashMap<String, String>,
    entry_point: &str,
    step: &Step,
    job_state: &JobState,
) {
    if entry_point != "post" {
        return;
    }
    let action_ctx = step
        .context_name
        .as_deref()
        .unwrap_or("")
        .replace("_post", "");
    if let Some(states) = job_state.action_states.get(&action_ctx) {
        for (k, v) in states {
            env.insert(format!("STATE_{k}"), v.clone());
        }
    }
}

fn resolve_args(
    args: &[String],
    env: &HashMap<String, String>,
    job_state: &JobState,
) -> Vec<String> {
    args.iter()
        .map(|a| {
            let ctx = ExprContext::new(env, job_state, false, false);
            crate::job::expression::resolve_expression(a, &ctx)
        })
        .collect()
}

// ── Container lifecycle ─────────────────────────────────────────

struct RunDockerParams<'a> {
    image: &'a str,
    entrypoint: Option<&'a str>,
    args: &'a [String],
    env: &'a HashMap<String, String>,
    step: &'a Step,
    job_state: &'a mut JobState,
    workspace: &'a Workspace,
    log_sender: &'a LogSender,
    cancel_token: &'a CancellationToken,
    docker_resources: Option<&'a JobDockerResources>,
    action_dir: Option<&'a Path>,
}

async fn run_docker_container(params: RunDockerParams<'_>) -> Result<StepResult> {
    let owned_docker;
    let docker: &Docker = match params.docker_resources {
        Some(resources) => resources.docker(),
        None => {
            owned_docker = crate::docker::client::connect(None)?;
            &owned_docker
        }
    };

    crate::docker::client::ensure_image(docker, params.image, None).await?;

    let container_env = build_container_env(params.env);
    let env_list: Vec<String> = container_env
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();

    let binds = build_bind_mounts(params.workspace, params.action_dir)?;
    let network_mode = params
        .docker_resources
        .and_then(|r| r.network_name())
        .map(|n| n.to_string());

    let container_name = format!("chimera-docker-{}", params.step.id);
    let entrypoint_vec = params.entrypoint.map(|ep| vec![ep.to_string()]);
    let cmd: Option<Vec<&str>> = if params.args.is_empty() {
        None
    } else {
        Some(params.args.iter().map(|s| s.as_str()).collect())
    };

    let network_for_config = network_mode.clone();
    let networking_config =
        network_for_config
            .as_ref()
            .map(|net| bollard::container::NetworkingConfig {
                endpoints_config: HashMap::from([(net.as_str(), EndpointSettings::default())]),
            });

    let config = Config {
        image: Some(params.image),
        entrypoint: entrypoint_vec
            .as_deref()
            .map(|v| v.iter().map(|s| s.as_str()).collect()),
        cmd,
        env: Some(env_list.iter().map(|s| s.as_str()).collect()),
        working_dir: Some("/github/workspace"),
        host_config: Some(HostConfig {
            binds: Some(binds),
            network_mode,
            security_opt: Some(vec!["no-new-privileges:true".into()]),
            ..Default::default()
        }),
        networking_config,
        ..Default::default()
    };

    let container = docker
        .create_container(
            Some(CreateContainerOptions {
                name: container_name.as_str(),
                ..Default::default()
            }),
            config,
        )
        .await
        .context("creating docker action container")?;

    let container_id = container.id;
    let result = start_and_stream_logs(
        docker,
        &container_id,
        params.step,
        params.job_state,
        params.log_sender,
        params.cancel_token,
    )
    .await;

    stop_and_remove(docker, &container_id, "docker action").await;
    result
}

fn build_container_env(host_env: &HashMap<String, String>) -> HashMap<String, String> {
    let mut env = host_env.clone();
    let remaps = [
        ("GITHUB_WORKSPACE", "/github/workspace"),
        ("GITHUB_ENV", "/github/workflow/_env"),
        ("GITHUB_PATH", "/github/workflow/_path"),
        ("GITHUB_OUTPUT", "/github/workflow/_output"),
        ("GITHUB_STATE", "/github/workflow/_state"),
        ("GITHUB_STEP_SUMMARY", "/github/workflow/_step_summary"),
        ("RUNNER_TEMP", "/github/tmp"),
        ("RUNNER_TOOL_CACHE", "/github/tool-cache"),
    ];
    for (key, val) in remaps {
        env.insert(key.into(), val.into());
    }
    env
}

fn build_bind_mounts(workspace: &Workspace, action_dir: Option<&Path>) -> Result<Vec<String>> {
    let workspace_dir = workspace.workspace_dir();
    let workflow_files = workspace_dir.parent().context("workspace has no parent")?;

    let mut binds = vec![
        format!("{}:/github/workspace", workspace_dir.display()),
        format!("{}:/github/workflow", workflow_files.display()),
        format!("{}:/github/tmp", workspace.runner_temp().display()),
    ];

    if let Some(dir) = action_dir {
        binds.push(format!("{}:/github/action:ro", dir.display()));
    }

    Ok(binds)
}

// ── Log streaming + exit code ───────────────────────────────────

async fn start_and_stream_logs(
    docker: &Docker,
    container_id: &str,
    step: &Step,
    job_state: &mut JobState,
    log_sender: &LogSender,
    cancel_token: &CancellationToken,
) -> Result<StepResult> {
    docker
        .start_container::<String>(container_id, None)
        .await
        .context("starting docker action container")?;

    let timeout = Duration::from_secs(step.timeout_in_minutes.unwrap_or(360) * 60);

    let collected_env = Arc::new(tokio::sync::Mutex::new(Vec::<(String, String)>::new()));
    let collected_paths = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
    let collected_outputs = Arc::new(tokio::sync::Mutex::new(Vec::<(String, String)>::new()));
    let collected_state = Arc::new(tokio::sync::Mutex::new(Vec::<(String, String)>::new()));

    let stream_task = {
        let sender = log_sender.clone();
        let masks = job_state.masks.clone();
        let env_buf = collected_env.clone();
        let path_buf = collected_paths.clone();
        let output_buf = collected_outputs.clone();
        let state_buf = collected_state.clone();
        let docker = docker.clone();
        let cid = container_id.to_string();

        tokio::spawn(async move {
            let mut stream = docker.logs::<String>(
                &cid,
                Some(LogsOptions {
                    follow: true,
                    stdout: true,
                    stderr: true,
                    ..Default::default()
                }),
            );
            while let Some(Ok(output)) = stream.next().await {
                let text = output.to_string();
                for line in text.lines() {
                    process_output_line(
                        line,
                        &sender,
                        &masks,
                        &env_buf,
                        &path_buf,
                        &output_buf,
                        &state_buf,
                    )
                    .await;
                }
            }
        })
    };

    let stream_result = tokio::select! {
        timeout_result = tokio::time::timeout(timeout, stream_task) => {
            match timeout_result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => {
                    warn!(error = %e, "docker action stream task panicked");
                    Ok(())
                }
                Err(_) => {
                    warn!("docker action timed out");
                    Err(StepConclusion::Failed)
                }
            }
        }
        _ = cancel_token.cancelled() => {
            warn!("job cancelled, stopping docker action container");
            Err(StepConclusion::Cancelled)
        }
    };

    if let Err(conclusion) = stream_result {
        return Ok(StepResult { conclusion });
    }

    apply_collected_mutations(
        job_state,
        &collected_env,
        &collected_paths,
        &collected_outputs,
        &collected_state,
    )
    .await;

    let exit_code = docker
        .inspect_container(container_id, None)
        .await
        .context("inspecting docker action container")?
        .state
        .and_then(|s| s.exit_code)
        .unwrap_or(-1);

    Ok(StepResult {
        conclusion: if exit_code == 0 {
            StepConclusion::Succeeded
        } else {
            StepConclusion::Failed
        },
    })
}

async fn apply_collected_mutations(
    job_state: &mut JobState,
    env_buf: &Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
    path_buf: &Arc<tokio::sync::Mutex<Vec<String>>>,
    output_buf: &Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
    state_buf: &Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
) {
    for (k, v) in env_buf.lock().await.drain(..) {
        job_state.env.insert(k, v);
    }
    job_state
        .path_prepends
        .extend(path_buf.lock().await.drain(..));
    for (k, v) in output_buf.lock().await.drain(..) {
        job_state.outputs.insert(k, v);
    }
    for (k, v) in state_buf.lock().await.drain(..) {
        job_state
            .action_states
            .entry(String::new())
            .or_default()
            .insert(k, v);
    }
}

// ── Utilities ───────────────────────────────────────────────────

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

/// Split a string into arguments respecting double and single quotes.
/// `"-c" "echo hello && world"` → `["-c", "echo hello && world"]`
fn split_shell_args(s: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let mut quote_char = ' ';

    for ch in s.chars() {
        if in_quote {
            if ch == quote_char {
                in_quote = false;
            } else {
                current.push(ch);
            }
        } else if ch == '"' || ch == '\'' {
            in_quote = true;
            quote_char = ch;
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                args.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

#[cfg(test)]
#[path = "docker_test.rs"]
mod docker_test;
