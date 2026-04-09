mod common;

use std::collections::HashMap;

use chimera::docker::container::{JobContainerSpec, ServiceContainerSpec};
use chimera::job::client::JobConclusion;
use common::*;

// ─── Container mode ──────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn container_mode_echo() {
    let env = TestEnv::setup().await;
    let job_spec = JobContainerSpec {
        image: "ubuntu:latest".into(),
        environment: HashMap::new(),
        ports: vec![],
        volumes: vec![],
        options: None,
        credentials: None,
    };

    let mut resources = setup_docker(&env.tmp, &env.workspace, Some(&job_spec), &[]).await;

    let manifest = manifest_with_steps(
        vec![script_step("s1", "echo hello from container")],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run_with_docker(&manifest, &resources).await.unwrap();
    resources.cleanup().await;

    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
#[ignore]
async fn container_mode_env_propagation() {
    let env = TestEnv::setup().await;
    let job_spec = JobContainerSpec {
        image: "ubuntu:latest".into(),
        environment: HashMap::new(),
        ports: vec![],
        volumes: vec![],
        options: None,
        credentials: None,
    };

    let mut resources = setup_docker(&env.tmp, &env.workspace, Some(&job_spec), &[]).await;

    let manifest = manifest_with_steps(
        vec![
            script_step("s1", r#"echo "MY_VAR=container_val" >> "$GITHUB_ENV""#),
            script_step("s2", r#"test "$MY_VAR" = "container_val" || exit 1"#),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run_with_docker(&manifest, &resources).await.unwrap();
    resources.cleanup().await;

    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
#[ignore]
async fn container_mode_workspace_mounted() {
    let env = TestEnv::setup().await;

    std::fs::write(
        env.workspace.workspace_dir().join("test.txt"),
        "hello from host",
    )
    .unwrap();

    let job_spec = JobContainerSpec {
        image: "ubuntu:latest".into(),
        environment: HashMap::new(),
        ports: vec![],
        volumes: vec![],
        options: None,
        credentials: None,
    };

    let mut resources = setup_docker(&env.tmp, &env.workspace, Some(&job_spec), &[]).await;

    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"cat /github/workspace/test.txt | grep -q "hello from host" || exit 1"#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run_with_docker(&manifest, &resources).await.unwrap();
    resources.cleanup().await;

    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
#[ignore]
async fn container_mode_job_context() {
    let env = TestEnv::setup().await;
    let job_spec = JobContainerSpec {
        image: "ubuntu:latest".into(),
        environment: HashMap::new(),
        ports: vec![],
        volumes: vec![],
        options: None,
        credentials: None,
    };

    let mut resources = setup_docker(&env.tmp, &env.workspace, Some(&job_spec), &[]).await;

    let manifest = manifest_with_steps(
        vec![
            script_step("s1", r#"test "${{ job.status }}" = "success" || exit 1"#),
            script_step("s2", r#"test -n "${{ job.container.id }}" || exit 1"#),
            script_step("s3", r#"test -n "${{ job.container.network }}" || exit 1"#),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run_with_docker(&manifest, &resources).await.unwrap();
    resources.cleanup().await;

    assert_eq!(conclusion, JobConclusion::Succeeded);
}

// ─── Services ────────────────────────────────────────────────────────

#[tokio::test]
#[ignore]
async fn service_redis_reachable() {
    let env = TestEnv::setup().await;

    let service = ServiceContainerSpec {
        image: "redis:7-alpine".into(),
        ports: vec!["6399:6379".into()],
        environment: HashMap::new(),
        volumes: vec![],
        options: Some("--health-cmd 'redis-cli ping' --health-interval 1s".into()),
        credentials: None,
        alias: Some("redis".into()),
    };

    let job_spec = JobContainerSpec {
        image: "ubuntu:latest".into(),
        environment: HashMap::new(),
        ports: vec![],
        volumes: vec![],
        options: None,
        credentials: None,
    };

    let mut resources = setup_docker(&env.tmp, &env.workspace, Some(&job_spec), &[service]).await;

    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            r#"
            apt-get update -qq > /dev/null 2>&1
            apt-get install -y -qq netcat-openbsd > /dev/null 2>&1
            nc -z redis 6379 || exit 1
            echo "redis is reachable"
            "#,
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run_with_docker(&manifest, &resources).await.unwrap();
    resources.cleanup().await;

    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
#[ignore]
async fn service_job_context_ports() {
    let env = TestEnv::setup().await;

    let service = ServiceContainerSpec {
        image: "redis:7-alpine".into(),
        ports: vec!["6398:6379".into()],
        environment: HashMap::new(),
        volumes: vec![],
        options: Some("--health-cmd 'redis-cli ping' --health-interval 1s".into()),
        credentials: None,
        alias: Some("redis".into()),
    };

    let job_spec = JobContainerSpec {
        image: "ubuntu:latest".into(),
        environment: HashMap::new(),
        ports: vec![],
        volumes: vec![],
        options: None,
        credentials: None,
    };

    let mut resources = setup_docker(&env.tmp, &env.workspace, Some(&job_spec), &[service]).await;

    let manifest = manifest_with_steps(
        vec![
            script_step("s1", r#"test -n "${{ job.services.redis.id }}" || exit 1"#),
            script_step(
                "s2",
                r#"test -n "${{ job.services.redis.ports['6379'] }}" || exit 1"#,
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run_with_docker(&manifest, &resources).await.unwrap();
    resources.cleanup().await;

    assert_eq!(conclusion, JobConclusion::Succeeded);
}

// ─── Docker actions (inline docker:// images) ────────────────────────

#[tokio::test]
#[ignore]
async fn inline_docker_alpine_echo() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![serde_json::json!({
            "id": "s1",
            "displayName": "Docker alpine echo",
            "reference": {
                "name": "docker://alpine:3.19",
                "type": "containerregistry",
                "image": "alpine:3.19"
            },
            "inputs": {
                "args": "echo hello from docker action"
            },
            "condition": null,
            "timeoutInMinutes": null,
            "continueOnError": false,
            "order": 1,
            "environment": null,
            "contextName": "s1"
        })],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
#[ignore]
async fn inline_docker_entrypoint_override() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![serde_json::json!({
            "id": "s1",
            "displayName": "Docker entrypoint override",
            "reference": {
                "name": "docker://alpine:3.19",
                "type": "containerregistry",
                "image": "alpine:3.19"
            },
            "inputs": {
                "entryPoint": "/bin/sh",
                "args": "-c uname"
            },
            "condition": null,
            "timeoutInMinutes": null,
            "continueOnError": false,
            "order": 1,
            "environment": null,
            "contextName": "s1"
        })],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
#[ignore]
async fn inline_docker_env_vars() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![serde_json::json!({
            "id": "s1",
            "displayName": "Docker env vars",
            "reference": {
                "name": "docker://alpine:3.19",
                "type": "containerregistry",
                "image": "alpine:3.19"
            },
            "inputs": {
                "entryPoint": "/bin/sh",
                "args": "-c echo $GREETING"
            },
            "condition": null,
            "timeoutInMinutes": null,
            "continueOnError": false,
            "order": 1,
            "environment": { "GREETING": "hello from env" },
            "contextName": "s1"
        })],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}
