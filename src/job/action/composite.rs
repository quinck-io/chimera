use std::collections::HashMap;
use std::ffi::OsStr;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use super::ActionStage;
use super::download::ActionCache;
use super::metadata::{ActionMetadata, load_action_metadata};
use super::resolve::ActionSource;
use crate::docker::resources::JobDockerResources;
use crate::job::execute::{
    JobState, JobStatus, StepConclusion, StepOutcome, StepResult, build_step_env, run_process,
    state_key, write_script_file,
};
use crate::job::expression::{
    ExprContext, action_step_condition, evaluate_condition, resolve_template,
};
use crate::job::logs::LogSender;
use crate::job::schema::{Evaluable, Step, StepReference, StepReferenceKind};
use crate::job::shell::Shell;
use crate::job::workspace::Workspace;
use crate::node::NodeRuntimes;

use super::build_action_inputs;

const MAX_COMPOSITE_DEPTH: u32 = 10;

type StepFuture<'a> = Pin<Box<dyn Future<Output = Result<StepResult>> + Send + 'a>>;

/// What every step of a composite action runs with, however deeply it is nested.
pub struct CompositeContext<'a> {
    pub workspace: &'a Workspace,
    pub log_sender: &'a LogSender,
    pub action_cache: &'a ActionCache,
    pub access_token: &'a str,
    pub docker_resources: Option<&'a JobDockerResources>,
    pub node_runtimes: &'a NodeRuntimes,
    pub job: &'a JobStatus,
}

/// The `post` of an action used inside a composite action. As on the official
/// runner, it is registered when its main step runs, and runs as part of the
/// composite action's own post step.
#[derive(Debug)]
pub struct EmbeddedPost {
    step: Step,
    action_dir: PathBuf,
    condition: String,
    env: HashMap<String, String>,
}

impl EmbeddedPost {
    /// Captured while the main step runs, so the post step gets the same inputs
    /// even though the composite action's `steps` context is gone by then.
    fn new(
        main_step: &Step,
        action_dir: &Path,
        condition: String,
        env: &HashMap<String, String>,
        job_state: &JobState,
    ) -> Self {
        let ctx = ExprContext::new(env, job_state, false, false);
        let mut step = main_step.clone();
        step.inputs = main_step
            .inputs
            .iter()
            .map(|(name, value)| (name.clone(), resolve_template(value, &ctx)))
            .collect();
        Self {
            step,
            action_dir: action_dir.to_path_buf(),
            condition,
            env: env.clone(),
        }
    }

    /// Post steps run after the action is done, so like the official runner they
    /// test their `post-if` against the job's status rather than the action's.
    fn condition_holds(&self, job_state: &JobState, job_failed: bool, job_cancelled: bool) -> bool {
        let ctx = ExprContext::new(&self.env, job_state, job_failed, job_cancelled);
        evaluate_condition(Some(&self.condition), &ctx)
    }
}

fn ykey(s: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(s.into())
}

/// GitHub reads `with:` and `env:` values as strings, so an unquoted `true` or `3`
/// must arrive as `"true"` or `"3"` instead of being dropped.
fn scalar_to_string(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub fn run_composite_action<'a>(
    ctx: &'a CompositeContext<'a>,
    action_dir: &'a Path,
    metadata: &'a ActionMetadata,
    step: &'a Step,
    job_state: &'a mut JobState,
    base_env: &'a HashMap<String, String>,
    depth: u32,
) -> StepFuture<'a> {
    Box::pin(run_composite_action_inner(
        ctx, action_dir, metadata, step, job_state, base_env, depth,
    ))
}

async fn run_composite_action_inner(
    ctx: &CompositeContext<'_>,
    action_dir: &Path,
    metadata: &ActionMetadata,
    step: &Step,
    job_state: &mut JobState,
    base_env: &HashMap<String, String>,
    depth: u32,
) -> Result<StepResult> {
    if depth >= MAX_COMPOSITE_DEPTH {
        bail!("composite action recursion depth limit ({MAX_COMPOSITE_DEPTH}) exceeded");
    }

    let steps = metadata
        .runs
        .steps
        .as_ref()
        .context("composite action has no steps")?;
    let sub_steps = parse_sub_steps(steps, state_key(step))?;
    let scope = ActionScope::new(ctx, action_dir, metadata, step, job_state, base_env)?;

    // Like on the official runner, a composite action has its own `steps` context:
    // sub-steps see only each other, and the caller sees only the action's
    // `outputs:`.
    let caller_step_outputs = std::mem::take(&mut job_state.step_outputs);
    let caller_step_outcomes = std::mem::take(&mut job_state.step_outcomes);

    let conclusion = run_sub_steps(ctx, &scope, &sub_steps, job_state, depth).await;

    let output_env = scope.env(job_state, ctx.workspace);
    job_state.outputs = evaluate_action_outputs(metadata, &output_env, job_state);
    job_state.step_outputs = caller_step_outputs;
    job_state.step_outcomes = caller_step_outcomes;

    Ok(StepResult { conclusion })
}

/// Run the action's steps in order. Like the official runner, a failure does not
/// end the action: each later step's `if:` is tested against the action's status,
/// so `always()` and `failure()` steps still run, and every step's result is
/// folded into the action's.
async fn run_sub_steps(
    ctx: &CompositeContext<'_>,
    scope: &ActionScope<'_>,
    sub_steps: &[SubStep],
    job_state: &mut JobState,
    depth: u32,
) -> StepConclusion {
    let mut status = StepConclusion::Succeeded;
    for sub_step in sub_steps {
        // Rebuilt for every sub-step, so env and PATH changes from earlier ones apply.
        let env = scope.env(job_state, ctx.workspace);
        let job_cancelled = ctx.job.is_cancelled();

        if !sub_step.condition_holds(&env, job_state, job_cancelled, status) {
            debug!(step = %sub_step.key, condition = %sub_step.condition, "skipping composite sub-step");
            if let Some(id) = &sub_step.id {
                job_state
                    .step_outcomes
                    .insert(id.clone(), skipped_outcome());
            }
            continue;
        }

        let cancel_token = sub_step_token(ctx.job, job_cancelled, || {
            sub_step.condition_holds(&env, job_state, true, StepConclusion::Cancelled)
        });
        let conclusion =
            run_sub_step(ctx, scope, sub_step, job_state, &env, &cancel_token, depth).await;

        let outputs = take_sub_step_file_commands(job_state, ctx.workspace, &sub_step.key);
        if let Some(id) = &sub_step.id {
            record_sub_step(
                job_state,
                id,
                outputs,
                conclusion,
                sub_step.continue_on_error,
            );
        }
        status = status.merge(conclusion.after_continue_on_error(sub_step.continue_on_error));
    }
    status
}

/// The token a sub-step runs under. As on the official runner, a cancel requested
/// while the step runs tests its `if:` again, as if the action were cancelled, and
/// stops the step only if that no longer holds. A step that starts after the cancel
/// is not tested again, so only forcing the job stops it.
fn sub_step_token(
    job: &JobStatus,
    job_cancelled: bool,
    survives_cancel: impl FnOnce() -> bool,
) -> CancellationToken {
    if job_cancelled {
        return job.cancel.step_token(true);
    }
    job.cancel.step_token(survives_cancel())
}

async fn run_sub_step(
    ctx: &CompositeContext<'_>,
    scope: &ActionScope<'_>,
    sub_step: &SubStep,
    job_state: &mut JobState,
    env: &HashMap<String, String>,
    cancel_token: &CancellationToken,
    depth: u32,
) -> StepConclusion {
    debug!(step = %sub_step.key, "running composite sub-step");
    let own_env = sub_step.own_env(env, job_state);
    let timeout = scope.step.timeout();
    let result = match &sub_step.kind {
        SubStepKind::Uses(uses) => {
            let nested_step = nested_action_step(sub_step, uses, own_env, timeout);
            run_nested_action(
                ctx,
                scope,
                &nested_step,
                job_state,
                env,
                cancel_token,
                depth,
            )
            .await
        }
        SubStepKind::Run(script) => {
            let mut step_env = env.clone();
            step_env.extend(own_env);
            run_nested_script(
                ctx,
                sub_step,
                script,
                job_state,
                &step_env,
                timeout,
                cancel_token,
            )
            .await
        }
    };
    conclusion_of(result, ctx.log_sender).await
}

/// Like the official runner, an error in one step fails that step rather than the
/// whole action, so the steps after it still get to run.
async fn conclusion_of(result: Result<StepResult>, log_sender: &LogSender) -> StepConclusion {
    match result {
        Ok(result) => result.conclusion,
        Err(e) => {
            log_sender.send(format!("Error: {e:#}")).await;
            StepConclusion::Failed
        }
    }
}

/// What all steps of one composite action share.
struct ActionScope<'a> {
    step: &'a Step,
    base_env: &'a HashMap<String, String>,
    inputs: HashMap<String, String>,
    action_path: String,
}

impl<'a> ActionScope<'a> {
    /// Inputs are resolved once, against the caller's context, before the action's
    /// own `steps` context replaces it.
    fn new(
        ctx: &CompositeContext<'_>,
        action_dir: &Path,
        metadata: &ActionMetadata,
        step: &'a Step,
        job_state: &JobState,
        base_env: &'a HashMap<String, String>,
    ) -> Result<Self> {
        let action_path = ctx
            .docker_resources
            .filter(|r| r.job_container_id().is_some())
            .map(|r| {
                r.remap_to_container_path(action_dir)
                    .context("cannot remap action dir to container path")
            })
            .transpose()?
            .unwrap_or_else(|| action_dir.display().to_string());

        let mut scope = Self {
            step,
            base_env,
            inputs: HashMap::new(),
            action_path,
        };
        let env = scope.env(job_state, ctx.workspace);
        scope.inputs = build_action_inputs(
            metadata,
            step,
            &ExprContext::new(&env, job_state, false, false),
        );
        Ok(scope)
    }

    fn env(&self, job_state: &JobState, workspace: &Workspace) -> HashMap<String, String> {
        let mut env = build_step_env(self.step, job_state, workspace, self.base_env);
        env.extend(self.inputs.clone());
        env.insert("GITHUB_ACTION_PATH".into(), self.action_path.clone());
        env
    }

    fn key(&self) -> &str {
        state_key(self.step)
    }
}

enum SubStepKind {
    Run(String),
    Uses(String),
}

/// One step of a composite action, as its `action.yml` writes it.
struct SubStep {
    map: serde_yaml::Mapping,
    kind: SubStepKind,
    id: Option<String>,
    /// Keys the step's saved state and post step: `<action key>.<id>`, like the
    /// official runner's qualified context name.
    key: String,
    condition: String,
    continue_on_error: bool,
}

fn parse_sub_steps(steps: &[serde_yaml::Value], action_key: &str) -> Result<Vec<SubStep>> {
    steps
        .iter()
        .enumerate()
        .map(|(index, step)| SubStep::parse(step, index, action_key))
        .collect()
}

impl SubStep {
    fn parse(value: &serde_yaml::Value, index: usize, action_key: &str) -> Result<Self> {
        let map = value
            .as_mapping()
            .with_context(|| format!("composite step {index} is not a mapping"))?
            .clone();
        let string = |key: &str| map.get(ykey(key)).and_then(scalar_to_string);

        let kind = match (string("uses"), string("run")) {
            (Some(uses), _) => SubStepKind::Uses(uses),
            (None, Some(script)) => SubStepKind::Run(script),
            (None, None) => bail!("composite step {index} has neither 'run' nor 'uses'"),
        };
        let id = string("id");
        // The official runner names a step without an `id` `__run`, `__owner_repo`
        // and so on. The name never reaches the `steps` context, so any unique one
        // does.
        let name = id.clone().unwrap_or_else(|| format!("__{index}"));
        let condition = action_step_condition(string("if").as_deref());
        let continue_on_error = map
            .get(ykey("continue-on-error"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Ok(Self {
            key: format!("{action_key}.{name}"),
            map,
            kind,
            id,
            condition,
            continue_on_error,
        })
    }

    /// Inside a composite action, `success()` and `failure()` follow the action's
    /// own status, while `cancelled()` follows the job.
    fn condition_holds(
        &self,
        env: &HashMap<String, String>,
        job_state: &JobState,
        job_cancelled: bool,
        action_status: StepConclusion,
    ) -> bool {
        let ctx = ExprContext::new(env, job_state, false, job_cancelled)
            .with_action_status(action_status);
        evaluate_condition(Some(&self.condition), &ctx)
    }

    fn string(&self, key: &str) -> Option<String> {
        self.map.get(ykey(key)).and_then(scalar_to_string)
    }

    /// The step's own `env:`. Like the official runner, every value is resolved
    /// against the environment the step starts from.
    fn own_env(
        &self,
        env: &HashMap<String, String>,
        job_state: &JobState,
    ) -> HashMap<String, String> {
        let Some(serde_yaml::Value::Mapping(env_map)) = self.map.get(ykey("env")) else {
            return HashMap::new();
        };
        let ctx = ExprContext::new(env, job_state, false, false);
        env_map
            .iter()
            .filter_map(|(key, value)| {
                let value = scalar_to_string(value)?;
                Some((key.as_str()?.to_string(), resolve_template(&value, &ctx)))
            })
            .collect()
    }

    fn inputs(&self) -> HashMap<String, String> {
        let Some(serde_yaml::Value::Mapping(with_map)) = self.map.get(ykey("with")) else {
            return HashMap::new();
        };
        with_map
            .iter()
            .filter_map(|(key, value)| Some((key.as_str()?.to_string(), scalar_to_string(value)?)))
            .collect()
    }
}

/// Apply what a sub-step wrote to GITHUB_OUTPUT, GITHUB_ENV, GITHUB_PATH and
/// GITHUB_STATE before the next sub-step starts, as the official runner does after
/// every step, and return the sub-step's outputs. Saved state goes under the
/// sub-step's own key, where its post step looks for it.
fn take_sub_step_file_commands(
    job_state: &mut JobState,
    workspace: &Workspace,
    state_key: &str,
) -> HashMap<String, String> {
    let mut outputs = std::mem::take(&mut job_state.outputs);
    if let Ok(file_outputs) = workspace.read_output_file() {
        outputs.extend(file_outputs);
    }
    if let Ok(file_env) = workspace.read_env_file() {
        job_state.env.extend(file_env);
    }
    if let Ok(extra_paths) = workspace.read_path_file() {
        job_state.path_prepends.extend(extra_paths);
    }

    let mut state = job_state.action_states.remove("").unwrap_or_default();
    if let Ok(file_state) = workspace.read_state_file() {
        state.extend(file_state);
    }
    if !state.is_empty() {
        job_state
            .action_states
            .entry(state_key.to_string())
            .or_default()
            .extend(state);
    }

    workspace.clear_sub_step_files();
    outputs
}

fn record_sub_step(
    job_state: &mut JobState,
    id: &str,
    outputs: HashMap<String, String>,
    conclusion: StepConclusion,
    continue_on_error: bool,
) {
    if !outputs.is_empty() {
        job_state.step_outputs.insert(id.to_string(), outputs);
    }

    job_state.step_outcomes.insert(
        id.to_string(),
        StepOutcome {
            outcome: conclusion.as_str().to_string(),
            conclusion: conclusion
                .after_continue_on_error(continue_on_error)
                .as_str()
                .to_string(),
        },
    );
}

fn skipped_outcome() -> StepOutcome {
    StepOutcome {
        outcome: "skipped".into(),
        conclusion: "skipped".into(),
    }
}

/// Resolve the action's `outputs:` mapping against its own `steps` context. These
/// are the only outputs the caller sees as `steps.<action id>.outputs`.
fn evaluate_action_outputs(
    metadata: &ActionMetadata,
    env: &HashMap<String, String>,
    job_state: &JobState,
) -> HashMap<String, String> {
    let ctx = ExprContext::new(env, job_state, false, false);
    metadata
        .outputs
        .iter()
        .filter_map(|(name, output)| {
            let value = output.value.as_deref()?;
            Some((name.clone(), resolve_template(value, &ctx)))
        })
        .collect()
}

async fn run_nested_script(
    ctx: &CompositeContext<'_>,
    sub_step: &SubStep,
    script: &str,
    job_state: &mut JobState,
    env: &HashMap<String, String>,
    timeout: Duration,
    cancel_token: &CancellationToken,
) -> Result<StepResult> {
    let script_ctx = ExprContext::new(env, job_state, false, false);
    let resolved_script = resolve_template(script, &script_ctx);
    // working-directory may carry expressions like ${{ inputs.dir || '.' }}
    let raw_workdir = sub_step
        .string("working-directory")
        .map(|dir| resolve_template(&dir, &script_ctx));

    let job_container = ctx
        .docker_resources
        .filter(|r| r.job_container_id().is_some());
    let fallback = job_container.map_or("bash", |r| r.default_shell());
    let shell = Shell::resolve(sub_step.string("shell").as_deref(), fallback)?;
    let script_file = write_script_file(ctx.workspace, &shell, &resolved_script)?;

    let result = match job_container {
        Some(resources) => {
            run_script_in_container(
                resources,
                &shell,
                &script_file,
                raw_workdir.as_deref(),
                job_state,
                env,
                ctx.log_sender,
                timeout,
                cancel_token,
            )
            .await
        }
        None => {
            let working_dir = match &raw_workdir {
                Some(d) if d != "." => ctx.workspace.workspace_dir().join(d),
                _ => ctx.workspace.workspace_dir().to_path_buf(),
            };
            let argv = shell.command(&script_file.to_string_lossy());
            let (program, args) = argv.split_first().context("shell command is empty")?;
            let args: Vec<&OsStr> = args.iter().map(OsStr::new).collect();
            run_process(
                program,
                &args,
                env,
                &working_dir,
                job_state,
                ctx.log_sender,
                timeout,
                cancel_token,
            )
            .await
        }
    };

    let _ = std::fs::remove_file(&script_file);
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_script_in_container(
    resources: &JobDockerResources,
    shell: &Shell,
    script_file: &Path,
    raw_workdir: Option<&str>,
    job_state: &mut JobState,
    env: &HashMap<String, String>,
    log_sender: &LogSender,
    timeout: Duration,
    cancel_token: &CancellationToken,
) -> Result<StepResult> {
    let container_id = resources
        .job_container_id()
        .context("no job container for composite script step")?;
    let container_script_path = resources
        .remap_to_container_path(script_file)
        .context("composite script file is outside the container mounts")?;

    let working_dir = match raw_workdir {
        Some(d) if d.starts_with('/') => d.to_string(),
        Some(d) if d != "." => format!("/github/workspace/{d}"),
        _ => "/github/workspace".into(),
    };

    crate::docker::exec::docker_exec(
        resources.docker(),
        container_id,
        shell.command(&container_script_path),
        env,
        &working_dir,
        job_state,
        log_sender,
        timeout,
        cancel_token,
        job_state.debug_enabled,
    )
    .await
}

/// The step an action used inside a composite action runs as. Its `env:` goes in
/// as the step's environment, so it wins over the job's env like any step's.
fn nested_action_step(
    sub_step: &SubStep,
    uses: &str,
    own_env: HashMap<String, String>,
    timeout: Duration,
) -> Step {
    Step {
        id: uuid::Uuid::new_v4().to_string(),
        display_name: uses.to_string(),
        reference: StepReference {
            name: uses.to_string(),
            kind: StepReferenceKind::Repository,
            ..Default::default()
        },
        inputs: sub_step.inputs(),
        condition: None,
        timeout_in_minutes: Some(Evaluable::Literal(timeout.as_secs() / 60)),
        continue_on_error: None,
        order: 0,
        environment: (!own_env.is_empty()).then_some(own_env),
        context_name: Some(sub_step.key.clone()),
    }
}

async fn run_nested_action(
    ctx: &CompositeContext<'_>,
    scope: &ActionScope<'_>,
    nested_step: &Step,
    job_state: &mut JobState,
    env: &HashMap<String, String>,
    cancel_token: &CancellationToken,
    depth: u32,
) -> Result<StepResult> {
    let source = super::parse_uses(&nested_step.reference.name)?;

    if let ActionSource::Docker { image } = &source {
        return super::docker::run_docker_image_action(
            image,
            nested_step,
            job_state,
            ctx.workspace,
            env,
            ctx.log_sender,
            cancel_token,
            ctx.docker_resources,
        )
        .await;
    }

    let action_dir = ctx
        .action_cache
        .get_action(&source, ctx.workspace.workspace_dir(), ctx.access_token)
        .await?;
    let metadata = load_action_metadata(&action_dir)?;
    if let Some(warning) = super::local_pre_warning(&source, &metadata) {
        ctx.log_sender.send(warning).await;
    }

    if let Some(condition) = post_condition(&metadata) {
        let post = EmbeddedPost::new(nested_step, &action_dir, condition, env, job_state);
        register_post(job_state, scope.key(), post);
    }

    let result = run_action(
        ctx,
        &action_dir,
        &metadata,
        ActionStage::Main,
        nested_step,
        job_state,
        env,
        cancel_token,
        depth + 1,
    )
    .await;

    // A nested composite action has a post step once an action inside it has one.
    if metadata.runs.is_composite()
        && job_state
            .embedded_posts
            .contains_key(state_key(nested_step))
    {
        let post = EmbeddedPost::new(nested_step, &action_dir, "always()".into(), env, job_state);
        register_post(job_state, scope.key(), post);
    }

    result
}

/// The `post-if` of an action that has a post step, `always()` unless it says
/// otherwise.
fn post_condition(metadata: &ActionMetadata) -> Option<String> {
    metadata.runs.has_post().then(|| {
        metadata
            .runs
            .post_if
            .clone()
            .unwrap_or_else(|| "always()".into())
    })
}

fn register_post(job_state: &mut JobState, composite_key: &str, post: EmbeddedPost) {
    job_state
        .embedded_posts
        .entry(composite_key.to_string())
        .or_default()
        .push(post);
}

#[allow(clippy::too_many_arguments)]
async fn run_action(
    ctx: &CompositeContext<'_>,
    action_dir: &Path,
    metadata: &ActionMetadata,
    stage: ActionStage,
    step: &Step,
    job_state: &mut JobState,
    env: &HashMap<String, String>,
    cancel_token: &CancellationToken,
    depth: u32,
) -> Result<StepResult> {
    if metadata.runs.is_node() {
        super::node::run_node_action(
            action_dir,
            metadata,
            stage.entry_point(),
            step,
            job_state,
            ctx.workspace,
            env,
            ctx.log_sender,
            cancel_token,
            ctx.docker_resources,
            ctx.node_runtimes,
        )
        .await
    } else if metadata.runs.is_composite() {
        if stage == ActionStage::Post {
            run_composite_post(ctx, step, job_state).await
        } else {
            run_composite_action(ctx, action_dir, metadata, step, job_state, env, depth).await
        }
    } else if metadata.runs.is_docker() {
        super::docker::run_docker_metadata_action(
            action_dir,
            metadata,
            stage.entry_point(),
            step,
            job_state,
            ctx.workspace,
            env,
            ctx.log_sender,
            cancel_token,
            ctx.docker_resources,
        )
        .await
    } else {
        bail!(
            "unsupported action runtime in composite: {}",
            metadata.runs.using
        )
    }
}

pub fn run_composite_post<'a>(
    ctx: &'a CompositeContext<'a>,
    step: &'a Step,
    job_state: &'a mut JobState,
) -> StepFuture<'a> {
    Box::pin(run_composite_post_inner(ctx, step, job_state))
}

/// A composite action's post step runs the post steps that actions inside it
/// registered, the last one first.
async fn run_composite_post_inner(
    ctx: &CompositeContext<'_>,
    step: &Step,
    job_state: &mut JobState,
) -> Result<StepResult> {
    let posts = job_state
        .embedded_posts
        .remove(state_key(step))
        .unwrap_or_default();

    let mut status = StepConclusion::Succeeded;
    for post in posts.iter().rev() {
        let job_cancelled = ctx.job.is_cancelled();
        if !post.condition_holds(job_state, ctx.job.failed, job_cancelled) {
            debug!(step = %post.step.display_name, condition = %post.condition, "skipping embedded post step");
            continue;
        }

        let cancel_token = sub_step_token(ctx.job, job_cancelled, || {
            post.condition_holds(job_state, ctx.job.failed, true)
        });
        let result = run_embedded_post(ctx, post, job_state, &cancel_token).await;
        let conclusion = conclusion_of(result, ctx.log_sender).await;
        take_sub_step_file_commands(job_state, ctx.workspace, state_key(&post.step));
        status = status.merge(conclusion);
    }
    Ok(StepResult { conclusion: status })
}

async fn run_embedded_post(
    ctx: &CompositeContext<'_>,
    post: &EmbeddedPost,
    job_state: &mut JobState,
    cancel_token: &CancellationToken,
) -> Result<StepResult> {
    let metadata = load_action_metadata(&post.action_dir)?;
    run_action(
        ctx,
        &post.action_dir,
        &metadata,
        ActionStage::Post,
        &post.step,
        job_state,
        &post.env,
        cancel_token,
        0,
    )
    .await
}

#[cfg(test)]
#[path = "composite_test.rs"]
mod composite_test;
