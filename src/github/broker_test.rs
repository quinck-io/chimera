use super::*;
use crate::github::auth::TokenManager;
use rsa::RsaPrivateKey;
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn setup() -> (MockServer, Arc<TokenManager>) {
    let mock_server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/oauth2/token"))
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

    (mock_server, tm)
}

fn make_client(server_url: &str, tm: Arc<TokenManager>) -> BrokerClient {
    BrokerClient::new(
        reqwest::Client::new(),
        server_url.to_string(),
        "session-123".into(),
        tm,
    )
}

// --- Session tests ---

#[tokio::test]
async fn connect_creates_session() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("POST"))
        .and(path("/session"))
        .and(header("authorization", "Bearer test-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sessionId": "session-uuid-123"
        })))
        .mount(&mock_server)
        .await;

    let client = BrokerClient::connect(
        reqwest::Client::new(),
        &mock_server.uri(),
        tm,
        42,
        "chimera-0",
    )
    .await
    .unwrap();

    assert_eq!(client.session_id(), "session-uuid-123");
}

#[tokio::test]
async fn connect_version_rejected() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("POST"))
        .and(path("/session"))
        .respond_with(ResponseTemplate::new(400).set_body_string("runner version too old"))
        .mount(&mock_server)
        .await;

    let result = BrokerClient::connect(
        reqwest::Client::new(),
        &mock_server.uri(),
        tm,
        42,
        "chimera-0",
    )
    .await;

    let err = result.err().expect("should be an error");
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("400"),
        "error should mention 400: {err_msg}"
    );
}

#[tokio::test]
async fn connect_request_body_shape() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("POST"))
        .and(path("/session"))
        .and(body_partial_json(serde_json::json!({
            "useFipsEncryption": false,
            "agent": {
                "version": RUNNER_VERSION,
                "ephemeral": true,
                "status": 0
            }
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sessionId": "abc"
        })))
        .mount(&mock_server)
        .await;

    BrokerClient::connect(reqwest::Client::new(), &mock_server.uri(), tm, 1, "r0")
        .await
        .unwrap();
}

#[tokio::test]
async fn disconnect_success() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("DELETE"))
        .and(path("/session"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;

    let client = make_client(&mock_server.uri(), tm);
    client.disconnect().await.unwrap();
}

#[tokio::test]
async fn disconnect_404_is_ok() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("DELETE"))
        .and(path("/session"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mock_server)
        .await;

    let client = make_client(&mock_server.uri(), tm);
    let result = client.disconnect().await;
    assert!(result.is_ok(), "404 should be treated as success");
}

// --- Poll tests ---

#[tokio::test]
async fn poll_202_returns_none() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("GET"))
        .and(path("/message"))
        .and(query_param("sessionId", "session-123"))
        .respond_with(ResponseTemplate::new(202))
        .expect(1)
        .mount(&mock_server)
        .await;

    let client = make_client(&mock_server.uri(), tm);
    let result = client.poll_message().await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn poll_200_returns_message() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "messageId": 12345,
            "messageType": "RunnerJobRequest",
            "body": "{\"runner_request_id\": \"abc\"}"
        })))
        .mount(&mock_server)
        .await;

    let client = make_client(&mock_server.uri(), tm);
    let msg = client.poll_message().await.unwrap().unwrap();
    assert_eq!(msg.message_id, 12345);
    assert_eq!(msg.message_type, "RunnerJobRequest");
    assert!(msg.body.is_some());
}

#[tokio::test]
async fn ack_job_posts_acknowledge() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("POST"))
        .and(path("/acknowledge"))
        .and(query_param("sessionId", "session-123"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let client = make_client(&mock_server.uri(), tm);
    client.ack_job("request-abc").await.unwrap();
}

#[tokio::test]
async fn delete_message_sends_delete() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("DELETE"))
        .and(path("/message/42"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let client = make_client(&mock_server.uri(), tm);
    client.delete_message(42).await.unwrap();
}

#[tokio::test]
async fn poll_500_returns_error() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    let client = make_client(&mock_server.uri(), tm);
    let result = client.poll_message().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn poll_401_returns_error() {
    let (mock_server, tm) = setup().await;

    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(401))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    let client = make_client(&mock_server.uri(), tm);
    let result = client.poll_message().await;
    let err = result.unwrap_err();
    assert!(
        err.downcast_ref::<BrokerError>()
            .is_some_and(|be| matches!(be, BrokerError::Unauthorized)),
        "expected BrokerError::Unauthorized, got: {err}"
    );
}
