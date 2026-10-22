use std::collections::HashMap;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use super::*;
use crate::job::execute::{JobState, StepConclusion};
use crate::job::logs::LogSender;
use crate::job::masker::SecretMasker;

#[test]
fn exec_markers_are_unique_env_entries() {
    let first = exec_marker();
    let second = exec_marker();

    assert!(first.starts_with("CHIMERA_EXEC_ID="));
    assert_ne!(first, second);
}

/// The kill command scans `/proc`, so it can be checked against host processes.
#[test]
fn kill_command_kills_only_marked_processes() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};

    let marker = exec_marker();
    let (name, value) = marker.split_once('=').unwrap();
    let mut marked = Command::new("sleep")
        .arg("30")
        .env(name, value)
        .spawn()
        .unwrap();
    let mut unmarked = Command::new("sleep").arg("30").spawn().unwrap();

    let command = kill_marked_processes_command(&marker);
    let status = Command::new(&command[0])
        .args(&command[1..])
        .stdout(Stdio::null())
        .status()
        .unwrap();

    assert!(status.success());
    assert_eq!(marked.wait().unwrap().signal(), Some(libc::SIGKILL));
    assert!(unmarked.try_wait().unwrap().is_none());
    unmarked.kill().unwrap();
    unmarked.wait().unwrap();
}

async fn start_alpine(docker: &Docker, purpose: &str) -> String {
    use bollard::container::{Config, CreateContainerOptions};

    crate::docker::client::ensure_image(docker, "alpine:latest", None)
        .await
        .unwrap();
    let name = format!("chimera-exec-{purpose}-{}", uuid::Uuid::new_v4());
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
    container.id
}

async fn remove_container(docker: &Docker, container_id: &str) {
    use bollard::container::RemoveContainerOptions;

    let _ = docker
        .remove_container(
            container_id,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
}

/// Runs a short command in the container and returns its stdout.
async fn exec_output(docker: &Docker, container_id: &str, cmd: &[&str]) -> String {
    let exec = docker
        .create_exec(
            container_id,
            CreateExecOptions {
                attach_stdout: Some(true),
                cmd: Some(cmd.to_vec()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let StartExecResults::Attached { mut output, .. } =
        docker.start_exec(&exec.id, None).await.unwrap()
    else {
        panic!("exec was not attached");
    };
    let mut stdout = String::new();
    while let Some(frame) = output.next().await {
        stdout.push_str(&frame.unwrap().to_string());
    }
    stdout
}

/// Runs `cmd` in the container, cancels it after `cancel_after` and returns the
/// result with the lines the step logged.
async fn exec_and_cancel(
    docker: &Docker,
    container_id: &str,
    cmd: &[&str],
    cancel_after: Duration,
) -> (StepResult, Vec<String>) {
    let masks = SecretMasker::default();
    let (tx, mut rx) = tokio::sync::mpsc::channel(256);
    let sender = LogSender::new_for_test(tx, masks.clone());
    let mut job_state = JobState::new(masks, HashMap::new(), serde_json::json!({}));
    let cancel_token = CancellationToken::new();
    let canceller = cancel_token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(cancel_after).await;
        canceller.cancel();
    });

    let result = docker_exec(
        docker,
        container_id,
        cmd.iter().map(|arg| arg.to_string()).collect(),
        &HashMap::new(),
        "/",
        &mut job_state,
        &sender,
        Duration::from_secs(60),
        &cancel_token,
        false,
    )
    .await
    .unwrap();

    let mut lines = Vec::new();
    while let Ok(line) = rx.try_recv() {
        lines.push(line.content);
    }
    (result, lines)
}

/// Integration test: a cancel kills the step and what it started inside the container.
#[tokio::test]
#[ignore]
async fn cancelled_exec_kills_its_processes_in_the_container() {
    let docker = crate::docker::client::connect(None).unwrap();
    let container_id = start_alpine(&docker, "cancel").await;
    let heartbeat = "sleep 60 & i=0; while :; do i=$((i+1)); echo $i > /tmp/beat; sleep 0.1; done";

    let (result, log) = exec_and_cancel(
        &docker,
        &container_id,
        &["sh", "-c", heartbeat],
        Duration::from_secs(1),
    )
    .await;
    let beat = exec_output(&docker, &container_id, &["cat", "/tmp/beat"]).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let beat_later = exec_output(&docker, &container_id, &["cat", "/tmp/beat"]).await;
    let sleeping = exec_output(&docker, &container_id, &["ps", "-o", "stat,args"]).await;
    remove_container(&docker, &container_id).await;

    assert_eq!(result.conclusion, StepConclusion::Cancelled);
    assert!(!beat.trim().is_empty(), "step never ran");
    assert_eq!(beat, beat_later, "step kept running after the cancel");
    let live_sleeps = sleeping
        .lines()
        .filter(|line| line.contains("sleep 60") && !line.trim_start().starts_with('Z'))
        .count();
    assert_eq!(live_sleeps, 0, "background process survived: {sleeping}");
    assert!(
        !log.iter().any(|line| line.starts_with("##[warning]")),
        "{log:?}"
    );
}

/// Integration test: without a shell to run the kill, a cancel still ends the step,
/// and the step's log warns that its processes may outlive it.
#[tokio::test]
#[ignore]
async fn cancelled_exec_without_a_shell_still_ends_the_step() {
    let docker = crate::docker::client::connect(None).unwrap();
    let container_id = start_alpine(&docker, "no-shell").await;
    exec_output(&docker, &container_id, &["rm", "/bin/sh"]).await;

    let begin = std::time::Instant::now();
    let (result, log) = exec_and_cancel(
        &docker,
        &container_id,
        &["sleep", "60"],
        Duration::from_secs(1),
    )
    .await;
    let elapsed = begin.elapsed();
    remove_container(&docker, &container_id).await;

    assert_eq!(result.conclusion, StepConclusion::Cancelled);
    assert!(elapsed < Duration::from_secs(10), "cancel took {elapsed:?}");
    let warning = log.iter().find(|line| line.starts_with("##[warning]"));
    assert!(
        warning
            .is_some_and(|line| line.contains("may keep running until the container is removed")),
        "{log:?}"
    );
}

#[test]
fn leftover_processes_warning_names_the_cause() {
    let error = anyhow!("kill exec exited with Some(127), the container may lack sh or grep");

    let warning = processes_left_running_warning(&error);

    assert_eq!(
        warning,
        "##[warning]Could not stop the step's processes in the job container \
         (kill exec exited with Some(127), the container may lack sh or grep), \
         they may keep running until the container is removed"
    );
}

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

    let masks = SecretMasker::default();
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
        false,
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

    let masks = SecretMasker::default();
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
        false,
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

/// Integration test: a background process that keeps the exec's output open does not hold the step.
#[tokio::test]
#[ignore]
async fn exec_ends_when_a_background_process_holds_the_output() {
    let docker = crate::docker::client::connect(None).unwrap();
    crate::docker::client::ping(&docker).await.unwrap();
    crate::docker::client::ensure_image(&docker, "alpine:latest", None)
        .await
        .unwrap();

    let name = format!("chimera-exec-background-{}", uuid::Uuid::new_v4());

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

    let masks = SecretMasker::default();
    let (tx, _rx) = tokio::sync::mpsc::channel(256);
    let sender = LogSender::new_for_test(tx, masks.clone());
    let mut job_state = JobState::new(masks, HashMap::new(), serde_json::json!({}));

    let start = std::time::Instant::now();
    let result = docker_exec(
        &docker,
        &container.id,
        vec!["sh".into(), "-c".into(), "sleep 60 & echo started".into()],
        &HashMap::new(),
        "/",
        &mut job_state,
        &sender,
        Duration::from_secs(30),
        &CancellationToken::new(),
        false,
    )
    .await
    .unwrap();

    assert!(start.elapsed().as_secs() < 10);
    assert_eq!(result.conclusion, StepConclusion::Succeeded);

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

#[test]
fn exec_not_yet_started_has_not_exited() {
    let inspect = ExecInspectResponse {
        running: Some(false),
        exit_code: None,
        ..Default::default()
    };

    assert!(!has_exited(&inspect));
}

#[test]
fn running_exec_has_not_exited() {
    let inspect = ExecInspectResponse {
        running: Some(true),
        exit_code: None,
        ..Default::default()
    };

    assert!(!has_exited(&inspect));
}

#[test]
fn stopped_exec_with_exit_code_has_exited() {
    let inspect = ExecInspectResponse {
        running: Some(false),
        exit_code: Some(1),
        ..Default::default()
    };

    assert!(has_exited(&inspect));
}

/// Integration test: a step that outlives the output drain grace keeps its full output and exit code.
#[tokio::test]
#[ignore]
async fn exec_longer_than_the_drain_grace_succeeds() {
    let docker = crate::docker::client::connect(None).unwrap();
    crate::docker::client::ping(&docker).await.unwrap();
    crate::docker::client::ensure_image(&docker, "alpine:latest", None)
        .await
        .unwrap();

    let name = format!("chimera-exec-long-{}", uuid::Uuid::new_v4());

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

    let masks = SecretMasker::default();
    let (tx, _rx) = tokio::sync::mpsc::channel(256);
    let sender = LogSender::new_for_test(tx, masks.clone());
    let mut job_state = JobState::new(masks, HashMap::new(), serde_json::json!({}));

    let result = docker_exec(
        &docker,
        &container.id,
        vec!["sh".into(), "-c".into(), "sleep 4 && echo done".into()],
        &HashMap::new(),
        "/",
        &mut job_state,
        &sender,
        Duration::from_secs(30),
        &CancellationToken::new(),
        false,
    )
    .await
    .unwrap();

    assert_eq!(result.conclusion, StepConclusion::Succeeded);

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
