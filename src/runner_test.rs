use super::*;
use crate::github::auth::TokenManager;
use crate::github::broker::BrokerClient;
use rsa::RsaPrivateKey;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

    let private_key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();

    let tm = Arc::new(TokenManager::new(
        reqwest::Client::new(),
        format!("{}/oauth2/token", mock_server.uri()),
        private_key,
        "test-client".into(),
    ));

    let (shutdown_tx, _shutdown_rx) = watch::channel(false);

    (mock_server, tm, shutdown_tx)
}

fn make_runner() -> Runner {
    // Credentials aren't used directly — poll_loop takes a BrokerClient
    Runner {
        name: "test-runner".into(),
        credentials: RunnerCredentials {
            info: crate::config::RunnerInfo {
                agent_id: 1,
                agent_name: "test".into(),
                pool_id: 1,
                server_url: "http://unused".into(),
                server_url_v2: "http://unused".into(),
                git_hub_url: "http://unused".into(),
                work_folder: "_work".into(),
                use_v2_flow: true,
            },
            oauth: crate::config::OAuthCredentials {
                scheme: "OAuth".into(),
                client_id: "unused".into(),
                authorization_url: "http://unused".into(),
            },
            rsa_params: crate::config::RsaParameters {
                d: String::new(),
                dp: String::new(),
                dq: String::new(),
                exponent: String::new(),
                inverse_q: String::new(),
                modulus: String::new(),
                p: String::new(),
                q: String::new(),
            },
        },
    }
}

#[tokio::test]
async fn poll_loop_returns_job_request() {
    let (mock_server, tm, shutdown_tx) = setup().await;

    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "messageId": 99,
            "messageType": "RunnerJobRequest",
            "body": "{\"runner_request_id\": \"abc123\"}"
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

    let runner = make_runner();
    let mut rx = shutdown_tx.subscribe();
    let result = runner.poll_loop(&broker, &mut rx).await.unwrap();
    let msg = result.expect("should return job message");
    assert_eq!(msg.message_id, 99);
    assert_eq!(msg.message_type, "RunnerJobRequest");
}

#[tokio::test]
async fn poll_loop_skips_control_then_returns_job() {
    let (mock_server, tm, shutdown_tx) = setup().await;

    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "messageId": 1,
            "messageType": "AgentRefresh",
            "body": null
        })))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("DELETE"))
        .and(path("/message/1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "messageId": 2,
            "messageType": "RunnerJobRequest",
            "body": "{\"runner_request_id\": \"xyz\"}"
        })))
        .mount(&mock_server)
        .await;

    let broker = BrokerClient::new(
        reqwest::Client::new(),
        mock_server.uri(),
        "session-123".into(),
        tm,
    );

    let runner = make_runner();
    let mut rx = shutdown_tx.subscribe();
    let result = runner.poll_loop(&broker, &mut rx).await.unwrap();
    let msg = result.expect("should return job after skipping control message");
    assert_eq!(msg.message_id, 2);
    assert_eq!(msg.message_type, "RunnerJobRequest");
}

#[tokio::test]
async fn poll_loop_shutdown_returns_none() {
    let (mock_server, tm, shutdown_tx) = setup().await;

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

    let mut rx = shutdown_tx.subscribe();
    shutdown_tx.send(true).unwrap();

    let runner = make_runner();
    let result = runner.poll_loop(&broker, &mut rx).await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn poll_loop_backoff_on_error() {
    let (mock_server, tm, shutdown_tx) = setup().await;

    // First request: 500 error
    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(500).set_body_string("error"))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    // Second request (after backoff): 202 → then shutdown
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

    let shutdown_tx_clone = shutdown_tx.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let _ = shutdown_tx_clone.send(true);
    });

    let runner = make_runner();
    let mut rx = shutdown_tx.subscribe();
    let result = runner.poll_loop(&broker, &mut rx).await.unwrap();
    assert!(
        result.is_none(),
        "should return None on shutdown after backoff"
    );
}

#[tokio::test]
async fn poll_loop_refreshes_token_on_401() {
    let (mock_server, tm, shutdown_tx) = setup().await;

    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(401))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

    // After token refresh, return 202
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

    let runner = make_runner();
    let mut rx = shutdown_tx.subscribe();
    let result = runner.poll_loop(&broker, &mut rx).await.unwrap();
    // After 401 → invalidate → retry → 202 (None) → return Ok(None)
    assert!(result.is_none());
}
