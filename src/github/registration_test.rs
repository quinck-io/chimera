use super::*;
use crate::config::{ChimeraConfig, load_config, save_config};
use tempfile::TempDir;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn parse_repo_url() {
    let target = GitHubTarget::parse("https://github.com/myorg/myrepo").unwrap();
    assert_eq!(
        target,
        GitHubTarget::Repo {
            owner: "myorg".into(),
            repo: "myrepo".into(),
        }
    );
}

#[test]
fn parse_org_url() {
    let target = GitHubTarget::parse("https://github.com/myorg").unwrap();
    assert_eq!(
        target,
        GitHubTarget::Org {
            org: "myorg".into(),
        }
    );
}

#[test]
fn parse_url_with_trailing_slash() {
    let target = GitHubTarget::parse("https://github.com/org/repo/").unwrap();
    assert_eq!(
        target,
        GitHubTarget::Repo {
            owner: "org".into(),
            repo: "repo".into(),
        }
    );
}

#[test]
fn parse_invalid_url_errors() {
    assert!(GitHubTarget::parse("https://gitlab.com/org/repo").is_err());
    assert!(GitHubTarget::parse("https://github.com/a/b/c").is_err());
    assert!(GitHubTarget::parse("not-a-url").is_err());
}

#[tokio::test]
async fn v1_registration_via_pipelines() {
    let mock_server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(wiremock::matchers::path_regex("/_apis/connectionData.*"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "locationServiceData": {
                "serviceDefinitions": []
            }
        })))
        .mount(&mock_server)
        .await;

    Mock::given(method("POST"))
        .and(wiremock::matchers::path_regex(
            "/_apis/distributedtask/pools/1/agents.*",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 42,
            "name": "test-runner",
            "authorization": {
                "authorizationUrl": format!("{}/oauth2/token", mock_server.uri()),
                "clientId": "client-id-xyz"
            }
        })))
        .mount(&mock_server)
        .await;

    let auth = GitHubAuthResult {
        url: mock_server.uri(),
        token: "test-oauth-token".into(),
        extra: serde_json::Value::Object(serde_json::Map::new()),
    };

    let private_key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let result = register_v1(
        &reqwest::Client::new(),
        &auth,
        "test-runner",
        &private_key,
        &[],
    )
    .await
    .unwrap();

    assert_eq!(result.agent_id, 42);
    assert_eq!(result.agent_name, "test-runner");
    assert_eq!(result.client_id, "client-id-xyz");
    assert!(result.authorization_url.contains("/oauth2/token"));
}

#[tokio::test]
async fn unregister_removes_files() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    let runner_dir = root.join("runners").join("test-runner");
    std::fs::create_dir_all(&runner_dir).unwrap();
    std::fs::write(runner_dir.join("runner.json"), "{}").unwrap();
    std::fs::write(runner_dir.join("credentials.json"), "{}").unwrap();

    let config = ChimeraConfig {
        runners: vec!["test-runner".into(), "other-runner".into()],
        ..Default::default()
    };
    save_config(&root.join("config.toml"), &config).unwrap();

    unregister("test-runner", root).await.unwrap();

    assert!(!runner_dir.exists());

    let updated_config = load_config(&root.join("config.toml")).unwrap();
    assert_eq!(updated_config.runners, vec!["other-runner"]);
}
