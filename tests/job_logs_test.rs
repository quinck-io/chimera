mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::*;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct BlobSpy {
    content: Arc<tokio::sync::Mutex<String>>,
    metadata_calls: Arc<AtomicUsize>,
}

/// Mount the Results endpoints for one blob kind, recording what is written to it.
/// Step and job blobs get separate paths so the two can be told apart.
async fn spy_on_blob(server: &MockServer, kind: &str) -> BlobSpy {
    let content = Arc::new(tokio::sync::Mutex::new(String::new()));
    let metadata_calls = Arc::new(AtomicUsize::new(0));
    let blob_path = format!("/{kind}-blob");

    Mock::given(method("POST"))
        .and(path_regex(format!(".*Get{kind}LogsSignedBlobURL$")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "logs_url": format!("{}{blob_path}?sig=x", server.uri()),
            "blob_storage_type": "BLOB_STORAGE_TYPE_AZURE",
        })))
        .mount(server)
        .await;

    let sink = content.clone();
    Mock::given(method("PUT"))
        .and(path_regex(format!("^{blob_path}$")))
        .respond_with(move |req: &wiremock::Request| {
            let body = String::from_utf8_lossy(&req.body).to_string();
            let sink = sink.clone();
            tokio::spawn(async move { sink.lock().await.push_str(&body) });
            ResponseTemplate::new(201)
        })
        .mount(server)
        .await;

    let counter = metadata_calls.clone();
    Mock::given(method("POST"))
        .and(path_regex(format!(".*Create{kind}LogsMetadata$")))
        .respond_with(move |_: &wiremock::Request| {
            counter.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200)
        })
        .mount(server)
        .await;

    BlobSpy {
        content,
        metadata_calls,
    }
}

/// GitHub builds the downloadable log archive from the job-level blob, so a run
/// that only uploads step blobs has no logs to download.
#[tokio::test]
async fn results_run_uploads_job_level_log() {
    let mut env = TestEnv::setup().await;
    let job = spy_on_blob(&env.mock_server, "Job").await;
    let step = spy_on_blob(&env.mock_server, "Step").await;

    let manifest = manifest_with_results_endpoint(
        vec![
            script_step("s1", "echo first-step-output"),
            script_step("s2", "echo second-step-output"),
        ],
        &env.mock_server.uri(),
    );
    env.configure_from_manifest(&manifest);

    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, chimera::job::client::JobConclusion::Succeeded);

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    let job_log = job.content.lock().await;
    assert!(
        job_log.contains("first-step-output") && job_log.contains("second-step-output"),
        "job log should hold every step's output, got: {job_log}"
    );
    assert_eq!(
        job.metadata_calls.load(Ordering::SeqCst),
        1,
        "job log metadata is published once, after the blob is sealed"
    );

    assert!(step.content.lock().await.contains("first-step-output"));
}
