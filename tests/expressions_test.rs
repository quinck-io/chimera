mod common;

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
