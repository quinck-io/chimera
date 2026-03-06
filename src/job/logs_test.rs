use super::*;
use crate::github::auth::TokenManager;
use crate::utils::format_log_timestamp;
use rsa::RsaPrivateKey;
use wiremock::matchers::{header, method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn setup_log_server() -> (MockServer, Arc<JobClient>) {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path_regex("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "test-token",
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

    let mut job_client = super::super::JobClient::new(
        reqwest::Client::new(),
        tm,
        mock_server.uri(),
        mock_server.uri(),
    );
    job_client.set_job_access_token("test-job-token".into());

    (mock_server, Arc::new(job_client))
}

#[test]
fn format_log_timestamp_seven_decimal_places() {
    use chrono::TimeZone;
    let ts = Utc.with_ymd_and_hms(2024, 6, 15, 12, 30, 45).unwrap();
    let formatted = format_log_timestamp(ts);
    assert_eq!(formatted, "2024-06-15T12:30:45.0000000Z");
    assert!(formatted.contains(".0000000Z"));
}

#[tokio::test]
async fn flush_on_sender_drop() {
    let (mock_server, client) = setup_log_server().await;

    Mock::given(method("POST"))
        .and(path_regex(r"/_apis/pipelines/workflows/.*/logs/\d+"))
        .and(header("Content-Type", "application/octet-stream"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let masks = Arc::new(RwLock::new(Vec::new()));
    let (sender, handle) = start_log_upload(client, "plan-1".into(), 1, masks);

    sender.send("hello world".into()).await;
    drop(sender);

    handle.await.unwrap();
}

#[tokio::test]
async fn flush_on_interval() {
    let (mock_server, client) = setup_log_server().await;

    Mock::given(method("POST"))
        .and(path_regex(r"/_apis/pipelines/workflows/.*/logs/\d+"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1..)
        .mount(&mock_server)
        .await;

    let masks = Arc::new(RwLock::new(Vec::new()));
    let (sender, handle) = start_log_upload(client, "plan-1".into(), 1, masks);

    sender.send("line 1".into()).await;

    tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;

    drop(sender);
    handle.await.unwrap();
}

#[tokio::test]
async fn flush_on_large_buffer() {
    let (mock_server, client) = setup_log_server().await;

    Mock::given(method("POST"))
        .and(path_regex(r"/_apis/pipelines/workflows/.*/logs/\d+"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1..)
        .mount(&mock_server)
        .await;

    let masks = Arc::new(RwLock::new(Vec::new()));
    let (sender, handle) = start_log_upload(client, "plan-1".into(), 1, masks);

    let big_line = "x".repeat(70_000);
    sender.send(big_line).await;

    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    drop(sender);
    handle.await.unwrap();
}

#[tokio::test]
async fn masking_replaces_secrets() {
    let (mock_server, client) = setup_log_server().await;

    let uploaded = Arc::new(tokio::sync::Mutex::new(String::new()));
    let uploaded_clone = uploaded.clone();

    Mock::given(method("POST"))
        .and(path_regex(r"/_apis/pipelines/workflows/.*/logs/\d+"))
        .respond_with(move |req: &wiremock::Request| {
            let body = String::from_utf8_lossy(&req.body).to_string();
            let uploaded = uploaded_clone.clone();
            tokio::spawn(async move {
                let mut guard = uploaded.lock().await;
                guard.push_str(&body);
            });
            ResponseTemplate::new(200)
        })
        .mount(&mock_server)
        .await;

    let masks = Arc::new(RwLock::new(vec!["supersecret".to_string()]));
    let (sender, handle) = start_log_upload(client, "plan-1".into(), 1, masks);

    sender.send("my password is supersecret here".into()).await;
    drop(sender);
    handle.await.unwrap();

    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    let content = uploaded.lock().await;
    assert!(!content.contains("supersecret"), "secret should be masked");
    assert!(content.contains("***"), "should contain mask replacement");
}

#[tokio::test]
async fn collector_collects_lines() {
    let masks = Arc::new(RwLock::new(Vec::new()));
    let (sender, handle) = start_log_collector(masks);

    sender.send("line one".into()).await;
    sender.send("line two".into()).await;
    drop(sender);

    let (text, count) = handle.await.unwrap();
    assert_eq!(count, 2);
    assert!(text.contains("line one"));
    assert!(text.contains("line two"));
}

#[tokio::test]
async fn collector_masks_secrets() {
    let masks = Arc::new(RwLock::new(vec!["secret123".to_string()]));
    let (sender, handle) = start_log_collector(masks);

    sender.send("token is secret123 here".into()).await;
    drop(sender);

    let (text, _) = handle.await.unwrap();
    assert!(!text.contains("secret123"));
    assert!(text.contains("***"));
}
