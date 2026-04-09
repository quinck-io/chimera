mod common;

use chimera::job::client::JobConclusion;
use common::*;

#[tokio::test]
async fn failure_stops_remaining_steps() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step("s1", "exit 1"),
            script_step("s2", "echo should not run"),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Failed);
}

#[tokio::test]
async fn continue_on_error_preserves_job_success() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step_continue("s1", "exit 1"),
            script_step("s2", "echo still running"),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn continue_on_error_outcome_vs_conclusion() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step_continue("soft_fail", "exit 1"),
            script_step(
                "check_outcome",
                r#"test "${{ steps.soft_fail.outcome }}" = "failure" || exit 1"#,
            ),
            script_step(
                "check_conclusion",
                r#"test "${{ steps.soft_fail.conclusion }}" = "success" || exit 1"#,
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn continue_on_error_with_nonzero_exit() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step_continue("hard_fail", "exit 42"),
            script_step("next", "echo still running"),
            script_step_if(
                "check",
                r#"test "${{ steps.hard_fail.outcome }}" = "failure" || exit 1"#,
                "always()",
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn condition_always_runs_after_failure() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step("fail", "exit 1"),
            script_step_if("always_step", r#"echo "ran after failure""#, "always()"),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Failed);
}

#[tokio::test]
async fn condition_failure_runs_after_failure() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step("fail", "exit 1"),
            script_step_if("on_failure", "echo ran on failure", "failure()"),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Failed);
}

#[tokio::test]
async fn condition_success_skips_after_failure() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step("fail", "exit 1"),
            script_step("should_skip", "echo should not run"),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Failed);
}

#[tokio::test]
async fn job_status_success() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"test "${{ job.status }}" = "success" || exit 1"#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn job_status_success_after_continue_on_error() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step_continue("soft_fail", "exit 1"),
            script_step("check", r#"test "${{ job.status }}" = "success" || exit 1"#),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn job_status_failure_after_hard_fail() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step("fail", "exit 1"),
            script_step_if(
                "check",
                r#"test "${{ job.status }}" = "failure" || exit 1"#,
                "always()",
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Failed);
}
