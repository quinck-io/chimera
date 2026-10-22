mod common;

use std::path::Path;

use chimera::job::client::JobConclusion;
use common::*;

const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

/// A node action whose pre, main and post scripts each append a line to `log`
/// naming the stage and the step they ran for. Main saves state for post to read.
fn write_staged_action(action_dir: &Path, log: &Path) {
    std::fs::create_dir_all(action_dir).unwrap();
    std::fs::write(
        action_dir.join("action.yml"),
        "name: staged\nruns:\n  using: node20\n  pre: pre.js\n  main: main.js\n  post: post.js\n",
    )
    .unwrap();

    let append = |line: &str| {
        format!(
            "require('fs').appendFileSync({:?}, {line} + '\\n');\n",
            log.to_string_lossy()
        )
    };
    std::fs::write(
        action_dir.join("pre.js"),
        append("'pre ' + process.env.GITHUB_ACTION"),
    )
    .unwrap();
    std::fs::write(
        action_dir.join("main.js"),
        append("'main ' + process.env.GITHUB_ACTION")
            + "require('fs').appendFileSync(process.env.GITHUB_STATE, 'saved=by-' + process.env.GITHUB_ACTION + '\\n');\n",
    )
    .unwrap();
    std::fs::write(
        action_dir.join("post.js"),
        append("'post ' + process.env.GITHUB_ACTION + ' ' + process.env.STATE_saved"),
    )
    .unwrap();
}

fn log_lines(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(String::from)
        .collect()
}

#[tokio::test]
async fn step_ids_that_look_like_stages_run_their_action_main() {
    let env = TestEnv::setup().await;
    let log = env.tmp.path().join("stages.log");
    write_staged_action(&env.cached_action_dir("acme", "staged", COMMIT), &log);
    let manifest = manifest_with_steps(
        vec![
            remote_action_step("cache_post", "acme/staged", COMMIT),
            remote_action_step("run_pre_checks", "acme/staged", COMMIT),
        ],
        &env.mock_server.uri(),
    );

    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Succeeded);
    assert_eq!(
        log_lines(&log),
        [
            "pre cache_post",
            "pre run_pre_checks",
            "main cache_post",
            "main run_pre_checks",
            "post run_pre_checks by-run_pre_checks",
            "post cache_post by-cache_post",
        ]
    );
}

/// Like the official runner, a local action's `pre` never runs, since the action is
/// only on disk once a step checks it out. Its main and post steps still run.
#[tokio::test]
async fn local_action_pre_is_skipped() {
    let env = TestEnv::setup().await;
    let log = env.tmp.path().join("stages.log");
    let action_dir = env.workspace.workspace_dir().join(".github/actions/staged");
    write_staged_action(&action_dir, &log);
    let manifest = manifest_with_steps(
        vec![local_action_step("staged", ".github/actions/staged")],
        &env.mock_server.uri(),
    );

    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Succeeded);
    assert_eq!(log_lines(&log), ["main staged", "post staged by-staged"]);
}

/// The official runner only reads `pre` and `post` for JavaScript and Docker
/// actions, so a composite action that declares them runs once, as its main step.
#[tokio::test]
async fn composite_action_declared_pre_and_post_are_ignored() {
    let env = TestEnv::setup().await;
    let log = env.tmp.path().join("stages.log");
    let action_dir = env.cached_action_dir("acme", "composite", COMMIT);
    std::fs::create_dir_all(&action_dir).unwrap();
    std::fs::write(action_dir.join("pre.js"), "").unwrap();
    std::fs::write(action_dir.join("post.js"), "").unwrap();
    std::fs::write(
        action_dir.join("action.yml"),
        format!(
            r#"
name: composite
runs:
  using: composite
  pre: pre.js
  post: post.js
  steps:
    - shell: bash
      run: echo "ran $GITHUB_ACTION" >> {log:?}
"#,
            log = log.to_string_lossy()
        ),
    )
    .unwrap();
    let manifest = manifest_with_steps(
        vec![remote_action_step("composite", "acme/composite", COMMIT)],
        &env.mock_server.uri(),
    );

    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Succeeded);
    assert_eq!(log_lines(&log), ["ran composite"]);
}

/// A node action named `label` whose pre step saves state that its main and post
/// steps log. `pre_if` is written as the action's `pre-if:`.
fn write_stateful_action(action_dir: &Path, log: &Path, label: &str, pre_if: Option<&str>) {
    std::fs::create_dir_all(action_dir).unwrap();
    let pre_if = pre_if.map_or(String::new(), |condition| {
        format!("  pre-if: {condition}\n")
    });
    std::fs::write(
        action_dir.join("action.yml"),
        format!(
            "name: {label}\nruns:\n  using: node20\n  pre: pre.js\n{pre_if}  main: main.js\n  post: post.js\n"
        ),
    )
    .unwrap();

    let append = |line: String| {
        format!(
            "require('fs').appendFileSync({:?}, {line} + '\\n');\n",
            log.to_string_lossy()
        )
    };
    std::fs::write(
        action_dir.join("pre.js"),
        append(format!("'pre {label}'"))
            + &format!(
                "require('fs').appendFileSync(process.env.GITHUB_STATE, 'from=pre-{label}\\n');\n"
            ),
    )
    .unwrap();
    std::fs::write(
        action_dir.join("main.js"),
        append(format!("'main {label} ' + process.env.STATE_from")),
    )
    .unwrap();
    std::fs::write(
        action_dir.join("post.js"),
        append(format!("'post {label} ' + process.env.STATE_from")),
    )
    .unwrap();
}

fn write_composite(action_dir: &Path, steps: &str) {
    std::fs::create_dir_all(action_dir).unwrap();
    std::fs::write(
        action_dir.join("action.yml"),
        format!("name: composite\nruns:\n  using: composite\n  steps:\n{steps}"),
    )
    .unwrap();
}

/// Like the official runner, a composite action gets a pre step that runs the pre
/// steps of the remote actions it uses before the job's steps, each under its own
/// `pre-if`, and what they save reaches their main and post steps.
#[tokio::test]
async fn composite_action_runs_the_pre_steps_of_the_actions_it_uses() {
    let env = TestEnv::setup().await;
    let log = env.tmp.path().join("stages.log");
    let alpha_dir = env.cached_action_dir("acme", "alpha", COMMIT);
    write_stateful_action(&alpha_dir, &log, "alpha", None);
    let beta_dir = env.cached_action_dir("acme", "beta", COMMIT);
    write_stateful_action(&beta_dir, &log, "beta", Some("false"));
    write_composite(
        &env.cached_action_dir("acme", "wrapper", COMMIT),
        &format!(
            "    - shell: bash\n      run: echo 'wrapper main' >> {log:?}\n\
             \x20   - if: false\n      uses: acme/alpha@{COMMIT}\n\
             \x20   - uses: acme/alpha@{COMMIT}\n\
             \x20   - uses: acme/beta@{COMMIT}\n",
            log = log.to_string_lossy()
        ),
    );
    let manifest = manifest_with_steps(
        vec![
            script_step(
                "first",
                &format!("echo first >> '{}'", log.to_string_lossy()),
            ),
            remote_action_step("wrap", "acme/wrapper", COMMIT),
        ],
        &env.mock_server.uri(),
    );

    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Succeeded);
    assert_eq!(
        log_lines(&log),
        [
            "pre alpha",
            "pre alpha",
            "first",
            "wrapper main",
            "main alpha pre-alpha",
            "main beta undefined",
            "post beta undefined",
            "post alpha pre-alpha",
        ]
    );
}

/// The pre steps of actions inside a nested remote composite action run too, while
/// a local action, only on disk once a step checks it out, has none.
#[tokio::test]
async fn composite_action_runs_nested_pre_steps_through_nested_composites() {
    let env = TestEnv::setup().await;
    let log = env.tmp.path().join("stages.log");
    write_stateful_action(
        &env.cached_action_dir("acme", "alpha", COMMIT),
        &log,
        "alpha",
        None,
    );
    write_stateful_action(
        &env.workspace.workspace_dir().join(".github/actions/local"),
        &log,
        "local",
        None,
    );
    write_composite(
        &env.cached_action_dir("acme", "middle", COMMIT),
        &format!("    - uses: acme/alpha@{COMMIT}\n"),
    );
    write_composite(
        &env.cached_action_dir("acme", "wrapper", COMMIT),
        &format!("    - uses: acme/middle@{COMMIT}\n    - uses: ./.github/actions/local\n"),
    );
    let manifest = manifest_with_steps(
        vec![remote_action_step("wrap", "acme/wrapper", COMMIT)],
        &env.mock_server.uri(),
    );

    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Succeeded);
    assert_eq!(
        log_lines(&log),
        [
            "pre alpha",
            "main alpha pre-alpha",
            "main local undefined",
            "post local undefined",
            "post alpha pre-alpha",
        ]
    );
}
