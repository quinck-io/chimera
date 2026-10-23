use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use wiremock::matchers::{body_partial_json, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::config::ChimeraPaths;
use crate::github::auth::TokenManager;
use crate::github::broker::{BrokerClient, MessageType};

use super::*;

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

fn make_runner() -> Runner {
    Runner {
        name: "test-runner".into(),
        credentials: crate::config::RunnerCredentials {
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
        paths: ChimeraPaths::new(std::path::PathBuf::from("/tmp/chimera-test")),
        state: None,
        cache_port: 9999,
        cache_scopes: Arc::new(crate::cache::scope::ScopeRegistry::default()),
        stop_jobs: tokio_util::sync::CancellationToken::new(),
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
    assert_eq!(msg.message_type, MessageType::RunnerJobRequest);
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

    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "messageId": 2,
            "messageType": "RunnerJobRequest",
            "body": "{\"runner_request_id\": \"xyz\"}"
        })))
        .mount(&mock_server)
        .await;

    Mock::given(method("DELETE"))
        .and(path("/message/2"))
        .respond_with(ResponseTemplate::new(200))
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
    assert_eq!(msg.message_type, MessageType::RunnerJobRequest);
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

    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(500).set_body_string("error"))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;

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
async fn poll_loop_gives_up_after_max_consecutive_errors() {
    let (mock_server, tm, shutdown_tx) = setup().await;
    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(500).set_body_string("error"))
        .expect(u64::from(super::MAX_CONSECUTIVE_ERRORS))
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

    let err = runner.poll_loop(&broker, &mut rx).await.unwrap_err();

    assert!(
        format!("{err:#}").contains("polling failed 10 times in a row"),
        "unexpected error: {err:#}"
    );
}

#[tokio::test]
async fn poll_loop_error_streak_resets_after_a_successful_poll() {
    let (mock_server, tm, shutdown_tx) = setup().await;
    let almost_max = u64::from(super::MAX_CONSECUTIVE_ERRORS) - 1;
    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(almost_max)
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(202))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(almost_max)
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "messageId": 9,
            "messageType": "RunnerJobRequest",
            "body": "{\"runner_request_id\": \"abc\"}"
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

    assert_eq!(result.expect("should return job message").message_id, 9);
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

    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(202))
        .up_to_n_times(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "messageId": 7,
            "messageType": "RunnerJobRequest",
            "body": "{\"runner_request_id\": \"abc\"}"
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

    // An empty poll after the refresh keeps polling; it must not read as a shutdown.
    assert_eq!(result.expect("should return job message").message_id, 7);
}

#[tokio::test]
async fn poll_loop_fails_on_second_401_in_a_row() {
    let (mock_server, tm, shutdown_tx) = setup().await;
    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(401))
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

    let err = runner.poll_loop(&broker, &mut rx).await.unwrap_err();

    assert!(matches!(
        err.downcast_ref::<BrokerError>(),
        Some(BrokerError::Unauthorized)
    ));
}

#[tokio::test]
async fn unreadable_job_is_completed_as_failed() {
    let (mock_server, tm, _shutdown_tx) = setup().await;
    let uri = mock_server.uri();

    let manifest_json = include_str!("../../tests/fixtures/job_manifest.json");
    let mut message: serde_json::Value = serde_json::from_str(manifest_json).unwrap();
    message["steps"][0]["order"] = serde_json::json!("first");
    message["jobId"] = serde_json::json!("job-001");
    message["resources"]["endpoints"][0]["url"] = serde_json::json!(uri);
    message["variables"]["system.github.results_endpoint"] =
        serde_json::json!({ "value": uri, "isSecret": false });

    Mock::given(method("POST"))
        .and(path("/acquirejob"))
        .respond_with(ResponseTemplate::new(200).set_body_json(message))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(
            "/twirp/.*(WorkflowStepsUpdate|CreateStepLogsMetadata)$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path_regex("/twirp/.*GetStepLogsSignedBlobURL$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "logs_url": format!("{uri}/blob?sig=test"),
            "blob_storage_type": "BLOB_STORAGE_TYPE_S3"
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/blob"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/completejob"))
        .and(body_partial_json(
            serde_json::json!({ "planId": "plan-001", "jobId": "job-001" }),
        ))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let mut job_client = JobClient::new(reqwest::Client::new(), tm, uri.clone(), uri.clone());
    let err = job_client.acquire_job("req-1").await.unwrap_err();
    let unreadable = err.downcast_ref::<UnreadableJob>().unwrap();

    report_unreadable_job(&mut job_client, unreadable, &err).await;
}

fn manifest_with_github(github: serde_json::Value) -> JobManifest {
    let manifest_json = include_str!("../../tests/fixtures/job_manifest.json");
    let mut manifest: JobManifest = serde_json::from_str(manifest_json).unwrap();
    manifest.context_data["github"] = github;
    manifest
}

#[test]
fn cache_scope_comes_from_manifest() {
    let manifest = manifest_with_github(serde_json::json!({
        "repository": "owner/repo",
        "ref": "refs/pull/7/merge",
        "event": { "repository": { "default_branch": "develop" } }
    }));

    let scope = cache_scope(&manifest).unwrap();

    assert_eq!(
        scope,
        CacheScope {
            repo: "owner/repo".into(),
            git_ref: "refs/pull/7/merge".into(),
            fallback_refs: vec!["refs/heads/develop".into()],
        }
    );
}

#[test]
fn cache_scope_of_pull_request_falls_back_to_base_then_default_branch() {
    let manifest = manifest_with_github(serde_json::json!({
        "repository": "owner/repo",
        "ref": "refs/pull/7/merge",
        "base_ref": "release",
        "event": { "repository": { "default_branch": "main" } }
    }));

    let scope = cache_scope(&manifest).unwrap();

    assert_eq!(
        scope.fallback_refs,
        vec![
            "refs/heads/release".to_string(),
            "refs/heads/main".to_string()
        ]
    );
}

#[test]
fn cache_scope_ignores_empty_base_ref() {
    let manifest = manifest_with_github(serde_json::json!({
        "repository": "owner/repo",
        "ref": "refs/heads/feature",
        "base_ref": "",
        "event": { "repository": { "default_branch": "main" } }
    }));

    let scope = cache_scope(&manifest).unwrap();

    assert_eq!(scope.fallback_refs, vec!["refs/heads/main".to_string()]);
}

#[test]
fn cache_scope_requires_ref() {
    let manifest = manifest_with_github(serde_json::json!({ "repository": "owner/repo" }));

    assert_eq!(cache_scope(&manifest), None);
}

#[test]
fn cache_scope_requires_repository() {
    let manifest = manifest_with_github(serde_json::json!({ "ref": "refs/heads/main" }));

    assert_eq!(cache_scope(&manifest), None);
}

#[tokio::test]
async fn shutdown_during_a_job_lets_it_finish_then_deletes_the_session() {
    let (mock_server, _tm, shutdown_tx) = setup().await;
    let uri = mock_server.uri();
    let acquire_delay = Duration::from_secs(2);

    Mock::given(method("POST"))
        .and(path("/session"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({ "sessionId": "session-1" })),
        )
        .mount(&mock_server)
        .await;
    // Every poll offers a job, so a runner that kept polling would acquire a second one.
    Mock::given(method("GET"))
        .and(path("/message"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "messageId": 1,
            "messageType": "RunnerJobRequest",
            "body": serde_json::json!({ "runner_request_id": "req-1", "run_service_url": uri })
                .to_string()
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/acknowledge"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mock_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/acquirejob"))
        .respond_with(ResponseTemplate::new(404).set_delay(acquire_delay))
        .expect(1)
        .mount(&mock_server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/session"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;

    let private_key = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let mut runner = make_runner();
    runner.credentials.oauth.authorization_url = format!("{uri}/oauth2/token");
    runner.credentials.info.server_url_v2 = uri.clone();
    runner.credentials.rsa_params = crate::config::private_key_to_rsa_params(&private_key).unwrap();
    let runner_task = tokio::spawn(runner.start(shutdown_tx.subscribe()));

    wait_for_request(&mock_server, "/acquirejob").await;
    let shutdown_at = tokio::time::Instant::now();
    shutdown_tx.send(true).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), runner_task)
        .await
        .expect("runner should exit after its job")
        .unwrap();

    assert!(result.is_ok(), "runner failed: {result:?}");
    assert!(
        shutdown_at.elapsed() >= acquire_delay / 2,
        "runner exited without waiting for its job"
    );
    mock_server.verify().await;
}

async fn wait_for_request(mock_server: &MockServer, request_path: &str) {
    for _ in 0..500 {
        let requests = mock_server.received_requests().await.unwrap_or_default();
        if requests.iter().any(|r| r.url.path() == request_path) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no request to {request_path} within 5s");
}
