use super::*;
use crate::github::auth::TokenManager;
use crate::job::timeline;
use rsa::RsaPrivateKey;
use wiremock::matchers::{body_json, header, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn setup() -> (MockServer, Arc<TokenManager>) {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "test-oauth-token",
            "expires_in": 7200
        })))
        .mount(&mock_server)
        .await;

    let private_key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let tm = Arc::new(TokenManager::new(
        reqwest::Client::new(),
        format!("{}/oauth2/token", mock_server.uri()),
        private_key,
        "test-client".into(),
    ));

    (mock_server, tm)
}

fn make_client(
    mock_server: &MockServer,
    token_manager: Arc<TokenManager>,
    job_token: bool,
) -> JobClient {
    let mut client = JobClient::new(
        reqwest::Client::new(),
        token_manager,
        mock_server.uri(),
        mock_server.uri(),
    );
    if job_token {
        client.set_job_access_token("test-job-token".into());
    }
    client
}

#[tokio::test]
async fn acquire_job_correct_body_and_response() {
    let (mock_server, tm) = setup().await;

    let manifest_json = include_str!("../../tests/fixtures/job_manifest.json");
    let manifest_value: serde_json::Value = serde_json::from_str(manifest_json).unwrap();

    Mock::given(method("POST"))
        .and(path("/acquirejob"))
        .and(body_json(serde_json::json!({
            "jobMessageId": "req-123",
            "runnerOS": "Linux",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(manifest_value))
        .expect(1)
        .mount(&mock_server)
        .await;

    let client = make_client(&mock_server, tm, false);
    let manifest = client.acquire_job("req-123").await.unwrap();
    assert_eq!(manifest.plan.plan_id, "plan-001");
    assert_eq!(manifest.steps.len(), 2);
}

#[tokio::test]
async fn acquire_job_timeout_error() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("POST"))
        .and(path("/acquirejob"))
        .respond_with(ResponseTemplate::new(408).set_body_string("timeout"))
        .mount(&mock_server)
        .await;

    let client = make_client(&mock_server, tm, false);
    let result = client.acquire_job("req-123").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn renew_job_correct_body() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("POST"))
        .and(path("/renewjob"))
        .and(body_json(serde_json::json!({
            "planId": "plan-1",
            "jobId": "job-1",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "lockedUntil": "2024-01-01T00:10:00Z"
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let client = make_client(&mock_server, tm, true);
    client.renew_job("plan-1", "job-1").await.unwrap();
}

#[tokio::test]
async fn complete_job_sends_conclusion_and_outputs() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("POST"))
        .and(path("/completejob"))
        .and(body_json(serde_json::json!({
            "planId": "plan-1",
            "jobId": "job-1",
            "conclusion": "succeeded",
            "outputs": {},
            "stepResults": [],
        })))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let client = make_client(&mock_server, tm, true);
    client
        .complete_job(
            "plan-1",
            "job-1",
            super::JobConclusion::Succeeded,
            &serde_json::json!({}),
            &[],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn create_log_returns_id() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("POST"))
        .and(path_regex(r"/_apis/pipelines/workflows/.*/logs$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 42
        })))
        .expect(1)
        .mount(&mock_server)
        .await;

    let client = make_client(&mock_server, tm, true);
    let log_id = client.create_log("plan-1", "step-log").await.unwrap();
    assert_eq!(log_id, 42);
}

#[tokio::test]
async fn upload_log_lines_sends_text_plain() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("POST"))
        .and(path_regex(r"/_apis/pipelines/workflows/.*/logs/\d+"))
        .and(header("Content-Type", "application/octet-stream"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let client = make_client(&mock_server, tm, true);
    client
        .upload_log_lines("plan-1", 42, "2024-01-01T00:00:00.0000000Z hello\n")
        .await
        .unwrap();
}

#[tokio::test]
async fn update_timeline_sends_patch() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("PATCH"))
        .and(path_regex(
            r"/_apis/pipelines/workflows/.*/timelines/.*/records",
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let client = make_client(&mock_server, tm, true);
    let records = vec![timeline::TimelineRecord {
        id: "step-1".into(),
        state: Some(timeline::TimelineState::InProgress),
        result: None,
        start_time: Some("2024-01-01T00:00:00.0000000Z".into()),
        finish_time: None,
        name: Some("Run tests".into()),
        order: Some(1),
        log: None,
    }];

    client
        .update_timeline("plan-1", "timeline-1", &records)
        .await
        .unwrap();
}
