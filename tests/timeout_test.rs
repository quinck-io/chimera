mod common;

use chimera::job::client::JobConclusion;
use common::*;

/// A step that exceeds its timeout gets killed and fails the job.
/// Uses timeoutInMinutes=1 (60s) with a sleep that would exceed it.
/// Marked #[ignore] because it takes ~60s to run.
#[tokio::test]
#[ignore]
async fn step_killed_on_timeout() {
    let env = TestEnv::setup().await;
    let mut step = script_step("s1", "sleep 120");
    step["timeoutInMinutes"] = serde_json::json!(1);

    let manifest = manifest_with_steps(vec![step], &env.mock_server.uri());

    let start = std::time::Instant::now();
    let (conclusion, _) = env.run(&manifest).await.unwrap();

    // Should fail due to timeout, not run for 120s
    assert_eq!(conclusion, JobConclusion::Failed);
    assert!(start.elapsed().as_secs() < 90, "should timeout around 60s");
}
