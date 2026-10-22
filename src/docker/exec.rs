use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::ExecInspectResponse;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};
use uuid::Uuid;

use super::output::process_docker_output;
use crate::job::commands::CommandPolicy;
use crate::job::execute::{JobState, OUTPUT_DRAIN_GRACE, StepConclusion, StepResult};
use crate::job::logs::LogSender;
use crate::job::output::OutputProcessor;

const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Tags every process a step starts, so a cancel can find them inside the container.
const EXEC_MARKER_ENV: &str = "CHIMERA_EXEC_ID";

/// How long stopping a step's processes may take before the step is left behind.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

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
    let marker = exec_marker();
    let env_list: Vec<String> = env
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .chain(std::iter::once(marker.clone()))
        .collect();

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

    let StartExecResults::Attached { output, .. } = exec_output else {
        anyhow::bail!("docker exec did not return attached output");
    };

    let processor = OutputProcessor::new(
        log_sender.clone(),
        job_state.masks.clone(),
        debug_enabled,
        CommandPolicy::from_env(env),
    );

    let stream_processor = processor.clone();
    let stream_task = tokio::spawn(async move {
        process_docker_output(output, &stream_processor).await;
    });
    let stream_abort = stream_task.abort_handle();

    // The stream stays open while any process holds the exec's output, including a
    // background process the step left behind, so the exec's exit also ends the step.
    let finished = async {
        let mut stream_task = stream_task;
        tokio::select! {
            result = &mut stream_task => result,
            () = wait_for_exit(docker, &exec.id) => {
                match tokio::time::timeout(OUTPUT_DRAIN_GRACE, &mut stream_task).await {
                    Ok(result) => result,
                    Err(_) => {
                        warn!("step exited but a process it started still holds its output, detaching");
                        stream_task.abort();
                        Ok(())
                    }
                }
            }
        }
    };

    let timed_stream = tokio::time::timeout(timeout, finished);

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
        stop_exec(docker, container_id, &exec.id, &marker, log_sender).await;
        return Ok(StepResult { conclusion });
    }

    processor.apply_to_job_state(job_state).await;

    // Check exit code
    let inspect = docker
        .inspect_exec(&exec.id)
        .await
        .context("inspecting docker exec result")?;

    let exit_code = inspect.exit_code.unwrap_or(-1);
    let conclusion = if exit_code == 0 && !processor.command_failed() {
        StepConclusion::Succeeded
    } else {
        StepConclusion::Failed
    };

    Ok(StepResult { conclusion })
}

/// Kills what a cancelled or timed out step left running inside the container.
///
/// Docker has no API to signal an exec, and dropping the attached stream leaves its
/// process running until the job container is removed, alongside `always()` and post
/// steps. A second exec finds the step's processes by their marker and kills them,
/// with SIGKILL like a host step's process group. If that fails, the step's log says
/// so too, as whoever reads the job never sees the daemon's log.
async fn stop_exec(
    docker: &Docker,
    container_id: &str,
    exec_id: &str,
    marker: &str,
    log_sender: &LogSender,
) {
    let killed = tokio::time::timeout(
        STOP_TIMEOUT,
        kill_marked_processes(docker, container_id, marker),
    )
    .await
    .unwrap_or_else(|_| Err(anyhow!("killing them timed out")));
    if let Err(e) = killed {
        warn!(error = %e, "could not kill the step's processes, they keep running until the job container is removed");
        log_sender.send(processes_left_running_warning(&e)).await;
        return;
    }

    if tokio::time::timeout(STOP_TIMEOUT, wait_for_exit(docker, exec_id))
        .await
        .is_err()
    {
        warn!("step's process did not exit after being killed");
    }
}

fn processes_left_running_warning(error: &anyhow::Error) -> String {
    format!(
        "##[warning]Could not stop the step's processes in the job container ({error:#}), they may keep running until the container is removed"
    )
}

async fn kill_marked_processes(docker: &Docker, container_id: &str, marker: &str) -> Result<()> {
    let exec = docker
        .create_exec(
            container_id,
            CreateExecOptions::<String> {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                cmd: Some(kill_marked_processes_command(marker)),
                ..Default::default()
            },
        )
        .await
        .context("creating the kill exec")?;

    let started = docker
        .start_exec(&exec.id, None)
        .await
        .context("starting the kill exec")?;
    let StartExecResults::Attached { mut output, .. } = started else {
        anyhow::bail!("kill exec did not return attached output");
    };
    while let Some(frame) = output.next().await {
        match frame {
            Ok(line) => debug!(output = %line, "kill exec output"),
            Err(e) => {
                debug!(error = %e, "reading kill exec output failed");
                break;
            }
        }
    }

    let exit_code = docker
        .inspect_exec(&exec.id)
        .await
        .context("inspecting the kill exec")?
        .exit_code;
    if exit_code != Some(0) {
        anyhow::bail!("kill exec exited with {exit_code:?}, the container may lack sh or grep");
    }
    Ok(())
}

/// A unique `NAME=value` entry for one exec's environment.
fn exec_marker() -> String {
    format!("{EXEC_MARKER_ENV}={}", Uuid::new_v4())
}

/// A shell command that SIGKILLs every process whose environment carries `marker`.
///
/// It runs inside the container, where the step's PIDs live in another namespace than
/// the one Docker reports, so it scans the container's own `/proc`. Children inherit
/// the marker, which reaches everything the step started, like a process group.
fn kill_marked_processes_command(marker: &str) -> Vec<String> {
    let script = format!(
        "command -v grep >/dev/null || exit 127\n\
         for environ in /proc/[0-9]*/environ; do\n\
         grep -qF '{marker}' \"$environ\" 2>/dev/null || continue\n\
         pid=${{environ#/proc/}}\n\
         kill -9 \"${{pid%/environ}}\" 2>/dev/null\n\
         done\n\
         exit 0"
    );
    vec!["sh".into(), "-c".into(), script]
}

/// Resolves once the exec's main process has exited.
async fn wait_for_exit(docker: &Docker, exec_id: &str) {
    loop {
        match docker.inspect_exec(exec_id).await {
            Ok(inspect) if has_exited(&inspect) => return,
            Ok(_) => {}
            Err(e) => debug!(error = %e, "inspecting docker exec failed"),
        }
        tokio::time::sleep(EXIT_POLL_INTERVAL).await;
    }
}

/// True once the exec's process has run and exited.
///
/// Docker reports `running: false` with no exit code in the window between
/// `start_exec` returning and the process actually starting.
fn has_exited(inspect: &ExecInspectResponse) -> bool {
    inspect.running == Some(false) && inspect.exit_code.is_some()
}

#[cfg(test)]
#[path = "exec_test.rs"]
mod exec_test;
