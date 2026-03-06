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
use super::client::{
    CONCLUSION_CANCELLED, CONCLUSION_FAILURE, CONCLUSION_SUCCESS, CONCLUSION_UNKNOWN, ResultsStep,
    STATUS_COMPLETED, STATUS_IN_PROGRESS,
};
use super::commands::{WorkflowCommand, parse_command};
use super::logs::{LogSender, StepLogger};
use super::schema::{JobManifest, Step};
use super::timeline::{TimelineLogRef, TimelineRecord, TimelineResult, TimelineState};
use super::workspace::Workspace;
use crate::utils::{format_results_timestamp, format_timeline_timestamp};

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

/// Per-step tracking for Results API updates.
struct StepTracker {
    id: String,
    name: String,
    order: u32,
    status: i32,
    conclusion: i32,
    started_at: Option<String>,
    completed_at: Option<String>,
}

impl StepTracker {
    fn from_step(step: &Step) -> Self {
        Self {
            id: step.id.clone(),
            name: step.display_name.clone(),
            order: step.order,
            status: 0,
            conclusion: CONCLUSION_UNKNOWN,
            started_at: None,
            completed_at: None,
        }
    }

    fn mark_started(&mut self) {
        self.status = STATUS_IN_PROGRESS;
        self.started_at = Some(format_results_timestamp(Utc::now()));
    }

    fn mark_completed(&mut self, conclusion: i32) {
        self.status = STATUS_COMPLETED;
        self.conclusion = conclusion;
        self.completed_at = Some(format_results_timestamp(Utc::now()));
    }

    fn to_results_step(&self) -> ResultsStep {
        ResultsStep {
            external_id: self.id.clone(),
            number: self.order,
            name: self.name.clone(),
            status: self.status,
            started_at: self.started_at.clone(),
            completed_at: self.completed_at.clone(),
            conclusion: self.conclusion,
        }
    }
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

    let script_file = workspace.runner_temp().join(format!("step_{}.sh", step.id));
    std::fs::write(&script_file, script)
        .with_context(|| format!("writing script file {}", script_file.display()))?;

    let env = build_step_env(step, job_state, workspace, base_env);
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

    // Collect state mutations from workflow commands in background tasks
    let collected_env = Arc::new(tokio::sync::Mutex::new(Vec::<(String, String)>::new()));
    let collected_paths = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
    let collected_outputs = Arc::new(tokio::sync::Mutex::new(Vec::<(String, String)>::new()));

    let stdout_task = spawn_stdout_reader(
        stdout,
        log_sender.clone(),
        job_state.masks.clone(),
        collected_env.clone(),
        collected_paths.clone(),
        collected_outputs.clone(),
    );

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
            // Kill closes the child's pipes, which will cause the reader tasks to
            // finish. We don't need to await them — they hold only Arc clones and
            // will clean up independently.
            warn!(step_id = %step.id, "step timed out, killing process");
            let _ = child.kill().await;
            return Ok(StepResult {
                conclusion: StepConclusion::Failed,
            });
        }
    };

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

    let _ = std::fs::remove_file(&script_file);

    let conclusion = if status.success() {
        StepConclusion::Succeeded
    } else {
        StepConclusion::Failed
    };

    Ok(StepResult { conclusion })
}

/// Build the full environment for a step execution.
fn build_step_env(
    step: &Step,
    job_state: &JobState,
    workspace: &Workspace,
    base_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut env = base_env.clone();
    env.extend(job_state.env.clone());
    if let Some(step_env) = &step.environment {
        env.extend(step_env.clone());
    }

    if let Ok(file_env) = workspace.read_env_file() {
        env.extend(file_env);
    }

    if let Ok(extra_paths) = workspace.read_path_file() {
        let mut all_paths = job_state.path_prepends.clone();
        all_paths.extend(extra_paths);
        if let Some(existing_path) = env.get("PATH") {
            env.insert(
                "PATH".into(),
                format!("{}:{existing_path}", all_paths.join(":")),
            );
        } else if !all_paths.is_empty() {
            env.insert("PATH".into(), all_paths.join(":"));
        }
    }

    env
}

/// Spawn a task that reads stdout, parses workflow commands, and forwards log lines.
fn spawn_stdout_reader(
    stdout: tokio::process::ChildStdout,
    sender: LogSender,
    masks: Arc<RwLock<Vec<String>>>,
    env_buf: Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
    path_buf: Arc<tokio::sync::Mutex<Vec<String>>>,
    output_buf: Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(cmd) = parse_command(&line) {
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
                    WorkflowCommand::SaveState { .. } => {}
                }
            } else {
                sender.send(line).await;
            }
        }
    })
}

/// Run all steps in a job manifest. Returns "succeeded" or "failed".
pub async fn run_all_steps(
    manifest: &JobManifest,
    job_client: &Arc<JobClient>,
    workspace: &Workspace,
    base_env: &HashMap<String, String>,
    runner_name: &str,
) -> Result<String> {
    let masks = collect_secret_masks(manifest).await;
    let mut job_state = JobState::new(masks.clone());
    let mut job_failed = false;
    let use_results = job_client.has_results_url();

    let mut trackers: Vec<StepTracker> =
        manifest.steps.iter().map(StepTracker::from_step).collect();

    let mut job_log_buffer = String::new();
    let mut job_line_count: i64 = 0;

    for (idx, step) in manifest.steps.iter().enumerate() {
        if let Some(condition) = &step.condition {
            debug!(step = %step.display_name, condition, "step has condition");
        }

        let start_time = format_timeline_timestamp(Utc::now());
        trackers[idx].mark_started();

        report_step_started(
            use_results,
            job_client,
            manifest,
            &trackers,
            step,
            &start_time,
        )
        .await;

        let logger = create_step_logger(
            use_results,
            job_client,
            &manifest.plan.plan_id,
            &step.display_name,
            masks.clone(),
        )
        .await;

        let (conclusion, result_conclusion) = execute_step(
            step,
            &mut job_state,
            workspace,
            base_env,
            job_failed,
            logger.sender(),
            runner_name,
        )
        .await;

        let legacy_log_id = logger.log_id();

        if let Some(collected) = logger
            .finish(
                job_client,
                &manifest.plan.plan_id,
                &manifest.plan.job_id,
                &step.id,
                &step.display_name,
            )
            .await
        {
            job_log_buffer.push_str(&collected.text);
            job_line_count += collected.line_count;
        }

        let finish_time = format_timeline_timestamp(Utc::now());
        trackers[idx].mark_completed(result_conclusion);

        report_step_completed(
            use_results,
            job_client,
            manifest,
            &trackers,
            step,
            &start_time,
            &finish_time,
            result_conclusion,
            legacy_log_id,
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

    upload_job_log(
        use_results,
        job_client,
        manifest,
        &job_log_buffer,
        job_line_count,
    )
    .await;

    if let Ok(file_outputs) = workspace.read_output_file() {
        job_state.outputs.extend(file_outputs);
    }

    Ok(if job_failed {
        "failed".to_string()
    } else {
        "succeeded".to_string()
    })
}

async fn collect_secret_masks(manifest: &JobManifest) -> Arc<RwLock<Vec<String>>> {
    let masks: Vec<String> = manifest
        .variables
        .values()
        .filter(|v| v.is_secret && !v.value.is_empty())
        .map(|v| v.value.clone())
        .collect();
    Arc::new(RwLock::new(masks))
}

async fn create_step_logger(
    use_results: bool,
    client: &Arc<JobClient>,
    plan_id: &str,
    step_name: &str,
    masks: Arc<RwLock<Vec<String>>>,
) -> StepLogger {
    if use_results {
        StepLogger::results(masks)
    } else {
        StepLogger::legacy(client.clone(), plan_id, step_name, masks).await
    }
}

/// Decide what to do with a step and run it.
async fn execute_step(
    step: &Step,
    job_state: &mut JobState,
    workspace: &Workspace,
    base_env: &HashMap<String, String>,
    job_failed: bool,
    log_sender: &LogSender,
    runner_name: &str,
) -> (StepConclusion, i32) {
    log_sender.send_banner(runner_name).await;

    if !step.is_script() {
        log_sender
            .send("Action steps are not yet supported, marking as succeeded".into())
            .await;
        return (StepConclusion::Succeeded, CONCLUSION_SUCCESS);
    }

    if job_failed && !step.continue_on_error {
        log_sender
            .send("Skipping step due to previous failure".into())
            .await;
        return (StepConclusion::Failed, CONCLUSION_CANCELLED);
    }

    match run_host_step(step, job_state, workspace, base_env, log_sender).await {
        Ok(result) => {
            let rc = match result.conclusion {
                StepConclusion::Succeeded => CONCLUSION_SUCCESS,
                StepConclusion::Failed => CONCLUSION_FAILURE,
            };
            (result.conclusion, rc)
        }
        Err(e) => {
            log_sender.send(format!("Step error: {e}")).await;
            (StepConclusion::Failed, CONCLUSION_FAILURE)
        }
    }
}

async fn report_step_started(
    use_results: bool,
    client: &Arc<JobClient>,
    manifest: &JobManifest,
    trackers: &[StepTracker],
    step: &Step,
    start_time: &str,
) {
    if use_results {
        let steps: Vec<ResultsStep> = trackers.iter().map(|t| t.to_results_step()).collect();
        let _ = client
            .update_steps(&manifest.plan.plan_id, &manifest.plan.job_id, &steps)
            .await;
    } else {
        let _ = client
            .update_timeline(
                &manifest.plan.plan_id,
                &manifest.plan.timeline_id,
                &[TimelineRecord {
                    id: step.id.clone(),
                    state: Some(TimelineState::InProgress),
                    result: None,
                    start_time: Some(start_time.to_string()),
                    finish_time: None,
                    name: Some(step.display_name.clone()),
                    order: Some(step.order),
                    log: None,
                }],
            )
            .await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn report_step_completed(
    use_results: bool,
    client: &Arc<JobClient>,
    manifest: &JobManifest,
    trackers: &[StepTracker],
    step: &Step,
    start_time: &str,
    finish_time: &str,
    result_conclusion: i32,
    legacy_log_id: u64,
) {
    if use_results {
        let steps: Vec<ResultsStep> = trackers.iter().map(|t| t.to_results_step()).collect();
        let _ = client
            .update_steps(&manifest.plan.plan_id, &manifest.plan.job_id, &steps)
            .await;
    } else {
        let timeline_result = match result_conclusion {
            CONCLUSION_SUCCESS => TimelineResult::Succeeded,
            CONCLUSION_FAILURE => TimelineResult::Failed,
            CONCLUSION_CANCELLED => TimelineResult::Cancelled,
            _ => TimelineResult::Failed,
        };
        let _ = client
            .update_timeline(
                &manifest.plan.plan_id,
                &manifest.plan.timeline_id,
                &[TimelineRecord {
                    id: step.id.clone(),
                    state: Some(TimelineState::Completed),
                    result: Some(timeline_result),
                    start_time: Some(start_time.to_string()),
                    finish_time: Some(finish_time.to_string()),
                    name: Some(step.display_name.clone()),
                    order: Some(step.order),
                    log: Some(TimelineLogRef { id: legacy_log_id }),
                }],
            )
            .await;
    }
}

async fn upload_job_log(
    use_results: bool,
    client: &Arc<JobClient>,
    manifest: &JobManifest,
    buffer: &str,
    line_count: i64,
) {
    if use_results
        && !buffer.is_empty()
        && let Err(e) = client
            .upload_job_log(
                &manifest.plan.plan_id,
                &manifest.plan.job_id,
                buffer,
                line_count,
            )
            .await
    {
        warn!(error = %e, "failed to upload job log");
    }
}

#[cfg(test)]
#[path = "execute_test.rs"]
mod execute_test;
