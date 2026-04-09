mod common;

use std::collections::HashMap;

use chimera::job::client::JobConclusion;
use common::*;

#[tokio::test]
async fn env_vars_are_set() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"
            test "$GITHUB_ACTIONS" = "true" || exit 1
            test -n "$GITHUB_WORKSPACE" || exit 1
            test -n "$GITHUB_ENV" || exit 1
            test -n "$GITHUB_PATH" || exit 1
            test -n "$GITHUB_OUTPUT" || exit 1
            test -n "$GITHUB_STATE" || exit 1
            test -n "$GITHUB_STEP_SUMMARY" || exit 1
            test -n "$GITHUB_EVENT_PATH" || exit 1
            test -n "$RUNNER_OS" || exit 1
            test -n "$RUNNER_NAME" || exit 1
            test -n "$RUNNER_TEMP" || exit 1
            test -n "$RUNNER_TOOL_CACHE" || exit 1
            "#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn github_context_env_vars() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step(
            "s1",
            r#"
            test "$GITHUB_REPOSITORY" = "owner/test-repo" || exit 1
            test "$GITHUB_SHA" = "abc123" || exit 1
            test "$GITHUB_REF" = "refs/heads/main" || exit 1
            test "$GITHUB_WORKFLOW" = "test.yml" || exit 1
            test "$GITHUB_ACTOR" = "testuser" || exit 1
            test "$GITHUB_EVENT_NAME" = "push" || exit 1
            "#,
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "github": {
                "repository": "owner/test-repo",
                "sha": "abc123",
                "ref": "refs/heads/main",
                "workflow": "test.yml",
                "actor": "testuser",
                "event_name": "push"
            }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn event_file_exists_and_is_json() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"test -f "$GITHUB_EVENT_PATH" || exit 1"#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn env_file_propagation_between_steps() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step("s1", r#"echo "MY_VAR=hello_from_env" >> "$GITHUB_ENV""#),
            script_step("s2", r#"test "$MY_VAR" = "hello_from_env" || exit 1"#),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn env_file_heredoc_multiline() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step(
                "s1",
                "echo 'MULTI<<EOF' >> \"$GITHUB_ENV\"\necho 'line1' >> \"$GITHUB_ENV\"\necho 'line2' >> \"$GITHUB_ENV\"\necho 'EOF' >> \"$GITHUB_ENV\"",
            ),
            script_step(
                "s2",
                "echo \"MULTI=$MULTI\"\ntest \"$MULTI\" = \"line1\nline2\" || exit 1",
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn path_file_prepend_between_steps() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step("s1", r#"echo "/custom/test/bin" >> "$GITHUB_PATH""#),
            script_step(
                "s2",
                r#"echo "$PATH" | grep -q "/custom/test/bin" || exit 1"#,
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn output_file_sets_step_output() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step(
                "s1",
                r#"echo "greeting=hello from step" >> "$GITHUB_OUTPUT""#,
            ),
            script_step(
                "s2",
                r#"test "${{ steps.s1.outputs.greeting }}" = "hello from step" || exit 1"#,
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn output_file_heredoc_multiline() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step(
                "s1",
                "echo 'result<<EOF' >> \"$GITHUB_OUTPUT\"\necho 'multi' >> \"$GITHUB_OUTPUT\"\necho 'line' >> \"$GITHUB_OUTPUT\"\necho 'EOF' >> \"$GITHUB_OUTPUT\"",
            ),
            script_step(
                "s2",
                r#"test -n "${{ steps.s1.outputs.result }}" || exit 1"#,
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn state_file_captures_state() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"echo "mykey=myval" >> "$GITHUB_STATE""#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn step_summary_file_writable() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            "echo '## Test Summary' >> \"$GITHUB_STEP_SUMMARY\" && test -s \"$GITHUB_STEP_SUMMARY\"",
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn step_env_vars_resolved() {
    let env = TestEnv::setup().await;
    let step = script_step_env(
        "s1",
        r#"test "$GREETING" = "hello world" || exit 1"#,
        HashMap::from([("GREETING".into(), "hello world".into())]),
    );
    let manifest = manifest_with_steps(vec![step], &env.mock_server.uri());
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn job_outputs_returned() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"echo "greeting=hello from job" >> "$GITHUB_OUTPUT""#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, outputs) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
    assert_eq!(outputs.get("greeting").unwrap(), "hello from job");
}

#[tokio::test]
async fn env_accumulates_across_three_steps() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step("s1", r#"echo "A=1" >> "$GITHUB_ENV""#),
            script_step("s2", r#"echo "B=2" >> "$GITHUB_ENV""#),
            script_step(
                "s3",
                r#"
                test "$A" = "1" || exit 1
                test "$B" = "2" || exit 1
                "#,
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn path_accumulates_across_steps() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step("s1", r#"echo "/first/bin" >> "$GITHUB_PATH""#),
            script_step("s2", r#"echo "/second/bin" >> "$GITHUB_PATH""#),
            script_step(
                "s3",
                r#"
                echo "$PATH" | grep -q "/first/bin" || exit 1
                echo "$PATH" | grep -q "/second/bin" || exit 1
                "#,
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn multiple_outputs_from_one_step() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step(
                "s1",
                "echo \"key1=val1\" >> \"$GITHUB_OUTPUT\"\necho \"key2=val2\" >> \"$GITHUB_OUTPUT\"",
            ),
            script_step(
                "s2",
                r#"
                test "${{ steps.s1.outputs.key1 }}" = "val1" || exit 1
                test "${{ steps.s1.outputs.key2 }}" = "val2" || exit 1
                "#,
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn step_output_chaining_three_steps() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step("s1", r#"echo "val=hello" >> "$GITHUB_OUTPUT""#),
            script_step(
                "s2",
                "echo \"val=${{ steps.s1.outputs.val }}-world\" >> \"$GITHUB_OUTPUT\"",
            ),
            script_step(
                "s3",
                r#"test "${{ steps.s2.outputs.val }}" = "hello-world" || exit 1"#,
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn env_var_overwrite_between_steps() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step("s1", r#"echo "X=first" >> "$GITHUB_ENV""#),
            script_step("s2", r#"echo "X=second" >> "$GITHUB_ENV""#),
            script_step("s3", r#"test "$X" = "second" || exit 1"#),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn cancel_token_cancels_job() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step("s1", "echo step1")],
        &env.mock_server.uri(),
    );

    let mut base_env =
        chimera::runner::env::build_base_env(&manifest, &env.workspace, "test-runner");
    if let Ok(path) = std::env::var("PATH") {
        base_env.entry("PATH".into()).or_insert(path);
    }
    let action_cache = chimera::job::action::ActionCache::new(
        env.workspace.runner_temp().join("actions"),
        reqwest::Client::new(),
    );
    let cancel_token = tokio_util::sync::CancellationToken::new();
    cancel_token.cancel();

    let (conclusion, _) = chimera::job::execute::run_all_steps(
        &manifest,
        &env.job_client,
        &env.workspace,
        &base_env,
        "test-runner",
        &action_cache,
        "fake-token",
        cancel_token,
        None,
        std::path::Path::new("node"),
        None,
    )
    .await
    .unwrap();
    assert_eq!(conclusion, JobConclusion::Cancelled);
}
