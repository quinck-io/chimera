mod common;

use std::collections::HashMap;

use chimera::job::client::JobConclusion;
use common::*;

#[tokio::test]
async fn secrets_from_context_data() {
    let env = TestEnv::setup().await;
    let step = script_step_env(
        "s1",
        r#"test "$MY_SECRET" = "super-secret-value" || exit 1"#,
        HashMap::from([("MY_SECRET".into(), "${{ secrets.TEST_SECRET }}".into())]),
    );
    let manifest = manifest_with_steps_and_context(
        vec![step],
        &env.mock_server.uri(),
        serde_json::json!({
            "secrets": { "TEST_SECRET": "super-secret-value" }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn missing_secret_resolves_empty() {
    let env = TestEnv::setup().await;
    let step = script_step_env(
        "s1",
        r#"test -z "$MY_SECRET" || exit 1"#,
        HashMap::from([("MY_SECRET".into(), "${{ secrets.NONEXISTENT }}".into())]),
    );
    let manifest = manifest_with_steps(vec![step], &env.mock_server.uri());
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn multiple_secrets() {
    let env = TestEnv::setup().await;
    let step = script_step_env(
        "s1",
        r#"
        test "$SECRET_A" = "value_a" || exit 1
        test "$SECRET_B" = "value_b" || exit 1
        "#,
        HashMap::from([
            ("SECRET_A".into(), "${{ secrets.A }}".into()),
            ("SECRET_B".into(), "${{ secrets.B }}".into()),
        ]),
    );
    let manifest = manifest_with_steps_and_context(
        vec![step],
        &env.mock_server.uri(),
        serde_json::json!({
            "secrets": { "A": "value_a", "B": "value_b" }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}
