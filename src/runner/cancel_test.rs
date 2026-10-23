use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::github::auth::TokenManager;
use crate::github::broker::BrokerClient;

use crate::job::cancel::JobCancel;

use tokio_util::sync::CancellationToken;

use super::{SHUTDOWN_CANCEL_GRACE, cancel_grace, spawn_cancel_poller, spawn_shutdown_canceller};

async fn setup() -> (MockServer, Arc<TokenManager>, watch::Sender<bool>) {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "test-token",
            "expires_in": 7200
        })))
        .mount(&mock_server)
        .await;

    let private_key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();

    let tm = Arc::new(TokenManager::new(
        reqwest::Client::new(),
        format!("{}/oauth2/token", mock_server.uri()),
        private_key,
        "test-client".into(),
    ));

    let (shutdown_tx, _shutdown_rx) = watch::channel(false);

    (mock_server, tm, shutdown_tx)
}

#[tokio::test]
async fn cancel_poller_triggers_token_on_cancellation() {
    let (mock_server, tm, _shutdown_tx) = setup().await;

    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "messageId": 42,
            "messageType": "JobCancellation",
            "body": "{\"jobId\": \"job-123\"}"
        })))
        .mount(&mock_server)
        .await;

    let broker = BrokerClient::new(
        reqwest::Client::new(),
        mock_server.uri(),
        "session-123".into(),
        tm,
    );

    let cancel = JobCancel::new();
    let handle = spawn_cancel_poller(&broker, cancel.clone());

    wait_until(|| async { cancel.is_requested() }).await;

    assert!(cancel.is_requested());
    cancel.finish();
    let _ = handle.await;
}

#[tokio::test]
async fn cancel_poller_keeps_watching_after_the_first_cancellation() {
    let (mock_server, tm, _shutdown_tx) = setup().await;
    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "messageId": 42,
            "messageType": "JobCancellation",
            "body": "{\"jobId\": \"job-123\"}"
        })))
        .mount(&mock_server)
        .await;
    let broker = BrokerClient::new(
        reqwest::Client::new(),
        mock_server.uri(),
        "session-123".into(),
        tm,
    );
    let cancel = JobCancel::new();

    let handle = spawn_cancel_poller(&broker, cancel.clone());
    wait_until(|| async { cancel.is_requested() }).await;
    let polls_at_cancel = message_polls(&mock_server).await;
    wait_until(|| async { message_polls(&mock_server).await > polls_at_cancel + 1 }).await;

    assert!(!handle.is_finished());
    cancel.finish();
    let _ = handle.await;
}

async fn message_polls(mock_server: &MockServer) -> usize {
    mock_server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path() == "/message")
        .count()
}

async fn wait_until<F: std::future::Future<Output = bool>>(condition: impl Fn() -> F) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("condition not met within 5s");
}

#[tokio::test]
async fn cancel_poller_stops_when_the_job_finishes() {
    let (mock_server, tm, _shutdown_tx) = setup().await;

    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&mock_server)
        .await;

    let broker = BrokerClient::new(
        reqwest::Client::new(),
        mock_server.uri(),
        "session-123".into(),
        tm,
    );

    let cancel = JobCancel::new();
    let handle = spawn_cancel_poller(&broker, cancel.clone());

    cancel.finish();

    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("poller should exit within 5s")
        .expect("poller task should not panic");
}

#[test]
fn cancel_grace_is_the_timeout_less_15_seconds() {
    assert_eq!(
        cancel_grace(Some(Duration::from_secs(300))),
        Duration::from_secs(285)
    );
}

#[test]
fn cancel_grace_gives_at_least_45_seconds() {
    assert_eq!(cancel_grace(Some(Duration::ZERO)), Duration::from_secs(45));
}

#[test]
fn cancel_grace_defaults_to_five_minutes_less_15_seconds() {
    assert_eq!(cancel_grace(None), Duration::from_secs(285));
}

#[tokio::test(start_paused = true)]
async fn shutdown_canceller_cancels_the_job_with_a_grace() {
    let stop_jobs = CancellationToken::new();
    let cancel = JobCancel::new();
    let handle = spawn_shutdown_canceller(stop_jobs.clone(), cancel.clone());

    stop_jobs.cancel();
    handle.await.unwrap();

    assert!(cancel.is_requested());
    assert!(!cancel.step_token(true).is_cancelled());
    tokio::time::sleep(SHUTDOWN_CANCEL_GRACE + Duration::from_millis(1)).await;
    assert!(cancel.step_token(true).is_cancelled());
}

#[tokio::test]
async fn shutdown_canceller_exits_when_the_job_finishes() {
    let cancel = JobCancel::new();
    let handle = spawn_shutdown_canceller(CancellationToken::new(), cancel.clone());

    cancel.finish();

    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("canceller should exit within 5s")
        .expect("canceller task should not panic");
    assert!(!cancel.is_requested());
}
