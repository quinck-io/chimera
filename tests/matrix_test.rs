mod common;

use chimera::job::client::JobConclusion;
use common::*;

#[tokio::test]
async fn matrix_context_resolves() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step(
            "s1",
            r#"
            test "${{ matrix.os }}" = "ubuntu-latest" || exit 1
            test "${{ matrix.node }}" = "20" || exit 1
            "#,
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "matrix": {
                "os": "ubuntu-latest",
                "node": "20"
            }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn matrix_context_in_expressions() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step_if(
            "s1",
            "echo matrix condition met",
            "matrix.os == 'ubuntu-latest'",
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "matrix": { "os": "ubuntu-latest" }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn matrix_context_in_env() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![{
            let mut step = script_step("s1", r#"test "$TARGET" = "x86_64" || exit 1"#);
            step["environment"] = serde_json::json!({ "TARGET": "${{ matrix.arch }}" });
            step
        }],
        &env.mock_server.uri(),
        serde_json::json!({
            "matrix": { "arch": "x86_64" }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn matrix_tojson() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step(
            "s1",
            "JSON='${{ toJSON(matrix) }}'\necho \"$JSON\" | grep -q 'ubuntu' || exit 1",
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "matrix": { "os": "ubuntu-latest", "node": "20" }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn strategy_context_resolves() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step(
            "s1",
            r#"
            test "${{ strategy.fail-fast }}" = "true" || exit 1
            test "${{ strategy.job-index }}" = "0" || exit 1
            test "${{ strategy.job-total }}" = "3" || exit 1
            "#,
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "strategy": {
                "fail-fast": true,
                "job-index": 0,
                "job-total": 3
            }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}
