use super::*;

/// Integration test: requires Docker daemon running.
#[tokio::test]
#[ignore]
async fn exec_echo_in_container() {
    let docker = crate::docker::client::connect(None).unwrap();
    crate::docker::client::ping(&docker).await.unwrap();
    crate::docker::client::ensure_image(&docker, "alpine:latest", None)
        .await
        .unwrap();

    let name = format!("chimera-exec-test-{}", uuid::Uuid::new_v4());

    use bollard::container::{Config, CreateContainerOptions, RemoveContainerOptions};
    let config = Config {
        image: Some("alpine:latest"),
        cmd: Some(vec!["tail", "-f", "/dev/null"]),
        ..Default::default()
    };
    let container = docker
        .create_container(
            Some(CreateContainerOptions {
                name: name.as_str(),
                ..Default::default()
            }),
            config,
        )
        .await
        .unwrap();
    docker
        .start_container::<String>(&container.id, None)
        .await
        .unwrap();

    let masks = Arc::new(RwLock::new(Vec::new()));
    let (tx, _rx) = tokio::sync::mpsc::channel(256);
    let sender = LogSender::new_for_test(tx, masks.clone());
    let mut job_state = JobState::new(masks, HashMap::new(), serde_json::json!({}));

    let result = docker_exec(
        &docker,
        &container.id,
        vec!["echo".into(), "hello".into()],
        &HashMap::new(),
        "/",
        &mut job_state,
        &sender,
        Duration::from_secs(30),
        &CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(result.conclusion, StepConclusion::Succeeded);

    // Cleanup
    let _ = docker.stop_container(&container.id, None).await;
    let _ = docker
        .remove_container(
            &container.id,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
}

/// Integration test: verify a failing command returns Failed.
#[tokio::test]
#[ignore]
async fn exec_failing_command() {
    let docker = crate::docker::client::connect(None).unwrap();
    crate::docker::client::ping(&docker).await.unwrap();
    crate::docker::client::ensure_image(&docker, "alpine:latest", None)
        .await
        .unwrap();

    let name = format!("chimera-exec-fail-{}", uuid::Uuid::new_v4());

    use bollard::container::{Config, CreateContainerOptions, RemoveContainerOptions};
    let config = Config {
        image: Some("alpine:latest"),
        cmd: Some(vec!["tail", "-f", "/dev/null"]),
        ..Default::default()
    };
    let container = docker
        .create_container(
            Some(CreateContainerOptions {
                name: name.as_str(),
                ..Default::default()
            }),
            config,
        )
        .await
        .unwrap();
    docker
        .start_container::<String>(&container.id, None)
        .await
        .unwrap();

    let masks = Arc::new(RwLock::new(Vec::new()));
    let (tx, _rx) = tokio::sync::mpsc::channel(256);
    let sender = LogSender::new_for_test(tx, masks.clone());
    let mut job_state = JobState::new(masks, HashMap::new(), serde_json::json!({}));

    let result = docker_exec(
        &docker,
        &container.id,
        vec!["sh".into(), "-c".into(), "exit 1".into()],
        &HashMap::new(),
        "/",
        &mut job_state,
        &sender,
        Duration::from_secs(30),
        &CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(result.conclusion, StepConclusion::Failed);

    let _ = docker.stop_container(&container.id, None).await;
    let _ = docker
        .remove_container(
            &container.id,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
}
