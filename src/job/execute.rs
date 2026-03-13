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
use super::live_feed::FeedSender;
use super::logs::{LogSender, StepLogger};
use super::schema::{JobManifest, Step};
use super::timeline::{TimelineLogRef, TimelineRecord, TimelineResult, TimelineState};
use super::workspace::Workspace;
use crate::docker::exec::process_output_line;
use crate::docker::resources::JobDockerResources;
use crate::utils::{format_results_timestamp, format_timeline_timestamp};

/// Per-step result for `steps.<id>.outcome` and `steps.<id>.conclusion`.
///
/// `outcome` is the raw result before `continue-on-error` is applied.
/// `conclusion` is the final result after `continue-on-error`:
/// if `continue-on-error: true` and the step failed, outcome="failure" but conclusion="success".
#[derive(Debug, Clone)]
pub struct StepOutcome {
    pub outcome: String,
    pub conclusion: String,
}

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
    /// Per-step outcome/conclusion for `steps.<id>.outcome` / `steps.<id>.conclusion`.
    pub step_outcomes: HashMap<String, StepOutcome>,
    /// Secret variables (name → value) for `secrets.<name>` expression resolution.
    pub secrets: HashMap<String, String>,
    /// Context data from the job manifest (needs, matrix, job, etc.).
    pub context_data: serde_json::Value,
    /// Host filesystem workspace path for hashFiles(). In container mode, GITHUB_WORKSPACE
    /// points to the container path (/github/workspace) but file operations need the real path.
    pub host_workspace: Option<String>,
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
            step_outcomes: HashMap::new(),
            secrets,
            context_data,
            host_workspace: None,
        }
    }
}

/// Build the `job` context object for expression evaluation.
/// Contains `status`, and optionally `container` and `services` when Docker is in use.
fn build_job_context(docker_resources: Option<&JobDockerResources>) -> serde_json::Value {
    let mut job = serde_json::json!({
        "status": "success"
    });

    if let Some(resources) = docker_resources {
        if let Some(container_id) = resources.job_container_id() {
            let mut container = serde_json::json!({
                "id": container_id
            });
            if let Some(network) = resources.network_name() {
                container["network"] = serde_json::json!(network);
            }
            job["container"] = container;
        }

        let svc_map = resources.service_container_map();
        if !svc_map.is_empty() {
            let mut services = serde_json::Map::new();
            for (alias, container_id) in svc_map {
                let mut svc = serde_json::json!({
                    "id": container_id
                });
                if let Some(network) = resources.network_name() {
                    svc["network"] = serde_json::json!(network);
                }
                if let Some(ports) = resources.service_ports().get(alias) {
                    svc["ports"] = serde_json::json!(ports);
                }
                services.insert(alias.clone(), svc);
            }
            job["services"] = serde_json::Value::Object(services);
        }
    }

    job
}

/// Update `context_data["job"]["status"]` based on current job state.
fn update_job_status(context_data: &mut serde_json::Value, failed: bool, cancelled: bool) {
    let status = if cancelled {
        "cancelled"
    } else if failed {
        "failure"
    } else {
        "success"
    };

    if let Some(job) = context_data.get_mut("job") {
        job["status"] = serde_json::json!(status);
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepConclusion {
    Succeeded,
    Failed,
    Cancelled,
}

impl StepConclusion {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "success",
            Self::Failed => "failure",
            Self::Cancelled => "cancelled",
        }
    }
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

    // Inject STATE_* env vars for pre/post steps so @actions/core.getState() works.
    // The main step writes state to GITHUB_STATE file, we read it into action_states,
    // and the post step reads it via STATE_<name> env vars.
    if let Some(ctx_name) = &step.context_name
        && let Some(base_ctx) = ctx_name
            .strip_suffix("_post")
            .or_else(|| ctx_name.strip_suffix("_pre"))
        && let Some(states) = job_state.action_states.get(base_ctx)
    {
        for (k, v) in states {
            env.insert(format!("STATE_{k}"), v.clone());
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
    feed_sender: Option<&FeedSender>,
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

    // Populate the `job` context for expression evaluation
    if let serde_json::Value::Object(ref mut map) = job_state.context_data {
        map.insert("job".to_string(), build_job_context(docker_resources));
    }

    // In container mode, GITHUB_WORKSPACE points to /github/workspace (container path).
    // hashFiles() runs on the host and needs the real filesystem path.
    if docker_resources
        .as_ref()
        .and_then(|r| r.job_container_id())
        .is_some()
    {
        job_state.host_workspace = Some(workspace.workspace_dir().to_string_lossy().into_owned());
    }

    let mut job_failed = false;
    let mut job_cancelled = false;
    let use_results = job_client.has_results_url();

    let mut trackers: Vec<StepTracker> =
        manifest.steps.iter().map(StepTracker::from_step).collect();

    let mut job_log_buffer = String::new();
    let mut job_line_count: i64 = 0;
    let mut pending_post_steps: Vec<(Step, Option<String>)> = Vec::new();

    // --- Pre steps ---
    // Actions can define a `pre` entry point in their action.yml (e.g.
    // actions/checkout pre-checks the repository state). Pre steps run
    // before all main steps, in forward order (matching manifest order).
    let mut pre_steps = Vec::new();
    for (idx, step) in manifest.steps.iter().enumerate() {
        if let Some((orig_step, pre_if)) =
            collect_pre_step(step, action_cache, workspace.workspace_dir(), access_token).await
        {
            let ctx_name = orig_step
                .context_name
                .clone()
                .unwrap_or_else(|| orig_step.id.clone());
            pre_steps.push(Step {
                id: uuid::Uuid::new_v4().to_string(),
                display_name: format!("Pre {}", orig_step.display_name),
                context_name: Some(format!("{ctx_name}_pre")),
                order: idx as u32,
                condition: Some(pre_if.unwrap_or_else(|| "always()".to_string())),
                ..orig_step
            });
        }
    }

    let pre_step_count = pre_steps.len();
    if !pre_steps.is_empty() {
        // Prepend pre step trackers so they appear before main steps in the UI
        let mut pre_trackers: Vec<StepTracker> =
            pre_steps.iter().map(StepTracker::from_step).collect();
        pre_trackers.append(&mut trackers);
        trackers = pre_trackers;

        for pre_step in &pre_steps {
            if cancel_token.is_cancelled() {
                job_cancelled = true;
            }

            let tracker_idx = trackers
                .iter()
                .position(|t| t.id == pre_step.id)
                .context("pre step tracker not found")?;

            update_job_status(&mut job_state.context_data, job_failed, job_cancelled);
            let condition_ctx = ExprContext::new(base_env, &job_state, job_failed, job_cancelled);
            if !super::expression::evaluate_condition(pre_step.condition.as_deref(), &condition_ctx)
            {
                debug!(step = %pre_step.display_name, "skipping pre step (condition not met)");
                let now = format_timeline_timestamp(Utc::now());
                trackers[tracker_idx].mark_started();
                trackers[tracker_idx].mark_completed(ResultsConclusion::Skipped);
                report_step_completed(
                    use_results,
                    job_client,
                    manifest,
                    &trackers,
                    pre_step,
                    &now,
                    &now,
                    ResultsConclusion::Skipped,
                    0,
                )
                .await;
                continue;
            }

            let start_time = format_timeline_timestamp(Utc::now());
            trackers[tracker_idx].mark_started();
            report_step_started(
                use_results,
                job_client,
                manifest,
                &trackers,
                pre_step,
                &start_time,
            )
            .await;

            let logger = create_step_logger(
                use_results,
                job_client,
                &manifest.plan.plan_id,
                &manifest.plan.job_id,
                &pre_step.id,
                &pre_step.display_name,
                masks.clone(),
                feed_sender,
            )
            .await;

            let (conclusion, result_conclusion) = execute_step(
                pre_step,
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
            if let Some(collected) = logger.finish().await {
                if !use_results {
                    job_log_buffer.push_str(&collected.text);
                }
                job_line_count += collected.line_count;
            }

            let finish_time = format_timeline_timestamp(Utc::now());
            trackers[tracker_idx].mark_completed(result_conclusion);
            report_step_completed(
                use_results,
                job_client,
                manifest,
                &trackers,
                pre_step,
                &start_time,
                &finish_time,
                result_conclusion,
                legacy_log_id,
            )
            .await;

            // Read file-based outputs/env/path/state after pre steps
            if let Ok(file_outputs) = workspace.read_output_file() {
                job_state.outputs.extend(file_outputs);
            }
            if let Ok(file_env) = workspace.read_env_file() {
                job_state.env.extend(file_env);
            }
            if let Ok(extra_paths) = workspace.read_path_file() {
                job_state.path_prepends.extend(extra_paths);
            }
            if let Ok(file_state) = workspace.read_state_file() {
                let key = pre_step.context_name.as_deref().unwrap_or(&pre_step.id);
                job_state
                    .action_states
                    .entry(key.to_string())
                    .or_default()
                    .extend(file_state);
            }
            workspace.clear_step_files();

            // Pre steps DO affect job conclusion (unlike post steps)
            if conclusion == StepConclusion::Cancelled {
                job_cancelled = true;
            } else if conclusion == StepConclusion::Failed {
                if pre_step.continue_on_error {
                    info!(step = %pre_step.display_name, "pre step failed but continue_on_error is set");
                } else {
                    job_failed = true;
                }
            }
        }
    }

    for (idx, step) in manifest.steps.iter().enumerate() {
        let idx = idx + pre_step_count;
        // Check for cancellation between steps
        if cancel_token.is_cancelled() {
            job_cancelled = true;
        }

        if let Some(condition) = &step.condition {
            debug!(step = %step.display_name, condition, "step has condition");
        }

        // Check condition before starting the step — skipped steps get no
        // logs and are reported as completed immediately.
        update_job_status(&mut job_state.context_data, job_failed, job_cancelled);
        let condition_ctx = ExprContext::new(base_env, &job_state, job_failed, job_cancelled);
        if !super::expression::evaluate_condition(step.condition.as_deref(), &condition_ctx) {
            debug!(step = %step.display_name, "skipping step (condition not met)");
            let now = format_timeline_timestamp(Utc::now());
            trackers[idx].mark_started();
            trackers[idx].mark_completed(ResultsConclusion::Skipped);

            if let Some(ctx_name) = step.context_name.as_deref() {
                job_state.step_outcomes.insert(
                    ctx_name.to_string(),
                    StepOutcome {
                        outcome: "skipped".into(),
                        conclusion: "skipped".into(),
                    },
                );
            }

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
            &manifest.plan.job_id,
            &step.id,
            &step.display_name,
            masks.clone(),
            feed_sender,
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

        if let Some(collected) = logger.finish().await {
            // In Results mode, step logs are already uploaded to the blob — only
            // accumulate text for the job-level log in legacy mode.
            if !use_results {
                job_log_buffer.push_str(&collected.text);
            }
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

        // Read file-based outputs/env/path/state after each step.
        // Modern actions use these files instead of legacy :: workflow commands.
        if let Ok(file_outputs) = workspace.read_output_file() {
            job_state.outputs.extend(file_outputs);
        }
        if let Ok(file_env) = workspace.read_env_file() {
            job_state.env.extend(file_env);
        }
        if let Ok(extra_paths) = workspace.read_path_file() {
            job_state.path_prepends.extend(extra_paths);
        }
        // Read GITHUB_STATE file (used by @actions/core saveState)
        if let Ok(file_state) = workspace.read_state_file() {
            let key = step.context_name.as_deref().unwrap_or(&step.id);
            job_state
                .action_states
                .entry(key.to_string())
                .or_default()
                .extend(file_state);
        }
        // Clear the files so the next step starts fresh
        workspace.clear_step_files();

        // If this action has a `post` entry point, schedule it for later
        if let Some(post_info) =
            collect_post_step(step, action_cache, workspace.workspace_dir(), access_token).await
        {
            pending_post_steps.push(post_info);
        }

        // Save this step's outputs for `steps.<id>.outputs.<name>` resolution.
        // Always take() to prevent outputs from bleeding into subsequent steps.
        let step_key = step.context_name.as_deref().unwrap_or(&step.id);
        let step_outs = std::mem::take(&mut job_state.outputs);
        if !step_outs.is_empty() {
            job_state
                .step_outputs
                .insert(step_key.to_string(), step_outs);
        }

        // Save outcome/conclusion for `steps.<id>.outcome` / `steps.<id>.conclusion`
        if let Some(ctx_name) = step.context_name.as_deref() {
            let effective_conclusion =
                if step.continue_on_error && conclusion == StepConclusion::Failed {
                    StepConclusion::Succeeded
                } else {
                    conclusion
                };
            job_state.step_outcomes.insert(
                ctx_name.to_string(),
                StepOutcome {
                    outcome: conclusion.as_str().to_string(),
                    conclusion: effective_conclusion.as_str().to_string(),
                },
            );
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

    // --- Post steps ---
    // Actions can define a `post` entry point in their action.yml (e.g.
    // actions/cache saves the cache in its post step). The official runner
    // generates these dynamically; chimera does the same here.
    // Post steps run in reverse order (last action's post runs first).
    if !pending_post_steps.is_empty() {
        let max_order = manifest.steps.iter().map(|s| s.order).max().unwrap_or(0);
        let mut post_steps = Vec::new();
        for (rev_idx, (orig_step, post_if)) in pending_post_steps.into_iter().rev().enumerate() {
            let ctx_name = orig_step
                .context_name
                .clone()
                .unwrap_or_else(|| orig_step.id.clone());
            post_steps.push(Step {
                id: uuid::Uuid::new_v4().to_string(),
                display_name: format!("Post {}", orig_step.display_name),
                context_name: Some(format!("{ctx_name}_post")),
                order: max_order + 1 + rev_idx as u32,
                // Default post-if is always() (not success()), so post steps
                // run even when the job failed (e.g. to save partial caches).
                condition: Some(post_if.unwrap_or_else(|| "always()".to_string())),
                ..orig_step
            });
        }

        for ps in &post_steps {
            trackers.push(StepTracker::from_step(ps));
        }

        for post_step in &post_steps {
            let tracker_idx = trackers
                .iter()
                .position(|t| t.id == post_step.id)
                .context("post step tracker not found")?;

            update_job_status(&mut job_state.context_data, job_failed, job_cancelled);
            let condition_ctx = ExprContext::new(base_env, &job_state, job_failed, job_cancelled);
            if !super::expression::evaluate_condition(
                post_step.condition.as_deref(),
                &condition_ctx,
            ) {
                debug!(step = %post_step.display_name, "skipping post step (condition not met)");
                let now = format_timeline_timestamp(Utc::now());
                trackers[tracker_idx].mark_started();
                trackers[tracker_idx].mark_completed(ResultsConclusion::Skipped);
                report_step_completed(
                    use_results,
                    job_client,
                    manifest,
                    &trackers,
                    post_step,
                    &now,
                    &now,
                    ResultsConclusion::Skipped,
                    0,
                )
                .await;
                continue;
            }

            let start_time = format_timeline_timestamp(Utc::now());
            trackers[tracker_idx].mark_started();
            report_step_started(
                use_results,
                job_client,
                manifest,
                &trackers,
                post_step,
                &start_time,
            )
            .await;

            let logger = create_step_logger(
                use_results,
                job_client,
                &manifest.plan.plan_id,
                &manifest.plan.job_id,
                &post_step.id,
                &post_step.display_name,
                masks.clone(),
                feed_sender,
            )
            .await;

            let (conclusion, result_conclusion) = execute_step(
                post_step,
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
            if let Some(collected) = logger.finish().await {
                if !use_results {
                    job_log_buffer.push_str(&collected.text);
                }
                job_line_count += collected.line_count;
            }

            let finish_time = format_timeline_timestamp(Utc::now());
            trackers[tracker_idx].mark_completed(result_conclusion);
            report_step_completed(
                use_results,
                job_client,
                manifest,
                &trackers,
                post_step,
                &start_time,
                &finish_time,
                result_conclusion,
                legacy_log_id,
            )
            .await;

            // Read file-based outputs/env/path/state after post steps too
            if let Ok(file_outputs) = workspace.read_output_file() {
                job_state.outputs.extend(file_outputs);
            }
            if let Ok(file_env) = workspace.read_env_file() {
                job_state.env.extend(file_env);
            }
            if let Ok(extra_paths) = workspace.read_path_file() {
                job_state.path_prepends.extend(extra_paths);
            }
            if let Ok(file_state) = workspace.read_state_file() {
                let key = post_step.context_name.as_deref().unwrap_or(&post_step.id);
                job_state
                    .action_states
                    .entry(key.to_string())
                    .or_default()
                    .extend(file_state);
            }
            workspace.clear_step_files();

            // Post steps don't affect job conclusion
            if conclusion == StepConclusion::Failed {
                info!(step = %post_step.display_name, "post step failed (does not affect job conclusion)");
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

    // Reconstruct job-level outputs from all step outputs.
    // The server uses these for `needs.X.outputs.Y` resolution in dependent jobs.
    for step in &manifest.steps {
        let key = step.context_name.as_deref().unwrap_or(&step.id);
        if let Some(outs) = job_state.step_outputs.get(key) {
            job_state.outputs.extend(outs.clone());
        }
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

#[allow(clippy::too_many_arguments)]
async fn create_step_logger(
    use_results: bool,
    client: &Arc<JobClient>,
    plan_id: &str,
    job_id: &str,
    step_id: &str,
    step_name: &str,
    masks: Arc<RwLock<Vec<String>>>,
    feed_sender: Option<&FeedSender>,
) -> StepLogger {
    let feed = feed_sender.map(|f| (f.clone(), step_id.to_string()));
    if use_results {
        StepLogger::results(
            client.clone(),
            plan_id.to_string(),
            job_id.to_string(),
            step_id.to_string(),
            masks,
            feed,
        )
    } else {
        StepLogger::legacy(client.clone(), plan_id, step_name, masks, feed).await
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
    let has_docker = docker_resources
        .as_ref()
        .and_then(|r| r.job_container_id())
        .is_some();

    log_sender.send_banner(runner_name, has_docker).await;
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
    use super::action::resolve::ActionSource;

    let source = resolve_action(step)?;

    // Case 1: inline docker://image — skip metadata, run directly
    if let ActionSource::Docker { ref image } = source {
        return super::action::docker::run_docker_image_action(
            image,
            step,
            job_state,
            workspace,
            base_env,
            log_sender,
            cancel_token,
            docker_resources,
        )
        .await;
    }

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
        let entry_point = detect_entry_point(step);
        super::action::docker::run_docker_metadata_action(
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
        )
        .await
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

/// Check if an action step has a `post` entry point, returning the cloned
/// step and its `post-if` condition for deferred execution.
async fn collect_post_step(
    step: &Step,
    action_cache: &ActionCache,
    workspace_dir: &Path,
    access_token: &str,
) -> Option<(Step, Option<String>)> {
    if step.is_script() {
        return None;
    }

    use super::action::resolve::ActionSource;
    let source = resolve_action(step).ok()?;

    // Inline docker://image actions don't have action.yml metadata
    if matches!(source, ActionSource::Docker { .. }) {
        return None;
    }

    let action_dir = action_cache
        .get_action(&source, workspace_dir, access_token)
        .await
        .ok()?;
    let metadata = load_action_metadata(&action_dir).ok()?;

    let has_post = metadata.runs.post.is_some() || metadata.runs.post_entrypoint.is_some();
    if has_post {
        Some((step.clone(), metadata.runs.post_if.clone()))
    } else {
        None
    }
}

/// Check if an action step has a `pre` entry point, returning the cloned
/// step and its `pre-if` condition for execution before the main steps.
async fn collect_pre_step(
    step: &Step,
    action_cache: &ActionCache,
    workspace_dir: &Path,
    access_token: &str,
) -> Option<(Step, Option<String>)> {
    if step.is_script() {
        return None;
    }

    use super::action::resolve::ActionSource;
    let source = resolve_action(step).ok()?;

    if matches!(source, ActionSource::Docker { .. }) {
        return None;
    }

    let action_dir = action_cache
        .get_action(&source, workspace_dir, access_token)
        .await
        .ok()?;
    let metadata = load_action_metadata(&action_dir).ok()?;

    let has_pre = metadata.runs.pre.is_some() || metadata.runs.pre_entrypoint.is_some();
    if has_pre {
        Some((step.clone(), metadata.runs.pre_if.clone()))
    } else {
        None
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
