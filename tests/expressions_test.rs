mod common;

use std::collections::HashMap;

use chimera::job::client::JobConclusion;
use common::*;

#[tokio::test]
async fn bracket_notation() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step(
            "s1",
            r#"test "${{ needs['setup'].result }}" = "success" || exit 1"#,
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "needs": {
                "setup": { "result": "success", "outputs": { "greeting": "hello" } }
            }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn bracket_notation_nested() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step(
            "s1",
            r#"test "${{ needs['setup'].outputs['greeting'] }}" = "hello" || exit 1"#,
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "needs": {
                "setup": { "result": "success", "outputs": { "greeting": "hello" } }
            }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn wildcard_operator() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step_if(
            "s1",
            "echo wildcard works",
            "contains(needs.*.result, 'success')",
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "needs": {
                "setup": { "result": "success", "outputs": {} },
                "build": { "result": "success", "outputs": {} }
            }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn wildcard_negative() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step_if(
            "s1",
            "echo should not run",
            "contains(needs.*.result, 'failure')",
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "needs": { "setup": { "result": "success", "outputs": {} } }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn case_insensitive_functions() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step_if("s1", "echo ok", "CONTAINS('Hello World', 'hello')"),
            script_step_if("s2", "echo ok", "startswith('Hello', 'hel')"),
            script_step_if("s3", "echo ok", "EndsWith('Hello', 'llo')"),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn tojson_on_object() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step(
            "s1",
            "RESULT='${{ toJSON(needs.setup) }}'\necho \"$RESULT\" | grep -q '\"result\"' || exit 1",
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "needs": { "setup": { "result": "success", "outputs": {} } }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn tojson_on_wildcard_array() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step(
            "s1",
            "RESULT='${{ toJSON(needs.*.result) }}'\necho \"$RESULT\" | grep -q 'success' || exit 1",
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "needs": { "a": { "result": "success", "outputs": {} } }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn join_with_separator() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step(
            "s1",
            "JOINED=\"${{ join(needs.*.result, ', ') }}\"\necho \"$JOINED\" | grep -q 'success' || exit 1",
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "needs": {
                "a": { "result": "success", "outputs": {} },
                "b": { "result": "success", "outputs": {} }
            }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn join_default_separator() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step(
            "s1",
            "JOINED=\"${{ join(needs.*.result) }}\"\necho \"$JOINED\" | grep -q ',' || exit 1",
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "needs": {
                "a": { "result": "success", "outputs": {} },
                "b": { "result": "success", "outputs": {} }
            }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn format_function() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"test "${{ format('Hello {0}, you are {1}!', 'World', 'great') }}" = "Hello World, you are great!" || exit 1"#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn hex_and_scientific_literals() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step_if("s1", "echo hex ok", "0xFF == 255"),
            script_step_if("s2", "echo sci ok", "1e3 == 1000"),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn boolean_logic() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step_if("s1", "echo ok", "true && true"),
            script_step_if("s2", "echo ok", "false || true"),
            script_step_if("s3", "echo ok", "!false"),
            script_step_if("s4", "echo ok", "(true || false) && true"),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn comparison_operators() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step_if("s1", "echo ok", "42 > 10"),
            script_step_if("s2", "echo ok", "10 <= 10"),
            script_step_if("s3", "echo ok", "10 != 20"),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn null_is_falsy() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"test "${{ null || 'fallback' }}" = "fallback" || exit 1"#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn vars_context_empty() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"test -z "${{ vars.NONEXISTENT_VAR }}" || exit 1"#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn string_functions_in_scripts() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step_if("s1", "echo ok", "contains('hello world', 'world')"),
            script_step_if("s2", "echo ok", "startsWith('hello', 'hel')"),
            script_step_if("s3", "echo ok", "endsWith('hello', 'llo')"),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn real_world_always_and_contains() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step_if(
            "s1",
            "echo conditional ran",
            "always() && contains(needs.*.result, 'success')",
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "needs": { "build": { "result": "success", "outputs": {} } }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

// ─── fromJSON ────────────────────────────────────────────────────────

#[tokio::test]
async fn fromjson_parses_boolean() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step_if("s1", "echo parsed", "fromJSON('true')")],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn fromjson_parses_number() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step_if("s1", "echo parsed", "fromJSON('42') == 42")],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

/// The pattern GitHub documents for JSON outputs: parse a step output and read
/// a property of the result.
#[tokio::test]
async fn fromjson_output_property_in_step_env() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step(
                "org",
                r#"echo 'config={"firebase":{"apiKey":"key-123"},"apps":[{"name":"web"}]}' >> "$GITHUB_OUTPUT""#,
            ),
            script_step_env(
                "deploy",
                r#"test "$API_KEY" = "key-123" && test "$APP" = "web" || exit 1"#,
                HashMap::from([
                    (
                        "API_KEY".to_string(),
                        "${{ fromJSON(steps.org.outputs.config).firebase.apiKey }}".to_string(),
                    ),
                    (
                        "APP".to_string(),
                        "${{ fromJSON(steps.org.outputs.config).apps[0]['name'] }}".to_string(),
                    ),
                ]),
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

// ─── Failed expressions ──────────────────────────────────────────────

/// Like the official runner, an expression that cannot be evaluated fails its
/// step instead of becoming an empty string.
#[tokio::test]
async fn unparsable_expression_in_step_env_fails_the_step() {
    let env = TestEnv::setup().await;
    let marker = env.tmp.path().join("ran");
    let manifest = manifest_with_steps(
        vec![
            script_step_env(
                "deploy",
                &format!("touch '{}'", marker.display()),
                HashMap::from([("X".to_string(), "${{ fromJSON('{}').a. }}".to_string())]),
            ),
            script_step_if(
                "report",
                r#"test "${{ steps.deploy.outcome }}" = "failure" || exit 1"#,
                "failure()",
            ),
        ],
        &env.mock_server.uri(),
    );

    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Failed);
    assert!(!marker.exists(), "the step's script must not run");
}

#[tokio::test]
async fn failed_expression_in_step_env_with_continue_on_error_keeps_the_job_green() {
    let env = TestEnv::setup().await;
    let mut step = script_step_env(
        "deploy",
        "echo never",
        HashMap::from([("X".to_string(), "${{ fromJSON('') }}".to_string())]),
    );
    step["continueOnError"] = serde_json::json!(true);
    let manifest = manifest_with_steps(vec![step], &env.mock_server.uri());

    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn failed_expression_in_script_fails_the_step() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step("s1", "echo ${{ noSuchFunction() }}")],
        &env.mock_server.uri(),
    );

    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Failed);
}

#[tokio::test]
async fn failed_expression_in_if_fails_the_step() {
    let env = TestEnv::setup().await;
    let marker = env.tmp.path().join("ran");
    let manifest = manifest_with_steps(
        vec![
            script_step_if("s1", "echo never", "noSuchFunction()"),
            script_step_if("s2", &format!("touch '{}'", marker.display()), "failure()"),
        ],
        &env.mock_server.uri(),
    );

    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Failed);
    assert!(
        marker.exists(),
        "a failure() step should see the failed step"
    );
}

// ─── Context resolution ──────────────────────────────────────────────

#[tokio::test]
async fn runner_context_in_expressions() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"
            test -n "${{ runner.os }}" || exit 1
            test -n "${{ runner.name }}" || exit 1
            test -n "${{ runner.arch }}" || exit 1
            test -n "${{ runner.temp }}" || exit 1
            "#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn github_context_in_expressions() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step(
            "s1",
            r#"
            test "${{ github.repository }}" = "owner/test-repo" || exit 1
            test "${{ github.event_name }}" = "push" || exit 1
            test "${{ github.actor }}" = "testuser" || exit 1
            "#,
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "github": {
                "repository": "owner/test-repo",
                "event_name": "push",
                "actor": "testuser"
            }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn env_context_in_expressions() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step("s1", r#"echo "CUSTOM=hello" >> "$GITHUB_ENV""#),
            script_step("s2", r#"test "${{ env.CUSTOM }}" = "hello" || exit 1"#),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

// ─── Nested / compound expressions ───────────────────────────────────

#[tokio::test]
async fn nested_expression_format_with_hashfiles() {
    let env = TestEnv::setup().await;
    std::fs::write(env.workspace.workspace_dir().join("lock.txt"), "deps").unwrap();

    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"
            KEY="${{ format('{0}-deps-{1}', runner.os, hashFiles('lock.txt')) }}"
            test -n "$KEY" || exit 1
            echo "$KEY" | grep -q "deps-" || exit 1
            "#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

// ─── Real-world patterns ─────────────────────────────────────────────

#[tokio::test]
async fn real_world_not_contains_failure() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step_if(
            "s1",
            "echo no failures",
            "!contains(needs.*.result, 'failure')",
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "needs": {
                "a": { "result": "success", "outputs": {} },
                "b": { "result": "success", "outputs": {} }
            }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}
