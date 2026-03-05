use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use super::JobClient;
use super::commands::{WorkflowCommand, parse_command};
use super::logs::{self, LogSender};
use super::schema::{JobManifest, Step};
use super::timeline::{
    TimelineLogRef, TimelineRecord, TimelineResult, TimelineState, format_timeline_timestamp,
};
use super::workspace::Workspace;

pub struct JobState {
    pub env: HashMap<String, String>,
    pub path_prepends: Vec<String>,
    pub outputs: HashMap<String, String>,
    pub masks: Arc<RwLock<Vec<String>>>,
}

impl JobState {
    pub fn new(masks: Arc<RwLock<Vec<String>>>) -> Self {
        Self {
            env: HashMap::new(),
            path_prepends: Vec::new(),
            outputs: HashMap::new(),
            masks,
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum StepConclusion {
    Succeeded,
    Failed,
}

pub struct StepResult {
    pub conclusion: StepConclusion,
}

/// Execute a single run: step as a host process.
pub async fn run_host_step(
    step: &Step,
    job_state: &mut JobState,
    workspace: &Workspace,
    base_env: &HashMap<String, String>,
    log_sender: &LogSender,
) -> Result<StepResult> {
    let script = step
        .inputs
        .get("script")
        .context("step has no 'script' input")?;

    // Write script to temp file
    let script_file = workspace.runner_temp().join(format!("step_{}.sh", step.id));
    std::fs::write(&script_file, script)
        .with_context(|| format!("writing script file {}", script_file.display()))?;

    // Build environment
    let mut env = base_env.clone();
    env.extend(job_state.env.clone());
    if let Some(step_env) = &step.environment {
        env.extend(step_env.clone());
    }

    // Apply GITHUB_ENV file mutations
    if let Ok(file_env) = workspace.read_env_file() {
        env.extend(file_env);
    }

    // Apply GITHUB_PATH file mutations
    if let Ok(extra_paths) = workspace.read_path_file() {
        let mut all_paths = job_state.path_prepends.clone();
        all_paths.extend(extra_paths);
        if let Some(existing_path) = env.get("PATH") {
            let new_path = format!("{}:{existing_path}", all_paths.join(":"));
            env.insert("PATH".into(), new_path);
        } else if !all_paths.is_empty() {
            env.insert("PATH".into(), all_paths.join(":"));
        }
    }

    let timeout = Duration::from_secs(step.timeout_in_minutes.unwrap_or(360) * 60);

    debug!(
        step_id = %step.id,
        step_name = %step.display_name,
        step_ref = %step.reference.name,
        "running host step"
    );

    let mut child = Command::new("bash")
        .arg("-e")
        .arg(&script_file)
        .current_dir(workspace.workspace_dir())
        .envs(&env)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning bash for step {}", step.id))?;

    let stdout = child.stdout.take().context("no stdout")?;
    let stderr = child.stderr.take().context("no stderr")?;

    let stdout_sender = log_sender.clone();
    let job_state_env = Arc::new(tokio::sync::Mutex::new(Vec::<(String, String)>::new()));
    let job_state_paths = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
    let job_state_outputs = Arc::new(tokio::sync::Mutex::new(Vec::<(String, String)>::new()));
    let masks_clone = job_state.masks.clone();

    let env_clone = job_state_env.clone();
    let paths_clone = job_state_paths.clone();
    let outputs_clone = job_state_outputs.clone();

    let stdout_task = tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(cmd) = parse_command(&line) {
                match cmd {
                    WorkflowCommand::SetEnv { name, value } => {
                        env_clone.lock().await.push((name, value));
                    }
                    WorkflowCommand::AddPath(p) => {
                        paths_clone.lock().await.push(p);
                    }
                    WorkflowCommand::SetOutput { name, value } => {
                        outputs_clone.lock().await.push((name, value));
                    }
                    WorkflowCommand::AddMask(secret) => {
                        masks_clone.write().await.push(secret);
                    }
                    WorkflowCommand::Debug(msg) => {
                        stdout_sender.send(format!("##[debug]{msg}")).await;
                        continue;
                    }
                    WorkflowCommand::Warning(msg) => {
                        stdout_sender.send(format!("##[warning]{msg}")).await;
                        continue;
                    }
                    WorkflowCommand::Error(msg) => {
                        stdout_sender.send(format!("##[error]{msg}")).await;
                        continue;
                    }
                    WorkflowCommand::Group(title) => {
                        stdout_sender.send(format!("##[group]{title}")).await;
                        continue;
                    }
                    WorkflowCommand::EndGroup => {
                        stdout_sender.send("##[endgroup]".into()).await;
                        continue;
                    }
                    WorkflowCommand::SaveState { .. } => {}
                }
            } else {
                stdout_sender.send(line).await;
            }
        }
    });

    let stderr_sender = log_sender.clone();
    let stderr_task = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            stderr_sender.send(line).await;
        }
    });

    let timed_wait = async {
        let (stdout_result, stderr_result, wait_result) =
            tokio::join!(stdout_task, stderr_task, child.wait());
        stdout_result.context("stdout task panicked")?;
        stderr_result.context("stderr task panicked")?;
        wait_result.context("waiting for child process")
    };

    let status = match tokio::time::timeout(timeout, timed_wait).await {
        Ok(result) => result?,
        Err(_) => {
            warn!(step_id = %step.id, "step timed out, killing process");
            let _ = child.kill().await;
            return Ok(StepResult {
                conclusion: StepConclusion::Failed,
            });
        }
    };

    // Apply collected state mutations
    for (k, v) in job_state_env.lock().await.drain(..) {
        job_state.env.insert(k, v);
    }
    job_state
        .path_prepends
        .extend(job_state_paths.lock().await.drain(..));
    for (k, v) in job_state_outputs.lock().await.drain(..) {
        job_state.outputs.insert(k, v);
    }

    // Clean up script file
    let _ = std::fs::remove_file(&script_file);

    let conclusion = if status.success() {
        StepConclusion::Succeeded
    } else {
        StepConclusion::Failed
    };

    Ok(StepResult { conclusion })
}

/// Run all steps in a job manifest. Returns "success" or "failure".
pub async fn run_all_steps(
    manifest: &JobManifest,
    job_client: &Arc<JobClient>,
    workspace: &Workspace,
    base_env: &HashMap<String, String>,
) -> Result<String> {
    let masks: Arc<RwLock<Vec<String>>> = Arc::new(RwLock::new(Vec::new()));

    // Add secret variables as masks
    {
        let mut mask_guard = masks.write().await;
        for var in manifest.variables.values() {
            if var.is_secret && !var.value.is_empty() {
                mask_guard.push(var.value.clone());
            }
        }
    }

    let mut job_state = JobState::new(masks.clone());
    let mut job_failed = false;

    for step in &manifest.steps {
        let is_action = step.reference.r#type == "action";

        if let Some(condition) = &step.condition {
            debug!(step = %step.display_name, condition, "step has condition");
        }

        // Update timeline: step starting
        let start_time = format_timeline_timestamp(Utc::now());
        let _ = job_client
            .update_timeline(
                &manifest.plan.plan_id,
                &manifest.plan.timeline_id,
                &[TimelineRecord {
                    id: step.id.clone(),
                    state: Some(TimelineState::InProgress),
                    result: None,
                    start_time: Some(start_time.clone()),
                    finish_time: None,
                    name: Some(step.display_name.clone()),
                    order: Some(step.order),
                    log: None,
                }],
            )
            .await;

        // Create log for this step
        let log_id = job_client
            .create_log(&manifest.plan.plan_id, &step.display_name)
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "failed to create log, using 0");
                0
            });

        let (log_sender, log_handle) = logs::start_log_upload(
            job_client.clone(),
            manifest.plan.plan_id.clone(),
            log_id,
            masks.clone(),
        );

        let (conclusion, result) = if is_action {
            log_sender
                .send("Action steps are not yet supported, marking as succeeded".into())
                .await;
            (StepConclusion::Succeeded, TimelineResult::Succeeded)
        } else if job_failed && !step.continue_on_error {
            log_sender
                .send("Skipping step due to previous failure".into())
                .await;
            (StepConclusion::Failed, TimelineResult::Cancelled)
        } else {
            match run_host_step(step, &mut job_state, workspace, base_env, &log_sender).await {
                Ok(step_result) => {
                    let timeline_result = match step_result.conclusion {
                        StepConclusion::Succeeded => TimelineResult::Succeeded,
                        StepConclusion::Failed => TimelineResult::Failed,
                    };
                    (step_result.conclusion, timeline_result)
                }
                Err(e) => {
                    log_sender.send(format!("Step error: {e}")).await;
                    (StepConclusion::Failed, TimelineResult::Failed)
                }
            }
        };

        // Close log sender and wait for flush
        drop(log_sender);
        let _ = log_handle.await;

        // Update timeline: step completed
        let finish_time = format_timeline_timestamp(Utc::now());
        let _ = job_client
            .update_timeline(
                &manifest.plan.plan_id,
                &manifest.plan.timeline_id,
                &[TimelineRecord {
                    id: step.id.clone(),
                    state: Some(TimelineState::Completed),
                    result: Some(result),
                    start_time: Some(start_time),
                    finish_time: Some(finish_time),
                    name: Some(step.display_name.clone()),
                    order: Some(step.order),
                    log: Some(TimelineLogRef { id: log_id }),
                }],
            )
            .await;

        if conclusion == StepConclusion::Failed {
            if step.continue_on_error {
                info!(step = %step.display_name, "step failed but continue_on_error is set");
            } else {
                job_failed = true;
            }
        }
    }

    // Merge file-based outputs into job_state
    if let Ok(file_outputs) = workspace.read_output_file() {
        job_state.outputs.extend(file_outputs);
    }

    Ok(if job_failed {
        "failure".to_string()
    } else {
        "success".to_string()
    })
}

#[cfg(test)]
#[path = "execute_test.rs"]
mod execute_test;
