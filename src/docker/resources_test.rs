use super::*;

#[test]
fn parse_port_bindings_simple() {
    let ports = vec!["8080:8080".into()];
    let bindings = parse_port_bindings(&ports);
    assert!(bindings.contains_key("8080/tcp"));
    let binding = bindings["8080/tcp"].as_ref().unwrap();
    assert_eq!(binding[0].host_port.as_deref(), Some("8080"));
}

#[test]
fn parse_port_bindings_with_protocol() {
    let ports = vec!["5432:5432/tcp".into()];
    let bindings = parse_port_bindings(&ports);
    assert!(bindings.contains_key("5432/tcp"));
}

#[test]
fn parse_port_bindings_different_ports() {
    let ports = vec!["3000:80".into()];
    let bindings = parse_port_bindings(&ports);
    let binding = bindings["80/tcp"].as_ref().unwrap();
    assert_eq!(binding[0].host_port.as_deref(), Some("3000"));
}

#[test]
fn parse_port_bindings_empty() {
    let ports: Vec<String> = vec![];
    let bindings = parse_port_bindings(&ports);
    assert!(bindings.is_empty());
}

#[test]
fn remap_to_container_path_works() {
    use std::path::PathBuf;
    let docker = bollard::Docker::connect_with_http_defaults().expect("create bollard HTTP client");
    let mut resources = JobDockerResources::new(docker);
    resources.path_mappings = vec![
        (PathBuf::from("/host/workspace"), "/github/workspace".into()),
        (PathBuf::from("/host/actions"), "/github/actions".into()),
        (
            PathBuf::from("/host/tool-cache"),
            "/github/tool-cache".into(),
        ),
    ];

    assert_eq!(
        resources
            .remap_to_container_path(Path::new("/host/workspace/src/main.rs"))
            .unwrap(),
        "/github/workspace/src/main.rs"
    );
    assert_eq!(
        resources
            .remap_to_container_path(Path::new("/host/actions/actions/checkout/v4"))
            .unwrap(),
        "/github/actions/actions/checkout/v4"
    );
    assert_eq!(
        resources
            .remap_to_container_path(Path::new("/host/workspace"))
            .unwrap(),
        "/github/workspace"
    );
    assert!(
        resources
            .remap_to_container_path(Path::new("/other/path"))
            .is_none()
    );
}

/// Integration test: requires Docker daemon.
/// Uses a unique job ID per run to avoid network name collisions.
#[tokio::test]
#[ignore]
async fn setup_and_cleanup_job_container() {
    let docker = crate::docker::client::connect(None).unwrap();
    crate::docker::client::ping(&docker).await.unwrap();

    let job_id = uuid::Uuid::new_v4().to_string();
    let mut resources = JobDockerResources::new(docker);

    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    let workflow = tmp.path().join("workflow");
    let runner_temp = tmp.path().join("tmp");
    let actions = tmp.path().join("actions");
    let tool_cache = tmp.path().join("tool-cache");
    let externals = tmp.path().join("externals");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&workflow).unwrap();
    std::fs::create_dir_all(&runner_temp).unwrap();
    std::fs::create_dir_all(&actions).unwrap();
    std::fs::create_dir_all(&tool_cache).unwrap();
    std::fs::create_dir_all(&externals).unwrap();

    let job_spec = JobContainerSpec {
        image: "alpine:latest".into(),
        environment: HashMap::new(),
        ports: vec![],
        volumes: vec![],
        options: None,
        credentials: None,
    };

    resources
        .setup(&SetupParams {
            runner_name: "test-runner",
            job_id: &job_id,
            job_container: Some(&job_spec),
            services: &[],
            workspace_host_path: &workspace,
            workflow_files_host_path: &workflow,
            runner_temp_host_path: &runner_temp,
            actions_host_path: &actions,
            tool_cache_host_path: &tool_cache,
            externals_dir: &externals,
        })
        .await
        .unwrap();

    assert!(resources.job_container_id().is_some());

    // Verify the container is actually running
    let id = resources.job_container_id().unwrap();
    let inspect = resources
        .docker()
        .inspect_container(id, None)
        .await
        .unwrap();
    let running = inspect
        .state
        .as_ref()
        .and_then(|s| s.running)
        .unwrap_or(false);
    assert!(running, "job container should be running");

    resources.cleanup().await;

    assert!(resources.job_container_id().is_none());
}

/// Integration test: job container with a service container on a shared network.
#[tokio::test]
#[ignore]
async fn setup_and_cleanup_with_service() {
    let docker = crate::docker::client::connect(None).unwrap();
    crate::docker::client::ping(&docker).await.unwrap();

    // Use nginx as the service — it stays running and gets an IP
    crate::docker::client::ensure_image(&docker, "nginx:alpine", None)
        .await
        .unwrap();

    let job_id = uuid::Uuid::new_v4().to_string();
    let mut resources = JobDockerResources::new(docker);

    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    let workflow = tmp.path().join("workflow");
    let runner_temp = tmp.path().join("tmp");
    let actions = tmp.path().join("actions");
    let tool_cache = tmp.path().join("tool-cache");
    let externals = tmp.path().join("externals");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&workflow).unwrap();
    std::fs::create_dir_all(&runner_temp).unwrap();
    std::fs::create_dir_all(&actions).unwrap();
    std::fs::create_dir_all(&tool_cache).unwrap();
    std::fs::create_dir_all(&externals).unwrap();

    let job_spec = JobContainerSpec {
        image: "alpine:latest".into(),
        environment: HashMap::new(),
        ports: vec![],
        volumes: vec![],
        options: None,
        credentials: None,
    };

    let service_spec = ServiceContainerSpec {
        image: "nginx:alpine".into(),
        ports: vec![],
        environment: HashMap::new(),
        volumes: vec![],
        options: None,
        credentials: None,
        alias: Some("web".into()),
    };

    resources
        .setup(&SetupParams {
            runner_name: "test-runner",
            job_id: &job_id,
            job_container: Some(&job_spec),
            services: &[service_spec],
            workspace_host_path: &workspace,
            workflow_files_host_path: &workflow,
            runner_temp_host_path: &runner_temp,
            actions_host_path: &actions,
            tool_cache_host_path: &tool_cache,
            externals_dir: &externals,
        })
        .await
        .unwrap();

    assert!(resources.job_container_id().is_some());
    assert!(
        resources.service_addresses().contains_key("web"),
        "service should have an IP: {:?}",
        resources.service_addresses()
    );
    assert!(
        resources.service_container_map().contains_key("web"),
        "service should have a container ID mapping"
    );

    resources.cleanup().await;

    assert!(resources.job_container_id().is_none());
    assert!(resources.service_addresses().is_empty());
    assert!(resources.service_container_map().is_empty());
}

/// Integration test: files a root job container leaves behind are deleted on cleanup.
#[tokio::test]
#[ignore]
async fn cleanup_purges_files_the_host_cannot_delete() {
    let docker = crate::docker::client::connect(None).unwrap();
    crate::docker::client::ping(&docker).await.unwrap();

    let job_id = uuid::Uuid::new_v4().to_string();
    let mut resources = JobDockerResources::new(docker);

    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    let workflow = tmp.path().join("workflow");
    let runner_temp = tmp.path().join("tmp");
    let actions = tmp.path().join("actions");
    let tool_cache = tmp.path().join("tool-cache");
    let externals = tmp.path().join("externals");
    for dir in [
        &workspace,
        &workflow,
        &runner_temp,
        &actions,
        &tool_cache,
        &externals,
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }

    let job_spec = JobContainerSpec {
        image: "alpine:latest".into(),
        environment: HashMap::new(),
        ports: vec![],
        volumes: vec![],
        options: None,
        credentials: None,
    };

    resources
        .setup(&SetupParams {
            runner_name: "test-runner",
            job_id: &job_id,
            job_container: Some(&job_spec),
            services: &[],
            workspace_host_path: &workspace,
            workflow_files_host_path: &workflow,
            runner_temp_host_path: &runner_temp,
            actions_host_path: &actions,
            tool_cache_host_path: &tool_cache,
            externals_dir: &externals,
        })
        .await
        .unwrap();

    // A uid outside the host user's reach: root-owned on rootful Docker, a subuid on
    // rootless Docker. Either way the host user cannot delete what sits below it.
    let write_foreign_files = [
        "sh",
        "-c",
        "mkdir -p /github/workspace/node_modules/pkg /github/workspace/.git /github/tmp/home \
         && touch /github/workspace/node_modules/pkg/index.js /github/tmp/home/cache \
         && chown -R 12345:12345 /github/workspace /github/tmp",
    ]
    .map(String::from)
    .to_vec();
    let id = resources.job_container_id().unwrap().to_string();
    let exit_code = exec_as_root(resources.docker(), &id, write_foreign_files)
        .await
        .unwrap();
    assert_eq!(exit_code, 0);
    assert!(std::fs::remove_dir_all(workspace.join("node_modules")).is_err());

    resources.cleanup().await;

    assert_eq!(std::fs::read_dir(&workspace).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&runner_temp).unwrap().count(), 0);
}
