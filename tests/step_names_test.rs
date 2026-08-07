mod common;

use std::sync::Arc;

use common::*;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Record every step name chimera reports to the Results API.
async fn capture_step_updates(server: &MockServer) -> Arc<tokio::sync::Mutex<Vec<String>>> {
    let names = Arc::new(tokio::sync::Mutex::new(Vec::new()));

    let sink = names.clone();
    Mock::given(method("POST"))
        .and(path_regex(".*WorkflowStepsUpdate$"))
        .respond_with(move |req: &wiremock::Request| {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap_or_default();
            let reported: Vec<String> = body["steps"]
                .as_array()
                .map(|steps| {
                    steps
                        .iter()
                        .filter_map(|s| s["name"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let sink = sink.clone();
            tokio::spawn(async move { sink.lock().await.extend(reported) });
            ResponseTemplate::new(200)
        })
        .mount(server)
        .await;

    names
}

/// A `name:` carrying an expression reaches the runner as a raw template — GitHub
/// does not resolve it server-side — so the step must not be reported under it.
#[tokio::test]
async fn templated_step_name_is_resolved_before_reporting() {
    let mut env = TestEnv::setup().await;
    let names = capture_step_updates(&env.mock_server).await;

    let mut step = script_step("s1", "echo hi");
    step["displayName"] = serde_json::json!("Build on ${{ runner.name }}");

    let manifest = manifest_with_results_endpoint(vec![step], &env.mock_server.uri());
    env.configure_from_manifest(&manifest);

    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, chimera::job::client::JobConclusion::Succeeded);

    let names = names.lock().await;
    assert!(
        names.iter().all(|n| !n.contains("${{")),
        "no step should be reported with an unresolved name, got: {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "Build on test-runner"),
        "expected the resolved name, got: {names:?}"
    );
}
