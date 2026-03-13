use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecResults};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::output::OutputProcessor;
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
    debug_enabled: bool,
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

    let processor =
        OutputProcessor::new(log_sender.clone(), job_state.masks.clone(), debug_enabled);

    let stream_processor = processor.clone();
    let stream_task = tokio::spawn(async move {
        while let Some(Ok(output)) = output.next().await {
            let text = output.to_string();
            for line in text.lines() {
                stream_processor.process_line(line).await;
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

    processor.apply_to_job_state(job_state).await;

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

#[cfg(test)]
#[path = "exec_test.rs"]
mod exec_test;
