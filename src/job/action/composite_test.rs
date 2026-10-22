use std::collections::HashMap;
use std::time::Duration;

use super::*;
use crate::job::action::download::ActionCache;
use crate::job::action::metadata::{ActionInput, ActionMetadata, ActionOutput, ActionRuns};
use crate::job::cancel::JobCancel;
use crate::job::execute::{JobState, JobStatus, StepConclusion};
use crate::job::logs::StepLogger;
use crate::job::masker::SecretMasker;
use crate::job::schema::{Step, StepReference};
use crate::job::workspace::Workspace;

/// Everything a composite action needs to run in a test.
struct Rig {
    tmp: tempfile::TempDir,
    workspace: Workspace,
    cache: ActionCache,
    logger: StepLogger,
    node_runtimes: NodeRuntimes,
    job: JobStatus,
    state: JobState,
}

impl Rig {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = Workspace::create(
            &tmp.path().join("work"),
            &tmp.path().join("tmp"),
            &tmp.path().join("tool-cache"),
            "test-runner",
            "owner/repo",
        )
        .unwrap();
        let cache = ActionCache::new(tmp.path().join("cache"), reqwest::Client::new());
        Self {
            workspace,
            cache,
            logger: StepLogger::results_for_test(SecretMasker::default()),
            node_runtimes: NodeRuntimes::single("node".into()),
            job: JobStatus {
                failed: false,
                cancelled: false,
                cancel: JobCancel::new(),
            },
            state: new_job_state(),
            tmp,
        }
    }

    fn marker(&self, name: &str) -> std::path::PathBuf {
        self.tmp.path().join(name)
    }

    async fn run(
        &mut self,
        metadata: &ActionMetadata,
        step: &Step,
        depth: u32,
    ) -> Result<StepResult> {
        let action_dir = self.tmp.path().join("action");
        std::fs::create_dir_all(&action_dir).unwrap();
        let ctx = CompositeContext {
            workspace: &self.workspace,
            log_sender: self.logger.sender(),
            action_cache: &self.cache,
            access_token: "fake-token",
            docker_resources: None,
            node_runtimes: &self.node_runtimes,
            job: &self.job,
        };
        let base_env = HashMap::from([(
            "GITHUB_STATE".to_string(),
            self.workspace.state_file().display().to_string(),
        )]);
        run_composite_action(
            &ctx,
            &action_dir,
            metadata,
            step,
            &mut self.state,
            &base_env,
            depth,
        )
        .await
    }

    async fn run_post(&mut self, step: &Step) -> Result<StepResult> {
        let ctx = CompositeContext {
            workspace: &self.workspace,
            log_sender: self.logger.sender(),
            action_cache: &self.cache,
            access_token: "fake-token",
            docker_resources: None,
            node_runtimes: &self.node_runtimes,
            job: &self.job,
        };
        run_composite_post(&ctx, step, &mut self.state).await
    }
}

fn new_job_state() -> JobState {
    JobState::new(
        SecretMasker::default(),
        HashMap::new(),
        serde_json::json!({}),
    )
}

fn make_composite_metadata(steps_yaml: &str) -> ActionMetadata {
    let steps: Vec<serde_yaml::Value> = serde_yaml::from_str(steps_yaml).unwrap();
    ActionMetadata {
        inputs: HashMap::new(),
        outputs: HashMap::new(),
        runs: ActionRuns {
            using: crate::job::action::metadata::ActionRuntime::Composite,
            main: None,
            pre: None,
            post: None,
            pre_if: None,
            post_if: None,
            steps: Some(steps),
            image: None,
            entrypoint: None,
            args: None,
            pre_entrypoint: None,
            post_entrypoint: None,
            env: None,
        },
    }
}

fn make_step() -> Step {
    Step {
        id: "1".into(),
        display_name: "Composite".into(),
        reference: StepReference {
            name: "test/composite".into(),
            kind: crate::job::schema::StepReferenceKind::Repository,
            ..Default::default()
        },
        inputs: HashMap::new(),
        condition: None,
        timeout_in_minutes: None,
        continue_on_error: None,
        order: 1,
        environment: None,
        context_name: Some("composite".into()),
    }
}

fn parse_one(yaml: &str) -> Result<SubStep> {
    SubStep::parse(&serde_yaml::from_str(yaml).unwrap(), 2, "outer")
}

// ── Running sub-steps ───────────────────────────────────────────────

#[tokio::test]
async fn nested_script_steps_execute() {
    let mut rig = Rig::new();
    let metadata = make_composite_metadata(
        r#"
- run: echo "step one"
  shell: bash
- run: echo "step two"
  shell: bash
"#,
    );

    let result = rig.run(&metadata, &make_step(), 0).await.unwrap();

    assert_eq!(result.conclusion, StepConclusion::Succeeded);
}

#[tokio::test]
async fn failed_step_fails_the_action_and_skips_later_default_steps() {
    let mut rig = Rig::new();
    let later = rig.marker("later");
    let metadata = make_composite_metadata(&format!(
        r#"
- run: exit 1
  shell: bash
- run: touch '{}'
  shell: bash
"#,
        later.display()
    ));

    let result = rig.run(&metadata, &make_step(), 0).await.unwrap();

    assert_eq!(result.conclusion, StepConclusion::Failed);
    assert!(!later.exists(), "a success() step ran after a failure");
}

#[tokio::test]
async fn always_and_failure_steps_run_after_a_failed_step() {
    let mut rig = Rig::new();
    let always = rig.marker("always");
    let failure = rig.marker("failure");
    let implicit = rig.marker("implicit");
    let metadata = make_composite_metadata(&format!(
        r#"
- run: exit 1
  shell: bash
- if: always()
  run: touch '{}'
  shell: bash
- if: ${{{{ failure() }}}}
  run: touch '{}'
  shell: bash
- if: "true"
  run: touch '{}'
  shell: bash
"#,
        always.display(),
        failure.display(),
        implicit.display()
    ));

    let result = rig.run(&metadata, &make_step(), 0).await.unwrap();

    assert_eq!(result.conclusion, StepConclusion::Failed);
    assert!(always.exists(), "always() step did not run");
    assert!(failure.exists(), "failure() step did not run");
    assert!(
        !implicit.exists(),
        "a condition without a status function ran after a failure"
    );
}

#[tokio::test]
async fn skipped_steps_are_recorded_as_skipped() {
    let mut rig = Rig::new();
    let checked = rig.marker("checked");
    let metadata = make_composite_metadata(&format!(
        r#"
- run: exit 1
  shell: bash
- id: later
  run: "true"
  shell: bash
- if: always()
  run: |
    test "${{{{ steps.later.outcome }}}}/${{{{ steps.later.conclusion }}}}" = "skipped/skipped"
    touch '{}'
  shell: bash
"#,
        checked.display()
    ));

    let result = rig.run(&metadata, &make_step(), 0).await.unwrap();

    assert_eq!(result.conclusion, StepConclusion::Failed);
    assert!(
        checked.exists(),
        "the skipped step was not recorded as skipped"
    );
}

#[tokio::test]
async fn continue_on_error_keeps_the_action_successful() {
    let mut rig = Rig::new();
    let later = rig.marker("later");
    let metadata = make_composite_metadata(&format!(
        r#"
- run: exit 1
  shell: bash
  continue-on-error: true
- run: touch '{}'
  shell: bash
"#,
        later.display()
    ));

    let result = rig.run(&metadata, &make_step(), 0).await.unwrap();

    assert_eq!(result.conclusion, StepConclusion::Succeeded);
    assert!(later.exists());
}

#[tokio::test]
async fn cancelled_job_runs_cancelled_steps_of_a_surviving_action() {
    let mut rig = Rig::new();
    rig.job.cancelled = true;
    let on_cancel = rig.marker("on-cancel");
    let on_failure = rig.marker("on-failure");
    let metadata = make_composite_metadata(&format!(
        r#"
- if: cancelled()
  run: touch '{}'
  shell: bash
- if: failure()
  run: touch '{}'
  shell: bash
"#,
        on_cancel.display(),
        on_failure.display()
    ));

    let result = rig.run(&metadata, &make_step(), 0).await.unwrap();

    assert_eq!(result.conclusion, StepConclusion::Succeeded);
    assert!(on_cancel.exists(), "cancelled() step did not run");
    assert!(
        !on_failure.exists(),
        "failure() step ran in a cancelled job"
    );
}

#[tokio::test]
async fn inputs_resolve_in_run_steps_but_are_not_in_their_env() {
    let mut rig = Rig::new();
    let mut metadata = make_composite_metadata(
        r#"
- run: test "${{ inputs.name }}" = "world" && test -z "${INPUT_NAME+set}"
  shell: bash
"#,
    );
    metadata
        .inputs
        .insert("name".into(), ActionInput { default: None });
    let mut step = make_step();
    step.inputs.insert("name".into(), "world".into());

    let result = rig.run(&metadata, &step, 0).await.unwrap();

    assert_eq!(result.conclusion, StepConclusion::Succeeded);
}

#[tokio::test]
async fn recursion_depth_limit() {
    let mut rig = Rig::new();
    let metadata = make_composite_metadata(
        r#"
- run: echo "hi"
  shell: bash
"#,
    );

    let result = rig.run(&metadata, &make_step(), 10).await;

    assert!(result.unwrap_err().to_string().contains("recursion depth"));
}

#[tokio::test]
async fn step_without_run_or_uses_is_an_error() {
    let mut rig = Rig::new();
    let metadata = make_composite_metadata(
        r#"
- name: nothing to do
"#,
    );

    let result = rig.run(&metadata, &make_step(), 0).await;

    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("neither 'run' nor 'uses'")
    );
}

#[tokio::test]
async fn state_saved_by_a_sub_step_is_keyed_by_that_step() {
    let mut rig = Rig::new();
    let metadata = make_composite_metadata(
        r#"
- id: saver
  run: echo "pid=42" >> "$GITHUB_STATE"
  shell: bash
"#,
    );

    rig.run(&metadata, &make_step(), 0).await.unwrap();

    assert_eq!(rig.state.action_states["composite.saver"]["pid"], "42");
    assert!(rig.workspace.read_state_file().unwrap().is_empty());
}

// ── Embedded post steps ─────────────────────────────────────────────

fn embedded_post(rig: &Rig, condition: &str) -> EmbeddedPost {
    let mut main_step = make_step();
    main_step.context_name = Some("composite.__0".into());
    EmbeddedPost::new(
        &main_step,
        &rig.tmp.path().join("missing-action"),
        condition.into(),
        &HashMap::new(),
    )
}

#[tokio::test]
async fn composite_post_skips_posts_whose_condition_fails() {
    let mut rig = Rig::new();
    let post = embedded_post(&rig, "failure()");
    register_post(&mut rig.state, "composite", post);
    let mut post_step = make_step();
    post_step.context_name = Some("composite".into());

    let result = rig.run_post(&post_step).await.unwrap();

    assert_eq!(result.conclusion, StepConclusion::Succeeded);
    assert!(rig.state.embedded_posts.is_empty());
}

#[tokio::test]
async fn composite_post_fails_when_a_post_it_runs_fails() {
    let mut rig = Rig::new();
    let post = embedded_post(&rig, "always()");
    register_post(&mut rig.state, "composite", post);
    let mut post_step = make_step();
    post_step.context_name = Some("composite".into());

    let result = rig.run_post(&post_step).await.unwrap();

    assert_eq!(result.conclusion, StepConclusion::Failed);
}

#[test]
fn sub_step_inputs_resolve_against_the_action_scope() {
    let state = new_job_state();
    let sub_step = parse_one(
        r#"
uses: actions/cache@v4
with:
  path: ${{ inputs.target }}/out
  retries: 3
"#,
    )
    .unwrap();
    let env = HashMap::from([("INPUT_TARGET".to_string(), "dist".to_string())]);

    let inputs = sub_step.inputs(&env, &state);

    assert_eq!(inputs["path"], "dist/out");
    assert_eq!(inputs["retries"], "3");
}

#[test]
fn post_condition_defaults_to_always() {
    let mut metadata = make_composite_metadata("[]");
    metadata.runs.using = crate::job::action::metadata::ActionRuntime::Node("20".into());
    assert_eq!(post_condition(&metadata), None);

    metadata.runs.post = Some("post.js".into());
    assert_eq!(post_condition(&metadata).as_deref(), Some("always()"));

    metadata.runs.post_if = Some("success()".into());
    assert_eq!(post_condition(&metadata).as_deref(), Some("success()"));
}

// ── Cancel tokens ───────────────────────────────────────────────────

fn job_status(cancel: &JobCancel) -> JobStatus {
    JobStatus {
        failed: false,
        cancelled: false,
        cancel: cancel.clone(),
    }
}

#[tokio::test]
async fn sub_step_whose_condition_fails_on_cancel_is_stopped_by_a_request() {
    let cancel = JobCancel::new();

    let token = sub_step_token(&job_status(&cancel), false, || false);
    cancel.request(Duration::from_secs(60));

    assert!(token.is_cancelled());
}

#[tokio::test]
async fn sub_step_whose_condition_holds_on_cancel_survives_a_request() {
    let cancel = JobCancel::new();

    let token = sub_step_token(&job_status(&cancel), false, || true);
    cancel.request(Duration::from_secs(60));

    assert!(!token.is_cancelled());
    cancel.force();
    assert!(token.is_cancelled());
}

#[tokio::test]
async fn sub_step_started_after_a_cancel_is_only_stopped_by_force() {
    let cancel = JobCancel::new();
    cancel.request(Duration::from_secs(60));

    let token = sub_step_token(&job_status(&cancel), true, || {
        unreachable!("a step started after the cancel is not tested again")
    });

    assert!(!token.is_cancelled());
    cancel.force();
    assert!(token.is_cancelled());
}

// ── Parsing sub-steps ───────────────────────────────────────────────

#[test]
fn sub_step_key_uses_the_id_or_the_position() {
    let with_id = parse_one("{ id: build, run: make }").unwrap();
    let without_id = parse_one("{ uses: actions/checkout@v4 }").unwrap();

    assert_eq!(with_id.key, "outer.build");
    assert_eq!(with_id.id.as_deref(), Some("build"));
    assert_eq!(without_id.key, "outer.__2");
    assert_eq!(without_id.id, None);
}

#[test]
fn sub_step_condition_is_read_like_an_action_step_condition() {
    let default = parse_one("{ run: make }").unwrap();
    let plain = parse_one("{ run: make, if: \"env.X == 'y'\" }").unwrap();
    let boolean = parse_one("{ run: make, if: false }").unwrap();

    assert_eq!(default.condition, "success()");
    assert_eq!(plain.condition, "success() && (env.X == 'y')");
    assert_eq!(boolean.condition, "success() && (false)");
}

#[test]
fn sub_step_must_be_a_mapping_that_runs_or_uses_something() {
    assert!(parse_one("just a string").is_err());
    assert!(parse_one("{ name: idle }").is_err());
}

#[test]
fn sub_step_own_env_is_resolved_against_the_starting_env() {
    let state = new_job_state();
    let sub_step = parse_one(
        r#"
run: make
env:
  TARGET: ${{ env.BASE }}/out
  RETRIES: 3
  BASE: changed
"#,
    )
    .unwrap();
    let env = HashMap::from([("BASE".to_string(), "dist".to_string())]);

    let own_env = sub_step.own_env(&env, &state);

    assert_eq!(own_env["TARGET"], "dist/out");
    assert_eq!(own_env["RETRIES"], "3");
    assert_eq!(own_env["BASE"], "changed");
}

#[test]
fn nested_action_step_carries_inputs_env_and_key() {
    let sub_step = parse_one(
        r#"
uses: ./.github/actions/inner
with:
  cache: true
"#,
    )
    .unwrap();
    let own_env = HashMap::from([("MODE".to_string(), "fast".to_string())]);

    let inputs = sub_step.inputs(&HashMap::new(), &new_job_state());

    let step = nested_action_step(
        &sub_step,
        "./.github/actions/inner",
        inputs,
        own_env,
        Duration::from_secs(600),
    );

    assert_eq!(step.inputs["cache"], "true");
    assert_eq!(step.environment.as_ref().unwrap()["MODE"], "fast");
    assert_eq!(step.context_name.as_deref(), Some("outer.__2"));
    assert_eq!(step.timeout(), Duration::from_secs(600));
}

#[test]
fn scalar_to_string_reads_yaml_scalars_like_github() {
    let parse = |yaml: &str| scalar_to_string(&serde_yaml::from_str(yaml).unwrap());

    assert_eq!(parse("text").as_deref(), Some("text"));
    assert_eq!(parse("'true'").as_deref(), Some("true"));
    assert_eq!(parse("true").as_deref(), Some("true"));
    assert_eq!(parse("false").as_deref(), Some("false"));
    assert_eq!(parse("3").as_deref(), Some("3"));
    assert_eq!(parse("1.5").as_deref(), Some("1.5"));
    assert_eq!(parse("[a, b]"), None);
}

// ── Recording results ───────────────────────────────────────────────

#[test]
fn record_sub_step_keeps_outputs_and_both_results() {
    let mut state = new_job_state();
    let outputs = HashMap::from([("name".to_string(), "value".to_string())]);

    record_sub_step(&mut state, "flaky", outputs, StepConclusion::Failed, true);

    assert_eq!(state.step_outputs["flaky"]["name"], "value");
    assert_eq!(state.step_outcomes["flaky"].outcome, "failure");
    assert_eq!(state.step_outcomes["flaky"].conclusion, "success");
}

#[test]
fn record_sub_step_without_outputs_records_only_results() {
    let mut state = new_job_state();

    record_sub_step(
        &mut state,
        "quiet",
        HashMap::new(),
        StepConclusion::Failed,
        false,
    );

    assert!(!state.step_outputs.contains_key("quiet"));
    assert_eq!(state.step_outcomes["quiet"].outcome, "failure");
    assert_eq!(state.step_outcomes["quiet"].conclusion, "failure");
}

#[tokio::test]
async fn take_sub_step_file_commands_applies_files_and_clears_them() {
    let rig = Rig::new();
    let mut state = new_job_state();
    state.outputs.insert("legacy".into(), "1".into());
    state
        .action_states
        .entry(String::new())
        .or_default()
        .insert("from_command".into(), "a".into());
    std::fs::write(rig.workspace.output_file(), "name=hello\n").unwrap();
    std::fs::write(rig.workspace.env_file(), "FOO=bar\n").unwrap();
    std::fs::write(rig.workspace.path_file(), "/opt/bin\n").unwrap();
    std::fs::write(rig.workspace.state_file(), "pid=42\n").unwrap();

    let outputs = take_sub_step_file_commands(&mut state, &rig.workspace, "outer.save");

    assert_eq!(outputs["name"], "hello");
    assert_eq!(outputs["legacy"], "1");
    assert!(state.outputs.is_empty());
    assert_eq!(state.env["FOO"], "bar");
    assert_eq!(state.path_prepends, ["/opt/bin"]);
    assert_eq!(state.action_states["outer.save"]["pid"], "42");
    assert_eq!(state.action_states["outer.save"]["from_command"], "a");
    assert!(!state.action_states.contains_key(""));
    assert!(rig.workspace.read_output_file().unwrap().is_empty());
    assert!(rig.workspace.read_state_file().unwrap().is_empty());
}

#[tokio::test]
async fn take_sub_step_file_commands_without_state_adds_no_entry() {
    let rig = Rig::new();
    let mut state = new_job_state();

    let outputs = take_sub_step_file_commands(&mut state, &rig.workspace, "outer.quiet");

    assert!(outputs.is_empty());
    assert!(state.action_states.is_empty());
}

#[test]
fn evaluate_action_outputs_resolves_against_the_action_steps_and_inputs() {
    let mut state = new_job_state();
    state.step_outputs.insert(
        "make".into(),
        HashMap::from([("raw".to_string(), "hello".to_string())]),
    );
    let mut metadata = make_composite_metadata("[]");
    metadata.outputs.insert(
        "greeting".into(),
        ActionOutput {
            value: Some("${{ steps.make.outputs.raw }}-${{ inputs.suffix }}".into()),
        },
    );
    metadata
        .outputs
        .insert("documented".into(), ActionOutput { value: None });
    let env = HashMap::from([("INPUT_SUFFIX".to_string(), "world".to_string())]);

    let outputs = evaluate_action_outputs(&metadata, &env, &state);

    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs["greeting"], "hello-world");
}

#[test]
fn sub_step_inputs_and_env_do_not_evaluate_what_an_expression_returned() {
    let state = JobState::new(
        SecretMasker::default(),
        HashMap::from([("MY_SECRET".to_string(), "leaked".to_string())]),
        serde_json::json!({}),
    );
    let sub_step = parse_one(
        r#"
uses: actions/cache@v4
env:
  COPY: ${{ inputs.title }}
with:
  title: ${{ inputs.title }}
"#,
    )
    .unwrap();
    let env = HashMap::from([(
        "INPUT_TITLE".to_string(),
        "${{ secrets.MY_SECRET }}".to_string(),
    )]);

    let inputs = sub_step.inputs(&env, &state);
    let own_env = sub_step.own_env(&env, &state);

    assert_eq!(inputs["title"], "${{ secrets.MY_SECRET }}");
    assert_eq!(own_env["COPY"], "${{ secrets.MY_SECRET }}");
}

#[test]
fn pre_condition_defaults_to_always_and_composites_always_run() {
    let mut metadata = make_composite_metadata("[]");
    assert_eq!(pre_condition(&metadata).as_deref(), Some("always()"));

    metadata.runs.using = crate::job::action::metadata::ActionRuntime::Node("20".into());
    assert_eq!(pre_condition(&metadata), None);

    metadata.runs.pre = Some("pre.js".into());
    assert_eq!(pre_condition(&metadata).as_deref(), Some("always()"));

    metadata.runs.pre_if = Some("runner.os == 'Linux'".into());
    assert_eq!(
        pre_condition(&metadata).as_deref(),
        Some("runner.os == 'Linux'")
    );
}

const PRE_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

fn write_cached_action(cache_dir: &Path, repo: &str, action_yml: &str) {
    let dir = cache_dir.join("acme").join(repo).join(PRE_COMMIT);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("action.yml"), action_yml).unwrap();
}

fn composite_using(repo: &str) -> ActionMetadata {
    make_composite_metadata(&format!("- uses: acme/{repo}@{PRE_COMMIT}"))
}

async fn has_pre(cache_dir: &Path, metadata: &ActionMetadata) -> bool {
    let action_cache = ActionCache::new(cache_dir.to_path_buf(), reqwest::Client::new());
    has_nested_pre(&action_cache, cache_dir, "token", metadata, 0).await
}

#[tokio::test]
async fn composite_has_a_pre_once_a_remote_action_it_uses_has_one() {
    let tmp = tempfile::tempdir().unwrap();
    write_cached_action(
        tmp.path(),
        "with-pre",
        "runs:\n  using: node20\n  pre: pre.js\n  main: main.js\n",
    );
    write_cached_action(
        tmp.path(),
        "without-pre",
        "runs:\n  using: node20\n  main: main.js\n",
    );
    write_cached_action(
        tmp.path(),
        "middle",
        &format!("runs:\n  using: composite\n  steps:\n    - uses: acme/with-pre@{PRE_COMMIT}\n"),
    );

    assert!(has_pre(tmp.path(), &composite_using("with-pre")).await);
    assert!(has_pre(tmp.path(), &composite_using("middle")).await);
    assert!(!has_pre(tmp.path(), &composite_using("without-pre")).await);
    assert!(!has_pre(tmp.path(), &make_composite_metadata("- run: echo hi")).await);
}

#[tokio::test]
async fn local_and_docker_actions_give_a_composite_no_pre() {
    let tmp = tempfile::tempdir().unwrap();
    let local_dir = tmp.path().join(".github/actions/local");
    std::fs::create_dir_all(&local_dir).unwrap();
    std::fs::write(
        local_dir.join("action.yml"),
        "runs:\n  using: node20\n  pre: pre.js\n  main: main.js\n",
    )
    .unwrap();
    let metadata =
        make_composite_metadata("- uses: ./.github/actions/local\n- uses: docker://alpine:3.19");

    assert!(!has_pre(tmp.path(), &metadata).await);
}
