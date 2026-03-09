use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::JobClient;
use super::action::{ActionCache, load_action_metadata, resolve_action};
use super::client::{JobConclusion, ResultsConclusion, ResultsStatus, ResultsStep};
use super::expression::ExprContext;
use super::logs::{LogSender, StepLogger};
use super::schema::{JobManifest, Step};
use super::timeline::{TimelineLogRef, TimelineRecord, TimelineResult, TimelineState};
use super::workspace::Workspace;
use crate::docker::exec::process_output_line;
use crate::docker::resources::JobDockerResources;
use crate::utils::{format_results_timestamp, format_timeline_timestamp};

pub struct JobState {
    pub env: HashMap<String, String>,
    pub path_prepends: Vec<String>,
    pub outputs: HashMap<String, String>,
    pub masks: Arc<RwLock<Vec<String>>>,
    /// Per-action state for pre→post transfer via SaveState workflow command.
    /// Key: action context_name, Value: map of state name→value.
    pub action_states: HashMap<String, HashMap<String, String>>,
    /// Per-step outputs for `steps.<id>.outputs.<name>` expression resolution.
    pub step_outputs: HashMap<String, HashMap<String, String>>,
    /// Secret variables (name → value) for `secrets.<name>` expression resolution.
    pub secrets: HashMap<String, String>,
    /// Context data from the job manifest (needs, matrix, job, etc.).
    pub context_data: serde_json::Value,
}

impl JobState {
    pub fn new(
        masks: Arc<RwLock<Vec<String>>>,
        secrets: HashMap<String, String>,
        context_data: serde_json::Value,
    ) -> Self {
        Self {
            env: HashMap::new(),
            path_prepends: Vec::new(),
            outputs: HashMap::new(),
            masks,
            action_states: HashMap::new(),
            step_outputs: HashMap::new(),
            secrets,
            context_data,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepConclusion {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug)]
pub struct StepResult {
    pub conclusion: StepConclusion,
}

impl From<StepConclusion> for ResultsConclusion {
    fn from(c: StepConclusion) -> Self {
        match c {
            StepConclusion::Succeeded => Self::Success,
            StepConclusion::Failed => Self::Failure,
            StepConclusion::Cancelled => Self::Cancelled,
        }
    }
}

/// Per-step tracking for Results API updates.
struct StepTracker {
    id: String,
    name: String,
    order: u32,
    status: ResultsStatus,
    conclusion: ResultsConclusion,
    started_at: Option<String>,
    completed_at: Option<String>,
}

impl StepTracker {
    fn from_step(step: &Step) -> Self {
        Self {
            id: step.id.clone(),
            name: step.display_name.clone(),
            order: step.order,
            status: ResultsStatus::Pending,
            conclusion: ResultsConclusion::Unknown,
            started_at: None,
            completed_at: None,
        }
    }

    fn mark_started(&mut self) {
        self.status = ResultsStatus::InProgress;
        self.started_at = Some(format_results_timestamp(Utc::now()));
    }

    fn mark_completed(&mut self, conclusion: ResultsConclusion) {
        self.status = ResultsStatus::Completed;
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
    cancel_token: &CancellationToken,
) -> Result<StepResult> {
    let script_raw = step
        .inputs
        .get("script")
        .context("step has no 'script' input")?;

    let env = build_step_env(step, job_state, workspace, base_env);

    let expr_ctx = ExprContext::new(&env, job_state, false, false);
    let script = super::expression::resolve_template(script_raw, &expr_ctx);

    let script_file = workspace.runner_temp().join(format!("step_{}.sh", step.id));
    std::fs::write(&script_file, &script)
        .with_context(|| format!("writing script file {}", script_file.display()))?;
    let timeout = Duration::from_secs(step.timeout_in_minutes.unwrap_or(360) * 60);

    debug!(
        step_id = %step.id,
        step_name = %step.display_name,
        step_ref = %step.reference.name,
        "running host step"
    );

    let result = run_process(
        "bash",
        &[OsStr::new("-e"), script_file.as_os_str()],
        &env,
        workspace.workspace_dir(),
        job_state,
        log_sender,
        timeout,
        cancel_token,
    )
    .await;

    // Re-key saved state from the empty-key bucket into the correct action-keyed bucket
    if let Some(unnamed_state) = job_state.action_states.remove("") {
        let key = step.context_name.as_deref().unwrap_or(&step.id);
        job_state
            .action_states
            .entry(key.to_string())
            .or_default()
            .extend(unnamed_state);
    }

    let _ = std::fs::remove_file(&script_file);
    result
}

/// Execute a single run: step inside a Docker container via `docker exec`.
pub async fn run_container_step(
    step: &Step,
    job_state: &mut JobState,
    workspace: &Workspace,
    base_env: &HashMap<String, String>,
    log_sender: &LogSender,
    docker_resources: &JobDockerResources,
    cancel_token: &CancellationToken,
) -> Result<StepResult> {
    let script_raw = step
        .inputs
        .get("script")
        .context("step has no 'script' input")?;

    let env = build_step_env(step, job_state, workspace, base_env);

    let expr_ctx = ExprContext::new(&env, job_state, false, false);
    let script = super::expression::resolve_template(script_raw, &expr_ctx);

    let timeout = Duration::from_secs(step.timeout_in_minutes.unwrap_or(360) * 60);
    let container_id = docker_resources
        .job_container_id()
        .context("no job container for container step")?;

    debug!(
        step_id = %step.id,
        step_name = %step.display_name,
        "running container step"
    );

    let result = crate::docker::exec::docker_exec(
        docker_resources.docker(),
        container_id,
        vec!["bash".into(), "-e".into(), "-c".into(), script],
        &env,
        "/github/workspace",
        job_state,
        log_sender,
        timeout,
        cancel_token,
    )
    .await;

    // Re-key saved state from the empty-key bucket into the correct action-keyed bucket
    if let Some(unnamed_state) = job_state.action_states.remove("") {
        let key = step.context_name.as_deref().unwrap_or(&step.id);
        job_state
            .action_states
            .entry(key.to_string())
            .or_default()
            .extend(unnamed_state);
    }

    result
}

/// Shared process runner used by host steps, node actions, and composite steps.
#[allow(clippy::too_many_arguments)]
pub async fn run_process(
    program: &str,
    args: &[&OsStr],
    env: &HashMap<String, String>,
    working_dir: &Path,
    job_state: &mut JobState,
    log_sender: &LogSender,
    timeout: Duration,
    cancel_token: &CancellationToken,
) -> Result<StepResult> {
    let mut child = Command::new(program)
        .args(args)
        .current_dir(working_dir)
        .envs(env)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {program}"))?;

    let stdout = child.stdout.take().context("no stdout")?;
    let stderr = child.stderr.take().context("no stderr")?;

    let collected_env = Arc::new(tokio::sync::Mutex::new(Vec::<(String, String)>::new()));
    let collected_paths = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
    let collected_outputs = Arc::new(tokio::sync::Mutex::new(Vec::<(String, String)>::new()));
    let collected_state = Arc::new(tokio::sync::Mutex::new(Vec::<(String, String)>::new()));

    let stdout_task = spawn_stdout_reader(
        stdout,
        log_sender.clone(),
        job_state.masks.clone(),
        collected_env.clone(),
        collected_paths.clone(),
        collected_outputs.clone(),
        collected_state.clone(),
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

    let status = tokio::select! {
        result = tokio::time::timeout(timeout, timed_wait) => {
            match result {
                Ok(result) => result?,
                Err(_) => {
                    warn!("process timed out, killing");
                    let _ = child.kill().await;
                    return Ok(StepResult {
                        conclusion: StepConclusion::Failed,
                    });
                }
            }
        }
        _ = cancel_token.cancelled() => {
            warn!("job cancelled, killing process");
            let _ = child.kill().await;
            return Ok(StepResult {
                conclusion: StepConclusion::Cancelled,
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
    for (k, v) in collected_state.lock().await.drain(..) {
        job_state
            .action_states
            .entry(String::new())
            .or_default()
            .insert(k, v);
    }

    let conclusion = if status.success() {
        StepConclusion::Succeeded
    } else {
        StepConclusion::Failed
    };

    Ok(StepResult { conclusion })
}

/// Build the full environment for a step execution.
pub fn build_step_env(
    step: &Step,
    job_state: &JobState,
    workspace: &Workspace,
    base_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut env = base_env.clone();
    env.extend(job_state.env.clone());
    if let Some(step_env) = &step.environment {
        for (k, v) in step_env {
            let ctx = ExprContext::new(&env, job_state, false, false);
            let resolved = super::expression::resolve_expression(v, &ctx);
            env.insert(k.clone(), resolved);
        }
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
    state_buf: Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            process_output_line(
                &line,
                &sender,
                &masks,
                &env_buf,
                &path_buf,
                &output_buf,
                &state_buf,
            )
            .await;
        }
    })
}

/// Run all steps in a job manifest.
#[allow(clippy::too_many_arguments)]
pub async fn run_all_steps(
    manifest: &JobManifest,
    job_client: &Arc<JobClient>,
    workspace: &Workspace,
    base_env: &HashMap<String, String>,
    runner_name: &str,
    action_cache: &ActionCache,
    access_token: &str,
    cancel_token: CancellationToken,
    docker_resources: Option<&JobDockerResources>,
    node_path: &Path,
) -> Result<(JobConclusion, HashMap<String, String>)> {
    let masks = collect_secret_masks(manifest);
    let mut secrets: HashMap<String, String> = manifest
        .variables
        .iter()
        .filter(|(_, v)| v.is_secret && !v.value.is_empty())
        .map(|(k, v)| (k.clone(), v.value.clone()))
        .collect();

    // User-defined secrets (repo/org secrets) come via contextData["secrets"],
    // not through the variables dict which only has system-level secrets.
    if let Some(ctx_secrets) = manifest
        .context_data
        .get("secrets")
        .and_then(|v| v.as_object())
    {
        for (k, v) in ctx_secrets {
            if let Some(s) = v.as_str()
                && !s.is_empty()
            {
                // Add to mask list so secret values are redacted in logs
                masks.write().await.push(s.to_string());
                secrets.insert(k.clone(), s.to_string());
            }
        }
    }

    let mut job_state = JobState::new(masks.clone(), secrets, manifest.context_data.clone());
    let mut job_failed = false;
    let mut job_cancelled = false;
    let use_results = job_client.has_results_url();

    let mut trackers: Vec<StepTracker> =
        manifest.steps.iter().map(StepTracker::from_step).collect();

    let mut job_log_buffer = String::new();
    let mut job_line_count: i64 = 0;

    for (idx, step) in manifest.steps.iter().enumerate() {
        // Check for cancellation between steps
        if cancel_token.is_cancelled() {
            job_cancelled = true;
        }

        if let Some(condition) = &step.condition {
            debug!(step = %step.display_name, condition, "step has condition");
        }

        // Check condition before starting the step — skipped steps get no
        // logs and are reported as completed immediately.
        let condition_ctx = ExprContext::new(base_env, &job_state, job_failed, job_cancelled);
        if !super::expression::evaluate_condition(step.condition.as_deref(), &condition_ctx) {
            debug!(step = %step.display_name, "skipping step (condition not met)");
            let now = format_timeline_timestamp(Utc::now());
            trackers[idx].mark_started();
            trackers[idx].mark_completed(ResultsConclusion::Skipped);

            report_step_completed(
                use_results,
                job_client,
                manifest,
                &trackers,
                step,
                &now,
                &now,
                ResultsConclusion::Skipped,
                0,
            )
            .await;
            continue;
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
            logger.sender(),
            runner_name,
            action_cache,
            access_token,
            &cancel_token,
            docker_resources,
            node_path,
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

        // Save this step's outputs for `steps.<id>.outputs.<name>` resolution
        if !job_state.outputs.is_empty() {
            let step_key = step.context_name.as_deref().unwrap_or(&step.id);
            job_state
                .step_outputs
                .insert(step_key.to_string(), std::mem::take(&mut job_state.outputs));
        }

        if conclusion == StepConclusion::Cancelled {
            job_cancelled = true;
        } else if conclusion == StepConclusion::Failed {
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

    let conclusion = if job_cancelled {
        JobConclusion::Cancelled
    } else if job_failed {
        JobConclusion::Failed
    } else {
        JobConclusion::Succeeded
    };
    Ok((conclusion, job_state.outputs.clone()))
}

fn collect_secret_masks(manifest: &JobManifest) -> Arc<RwLock<Vec<String>>> {
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

/// Run a step (already determined to not be skipped).
#[allow(clippy::too_many_arguments)]
async fn execute_step(
    step: &Step,
    job_state: &mut JobState,
    workspace: &Workspace,
    base_env: &HashMap<String, String>,
    log_sender: &LogSender,
    runner_name: &str,
    action_cache: &ActionCache,
    access_token: &str,
    cancel_token: &CancellationToken,
    docker_resources: Option<&JobDockerResources>,
    node_path: &Path,
) -> (StepConclusion, ResultsConclusion) {
    log_sender.send_banner(runner_name).await;

    let has_docker = docker_resources
        .as_ref()
        .and_then(|r| r.job_container_id())
        .is_some();
    debug!(
        step = %step.display_name,
        is_script = step.is_script(),
        has_docker,
        "executing step"
    );

    let result = if step.is_script() {
        // Script steps: container mode if docker_resources has a job container, else host
        if let Some(resources) = docker_resources.filter(|r| r.job_container_id().is_some()) {
            run_container_step(
                step,
                job_state,
                workspace,
                base_env,
                log_sender,
                resources,
                cancel_token,
            )
            .await
        } else {
            run_host_step(
                step,
                job_state,
                workspace,
                base_env,
                log_sender,
                cancel_token,
            )
            .await
        }
    } else {
        run_action_step(
            step,
            job_state,
            workspace,
            base_env,
            log_sender,
            action_cache,
            access_token,
            cancel_token,
            docker_resources,
            node_path,
        )
        .await
    };

    match result {
        Ok(result) => {
            let rc = ResultsConclusion::from(result.conclusion);
            (result.conclusion, rc)
        }
        Err(e) => {
            log_sender.send(format!("Step error: {e}")).await;
            (StepConclusion::Failed, ResultsConclusion::Failure)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_action_step(
    step: &Step,
    job_state: &mut JobState,
    workspace: &Workspace,
    base_env: &HashMap<String, String>,
    log_sender: &LogSender,
    action_cache: &ActionCache,
    access_token: &str,
    cancel_token: &CancellationToken,
    docker_resources: Option<&JobDockerResources>,
    node_path: &Path,
) -> Result<StepResult> {
    let source = resolve_action(step)?;
    let action_dir = action_cache
        .get_action(&source, workspace.workspace_dir(), access_token)
        .await?;
    let metadata = load_action_metadata(&action_dir)?;

    if metadata.runs.is_node() {
        let entry_point = detect_entry_point(step);
        super::action::node::run_node_action(
            &action_dir,
            &metadata,
            entry_point,
            step,
            job_state,
            workspace,
            base_env,
            log_sender,
            cancel_token,
            docker_resources,
            node_path,
        )
        .await
    } else if metadata.runs.is_composite() {
        super::action::composite::run_composite_action(
            &action_dir,
            &metadata,
            step,
            job_state,
            workspace,
            base_env,
            log_sender,
            action_cache,
            access_token,
            0,
            cancel_token,
            docker_resources,
            node_path,
        )
        .await
    } else if metadata.runs.is_docker() {
        anyhow::bail!("Docker actions not supported yet (Phase 3)")
    } else {
        anyhow::bail!("unsupported action runtime: {}", metadata.runs.using)
    }
}

/// Detect whether this is a pre, main, or post step based on context_name.
fn detect_entry_point(step: &Step) -> &str {
    if let Some(ctx) = &step.context_name {
        if ctx.ends_with("_pre") || ctx.contains("_pre_") {
            return "pre";
        }
        if ctx.ends_with("_post") || ctx.contains("_post_") {
            return "post";
        }
    }
    "main"
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
    result_conclusion: ResultsConclusion,
    legacy_log_id: u64,
) {
    if use_results {
        let steps: Vec<ResultsStep> = trackers.iter().map(|t| t.to_results_step()).collect();
        let _ = client
            .update_steps(&manifest.plan.plan_id, &manifest.plan.job_id, &steps)
            .await;
    } else {
        let timeline_result = match result_conclusion {
            ResultsConclusion::Success => TimelineResult::Succeeded,
            ResultsConclusion::Failure => TimelineResult::Failed,
            ResultsConclusion::Cancelled => TimelineResult::Cancelled,
            ResultsConclusion::Skipped => TimelineResult::Skipped,
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
