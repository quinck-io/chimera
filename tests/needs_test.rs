mod common;

use chimera::job::client::JobConclusion;
use common::*;

#[tokio::test]
async fn needs_context_outputs() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step(
            "s1",
            r#"test "${{ needs.setup.outputs.greeting }}" = "hello from setup" || exit 1"#,
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "needs": {
                "setup": {
                    "result": "success",
                    "outputs": { "greeting": "hello from setup" }
                }
            }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn needs_result_check() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step_if(
            "s1",
            "echo dependency succeeded",
            "needs.setup.result == 'success'",
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
async fn needs_multiple_outputs() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps_and_context(
        vec![script_step(
            "s1",
            r#"
            test "${{ needs.build.outputs.artifact }}" = "dist.tar.gz" || exit 1
            test "${{ needs.build.outputs.version }}" = "1.0.0" || exit 1
            "#,
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "needs": {
                "build": {
                    "result": "success",
                    "outputs": { "artifact": "dist.tar.gz", "version": "1.0.0" }
                }
            }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}
