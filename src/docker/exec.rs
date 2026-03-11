use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecResults};
use futures::StreamExt;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::job::commands::{WorkflowCommand, parse_command};
use crate::job::execute::{JobState, StepConclusion, StepResult};
use crate::job::logs::LogSender;

/// Run a command inside a running container via `docker exec`.
///
/// This is the container equivalent of `run_process()` — it handles stdout/stderr
/// streaming, workflow command parsing, timeout, and cancellation.
#[allow(clippy::too_many_arguments)]
pub async fn docker_exec(
    docker: &Docker,
    container_id: &str,
    cmd: Vec<String>,
    env: &HashMap<String, String>,
    working_dir: &str,
    job_state: &mut JobState,
    log_sender: &LogSender,
    timeout: Duration,
    cancel_token: &CancellationToken,
) -> Result<StepResult> {
    let env_list: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();

    let exec = docker
        .create_exec(
            container_id,
            CreateExecOptions::<String> {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                cmd: Some(cmd),
                env: Some(env_list),
                working_dir: Some(working_dir.to_string()),
                ..Default::default()
            },
        )
        .await
        .context("creating docker exec")?;

    let exec_output = docker
        .start_exec(&exec.id, None)
        .await
        .context("starting docker exec")?;

    let StartExecResults::Attached { mut output, .. } = exec_output else {
        anyhow::bail!("docker exec did not return attached output");
    };

    let collected_env = Arc::new(tokio::sync::Mutex::new(Vec::<(String, String)>::new()));
    let collected_paths = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
    let collected_outputs = Arc::new(tokio::sync::Mutex::new(Vec::<(String, String)>::new()));
    let collected_state = Arc::new(tokio::sync::Mutex::new(Vec::<(String, String)>::new()));

    let sender = log_sender.clone();
    let masks = job_state.masks.clone();
    let env_buf = collected_env.clone();
    let path_buf = collected_paths.clone();
    let output_buf = collected_outputs.clone();
    let state_buf = collected_state.clone();

    let stream_task = tokio::spawn(async move {
        while let Some(Ok(output)) = output.next().await {
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
    });
    let stream_abort = stream_task.abort_handle();

    let timed_stream = tokio::time::timeout(timeout, stream_task);

    let result = tokio::select! {
        timeout_result = timed_stream => {
            match timeout_result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => {
                    warn!(error = %e, "docker exec stream task panicked");
                    Ok(())
                }
                Err(_) => {
                    warn!("docker exec timed out");
                    stream_abort.abort();
                    Err(StepConclusion::Failed)
                }
            }
        }
        _ = cancel_token.cancelled() => {
            warn!("job cancelled, docker exec will be stopped");
            stream_abort.abort();
            Err(StepConclusion::Cancelled)
        }
    };

    if let Err(conclusion) = result {
        return Ok(StepResult { conclusion });
    }

    // Apply collected state mutations
    for (k, v) in collected_env.lock().await.drain(..) {
        job_state.env.insert(k, v);
    }
    job_state
        .path_prepends
        .extend(collected_paths.lock().await.drain(..));
    for (k, v) in collected_outputs.lock().await.drain(..) {
        job_state.outputs.insert(k, v);
    }
    for (k, v) in collected_state.lock().await.drain(..) {
        job_state
            .action_states
            .entry(String::new())
            .or_default()
            .insert(k, v);
    }

    // Check exit code
    let inspect = docker
        .inspect_exec(&exec.id)
        .await
        .context("inspecting docker exec result")?;

    let exit_code = inspect.exit_code.unwrap_or(-1);
    let conclusion = if exit_code == 0 {
        StepConclusion::Succeeded
    } else {
        StepConclusion::Failed
    };

    Ok(StepResult { conclusion })
}

/// Process a single output line: parse workflow commands and forward to log sender.
///
/// Shared between `run_process()` (host mode) and `docker_exec()` (container mode).
pub async fn process_output_line(
    line: &str,
    sender: &LogSender,
    masks: &Arc<RwLock<Vec<String>>>,
    env_buf: &Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
    path_buf: &Arc<tokio::sync::Mutex<Vec<String>>>,
    output_buf: &Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
    state_buf: &Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
) {
    if let Some(cmd) = parse_command(line) {
        match cmd {
            WorkflowCommand::SetEnv { name, value } => {
                env_buf.lock().await.push((name, value));
            }
            WorkflowCommand::AddPath(p) => {
                path_buf.lock().await.push(p);
            }
            WorkflowCommand::SetOutput { name, value } => {
                output_buf.lock().await.push((name, value));
            }
            WorkflowCommand::AddMask(secret) => {
                masks.write().await.push(secret);
            }
            WorkflowCommand::Debug(msg) => {
                sender.send(format!("##[debug]{msg}")).await;
            }
            WorkflowCommand::Warning(msg) => {
                sender.send(format!("##[warning]{msg}")).await;
            }
            WorkflowCommand::Error(msg) => {
                sender.send(format!("##[error]{msg}")).await;
            }
            WorkflowCommand::Group(title) => {
                sender.send(format!("##[group]{title}")).await;
            }
            WorkflowCommand::EndGroup => {
                sender.send("##[endgroup]".into()).await;
            }
            WorkflowCommand::SaveState { name, value } => {
                state_buf.lock().await.push((name, value));
            }
        }
    } else {
        sender.send(line.to_string()).await;
    }
}

#[cfg(test)]
#[path = "exec_test.rs"]
mod exec_test;
