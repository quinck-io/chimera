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
