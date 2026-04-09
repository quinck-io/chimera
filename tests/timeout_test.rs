mod common;

use chimera::job::client::JobConclusion;
use common::*;

/// Verify that a step with a timeout set still succeeds when it completes quickly.
/// The actual timeout-kill behavior (process killed after N seconds) is tested
/// at the unit level in execute_test.rs via cancel_token_kills_running_process,
/// which uses the same code path. We don't test a real 60s timeout here because
/// timeoutInMinutes has a minimum granularity of 1 minute.
#[tokio::test]
async fn step_with_timeout_succeeds_when_fast() {
    let env = TestEnv::setup().await;
    let mut step = script_step("s1", "echo done");
    step["timeoutInMinutes"] = serde_json::json!(1);

    let manifest = manifest_with_steps(vec![step], &env.mock_server.uri());
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}
