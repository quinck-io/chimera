use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::github::auth::TokenManager;
use crate::github::broker::BrokerClient;

use super::spawn_cancel_poller;

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
        .expect(1)
        .mount(&mock_server)
        .await;

    let broker = BrokerClient::new(
        reqwest::Client::new(),
        mock_server.uri(),
        "session-123".into(),
        tm,
    );

    let cancel_token = CancellationToken::new();
    let handle = spawn_cancel_poller(&broker, cancel_token.clone());

    tokio::time::timeout(Duration::from_secs(5), cancel_token.cancelled())
        .await
        .expect("cancel token should be triggered within 5s");

    assert!(cancel_token.is_cancelled());
    let _ = handle.await;
}

#[tokio::test]
async fn cancel_poller_stops_when_token_cancelled_externally() {
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

    let cancel_token = CancellationToken::new();
    let handle = spawn_cancel_poller(&broker, cancel_token.clone());

    cancel_token.cancel();

    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("poller should exit within 5s")
        .expect("poller task should not panic");
}
