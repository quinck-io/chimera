mod common;

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use chimera::docker::container::JobContainerSpec;
use chimera::job::cancel::JobCancel;
use chimera::job::client::JobConclusion;
use common::*;

const GRACE: Duration = Duration::from_secs(60);

fn local_action_step(id: &str, action_path: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "displayName": format!("Run: {id}"),
        "reference": {
            "name": action_path,
            "type": "repository",
            "repositoryType": "self",
            "path": action_path
        },
        "inputs": {},
        "condition": null,
        "timeoutInMinutes": null,
        "continueOnError": false,
        "order": 1,
        "environment": null,
        "contextName": id
    })
}

/// A node action whose pre step creates `started`, waits `seconds`, then
/// leaves `marker` behind.
fn write_action_with_pre(
    workspace_dir: &Path,
    name: &str,
    pre_if: &str,
    started: &Path,
    seconds: u32,
    marker: &Path,
) {
    let action_dir = workspace_dir.join(".github/actions").join(name);
    std::fs::create_dir_all(&action_dir).unwrap();
    let pre_if = if pre_if.is_empty() {
        String::new()
    } else {
        format!("  pre-if: {pre_if}\n")
    };
    std::fs::write(
        action_dir.join("action.yml"),
        format!("name: {name}\nruns:\n  using: node20\n  main: main.js\n  pre: pre.js\n{pre_if}"),
    )
    .unwrap();
    std::fs::write(action_dir.join("main.js"), "").unwrap();
    std::fs::write(
        action_dir.join("pre.js"),
        format!(
            "const fs = require('fs');\nfs.writeFileSync({:?}, '');\nsetTimeout(() => fs.writeFileSync({:?}, 'ran'), {});\n",
            started.to_string_lossy(),
            marker.to_string_lossy(),
            seconds * 1000
        ),
    )
    .unwrap();
}

/// A node action whose post step leaves `marker` behind when it runs.
fn write_action_with_post(workspace_dir: &Path, name: &str, post_if: &str, marker: &Path) {
    let action_dir = workspace_dir.join(".github/actions").join(name);
    std::fs::create_dir_all(&action_dir).unwrap();
    std::fs::write(
        action_dir.join("action.yml"),
        format!(
            "name: {name}\nruns:\n  using: node20\n  main: main.js\n  post: post.js\n  post-if: {post_if}\n"
        ),
    )
    .unwrap();
    std::fs::write(action_dir.join("main.js"), "").unwrap();
    std::fs::write(
        action_dir.join("post.js"),
        format!(
            "require('fs').writeFileSync({:?}, 'ran');\n",
            marker.to_string_lossy()
        ),
    )
    .unwrap();
}

fn touch(marker: &Path) -> String {
    format!("touch '{}'", marker.display())
}

/// Requests a cancel once a running step has created `started`, so the cancel
/// lands mid-step however slow the machine is.
fn cancel_once_exists(started: &Path, grace: Duration) -> JobCancel {
    let cancel = JobCancel::new();
    let trigger = cancel.clone();
    let started = started.to_path_buf();
    tokio::spawn(async move {
        while !started.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        trigger.request(grace);
    });
    cancel
}

fn touch_then_sleep(started: &Path, seconds: u32) -> String {
    format!("{} && sleep {seconds}", touch(started))
}

#[tokio::test]
async fn cancelled_job_still_runs_always_and_cancelled_steps() {
    let env = TestEnv::setup().await;
    let markers = env.tmp.path().join("markers");
    std::fs::create_dir_all(&markers).unwrap();
    let manifest = manifest_with_steps(
        vec![
            script_step("long", &touch_then_sleep(&markers.join("long"), 30)),
            script_step("next", &touch(&markers.join("next"))),
            script_step_if("cleanup", &touch(&markers.join("cleanup")), "always()"),
            script_step_if(
                "on_cancel",
                &touch(&markers.join("on_cancel")),
                "cancelled()",
            ),
            script_step_if(
                "on_failure",
                &touch(&markers.join("on_failure")),
                "failure()",
            ),
        ],
        &env.mock_server.uri(),
    );

    let (conclusion, _) = env
        .run_with_cancel(&manifest, cancel_once_exists(&markers.join("long"), GRACE))
        .await
        .unwrap();

    assert_eq!(conclusion, JobConclusion::Cancelled);
    assert!(
        markers.join("cleanup").exists(),
        "always() step did not run"
    );
    assert!(
        markers.join("on_cancel").exists(),
        "cancelled() step did not run"
    );
    assert!(
        !markers.join("next").exists(),
        "success() step ran after cancel"
    );
    assert!(
        !markers.join("on_failure").exists(),
        "failure() step ran after cancel"
    );
}

#[tokio::test]
async fn cancel_lets_a_running_always_step_finish() {
    let env = TestEnv::setup().await;
    let started = env.tmp.path().join("started");
    let marker = env.tmp.path().join("finished");
    let manifest = manifest_with_steps(
        vec![script_step_if(
            "cleanup",
            &format!("{} && {}", touch_then_sleep(&started, 2), touch(&marker)),
            "always()",
        )],
        &env.mock_server.uri(),
    );

    let (conclusion, _) = env
        .run_with_cancel(&manifest, cancel_once_exists(&started, GRACE))
        .await
        .unwrap();

    assert_eq!(conclusion, JobConclusion::Cancelled);
    assert!(marker.exists(), "cancel killed a running always() step");
}

#[tokio::test]
async fn cancelled_job_runs_post_steps_whose_post_if_allows_it() {
    let env = TestEnv::setup().await;
    let workspace_dir = env.workspace.workspace_dir();
    let always_post = env.tmp.path().join("always-post");
    let success_post = env.tmp.path().join("success-post");
    let started = env.tmp.path().join("started");
    write_action_with_post(workspace_dir, "always-post", "always()", &always_post);
    write_action_with_post(workspace_dir, "success-post", "success()", &success_post);
    let manifest = manifest_with_steps(
        vec![
            local_action_step("cleanup_action", ".github/actions/always-post"),
            local_action_step("cache_action", ".github/actions/success-post"),
            script_step("long", &touch_then_sleep(&started, 30)),
        ],
        &env.mock_server.uri(),
    );

    let (conclusion, _) = env
        .run_with_cancel(&manifest, cancel_once_exists(&started, GRACE))
        .await
        .unwrap();

    assert_eq!(conclusion, JobConclusion::Cancelled);
    assert!(
        always_post.exists(),
        "post step with post-if: always() did not run"
    );
    assert!(
        !success_post.exists(),
        "post step with post-if: success() ran after cancel"
    );
}

#[tokio::test]
async fn cancel_grace_end_kills_a_running_always_step() {
    let env = TestEnv::setup().await;
    let started = env.tmp.path().join("started");
    let manifest = manifest_with_steps(
        vec![script_step_if(
            "cleanup",
            &touch_then_sleep(&started, 30),
            "always()",
        )],
        &env.mock_server.uri(),
    );

    let begin = std::time::Instant::now();
    let (conclusion, _) = env
        .run_with_cancel(
            &manifest,
            cancel_once_exists(&started, Duration::from_secs(1)),
        )
        .await
        .unwrap();

    assert_eq!(conclusion, JobConclusion::Cancelled);
    assert!(
        begin.elapsed() < Duration::from_secs(20),
        "always() step outlived the cancel grace: {:?}",
        begin.elapsed()
    );
}

#[tokio::test]
async fn cancel_lets_a_running_pre_step_with_the_default_pre_if_finish() {
    let env = TestEnv::setup().await;
    let started = env.tmp.path().join("started");
    let pre_done = env.tmp.path().join("pre-done");
    let main_ran = env.tmp.path().join("main-ran");
    write_action_with_pre(
        env.workspace.workspace_dir(),
        "slow-pre",
        "",
        &started,
        2,
        &pre_done,
    );
    let manifest = manifest_with_steps(
        vec![
            local_action_step("setup", ".github/actions/slow-pre"),
            script_step("main", &touch(&main_ran)),
        ],
        &env.mock_server.uri(),
    );

    let (conclusion, _) = env
        .run_with_cancel(&manifest, cancel_once_exists(&started, GRACE))
        .await
        .unwrap();

    assert_eq!(conclusion, JobConclusion::Cancelled);
    assert!(
        pre_done.exists(),
        "cancel killed a pre step whose pre-if defaults to always()"
    );
    assert!(!main_ran.exists(), "success() step ran after cancel");
}

#[tokio::test]
async fn cancel_stops_a_running_pre_step_whose_pre_if_no_longer_holds() {
    let env = TestEnv::setup().await;
    let started = env.tmp.path().join("started");
    let pre_done = env.tmp.path().join("pre-done");
    write_action_with_pre(
        env.workspace.workspace_dir(),
        "slow-pre",
        "success()",
        &started,
        30,
        &pre_done,
    );
    let manifest = manifest_with_steps(
        vec![local_action_step("setup", ".github/actions/slow-pre")],
        &env.mock_server.uri(),
    );

    let begin = std::time::Instant::now();
    let (conclusion, _) = env
        .run_with_cancel(&manifest, cancel_once_exists(&started, GRACE))
        .await
        .unwrap();

    assert_eq!(conclusion, JobConclusion::Cancelled);
    assert!(!pre_done.exists(), "success() pre step outlived the cancel");
    assert!(
        begin.elapsed() < Duration::from_secs(20),
        "success() pre step was not stopped: {:?}",
        begin.elapsed()
    );
}

#[tokio::test]
async fn cancel_after_a_failure_runs_cancelled_steps_but_not_failure_steps() {
    let env = TestEnv::setup().await;
    let markers = env.tmp.path().join("markers");
    std::fs::create_dir_all(&markers).unwrap();
    let manifest = manifest_with_steps(
        vec![
            script_step("broken", "exit 1"),
            script_step_if(
                "long",
                &touch_then_sleep(&markers.join("long"), 2),
                "always()",
            ),
            script_step_if(
                "on_failure",
                &touch(&markers.join("on_failure")),
                "failure()",
            ),
            script_step_if(
                "on_cancel",
                &touch(&markers.join("on_cancel")),
                "cancelled()",
            ),
            script_step_if("cleanup", &touch(&markers.join("cleanup")), "always()"),
        ],
        &env.mock_server.uri(),
    );

    let (conclusion, _) = env
        .run_with_cancel(&manifest, cancel_once_exists(&markers.join("long"), GRACE))
        .await
        .unwrap();

    assert_eq!(conclusion, JobConclusion::Cancelled);
    assert!(
        !markers.join("on_failure").exists(),
        "failure() step ran after the failed job was cancelled"
    );
    assert!(
        markers.join("on_cancel").exists(),
        "cancelled() step did not run"
    );
    assert!(
        markers.join("cleanup").exists(),
        "always() step did not run"
    );
}

#[tokio::test]
#[ignore]
async fn cancelled_container_job_stops_the_running_step_and_runs_always_steps() {
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
    let host_workspace = env.workspace.workspace_dir();
    let manifest = manifest_with_steps(
        vec![
            script_step("long", "touch /github/workspace/started && sleep 30"),
            script_step("next", "touch /github/workspace/next"),
            script_step_if("cleanup", "touch /github/workspace/cleanup", "always()"),
        ],
        &env.mock_server.uri(),
    );

    let begin = std::time::Instant::now();
    let result = env
        .run_with_docker_and_cancel(
            &manifest,
            &resources,
            cancel_once_exists(&host_workspace.join("started"), GRACE),
        )
        .await;
    let elapsed = begin.elapsed();
    // Cleanup empties the workspace, so look at the markers first.
    let next_ran = host_workspace.join("next").exists();
    let cleanup_ran = host_workspace.join("cleanup").exists();
    resources.cleanup().await;
    let (conclusion, _) = result.unwrap();

    assert_eq!(conclusion, JobConclusion::Cancelled);
    assert!(
        elapsed < Duration::from_secs(20),
        "cancel did not stop the running container step: {elapsed:?}"
    );
    assert!(!next_ran, "success() step ran after cancel");
    assert!(cleanup_ran, "always() step did not run");
}

/// A composite action that survives a cancel tests each of its steps again: the
/// step running when the cancel lands is stopped, later steps that need success
/// are skipped, and its `cancelled()` and `always()` steps run.
#[tokio::test]
async fn composite_surviving_a_cancel_runs_only_its_cancel_steps() {
    let env = TestEnv::setup().await;
    let markers = env.tmp.path().join("markers");
    std::fs::create_dir_all(&markers).unwrap();
    let action_dir = env
        .workspace
        .workspace_dir()
        .join(".github/actions/cleanup");
    std::fs::create_dir_all(&action_dir).unwrap();
    std::fs::write(
        action_dir.join("action.yml"),
        format!(
            r#"
name: cleanup
runs:
  using: composite
  steps:
    - shell: bash
      run: {long}
    - shell: bash
      run: {next}
    - if: cancelled()
      shell: bash
      run: {on_cancel}
    - if: always()
      shell: bash
      run: {cleanup}
    - if: failure()
      shell: bash
      run: {on_failure}
"#,
            long = touch_then_sleep(&markers.join("long"), 30),
            next = touch(&markers.join("next")),
            on_cancel = touch(&markers.join("on_cancel")),
            cleanup = touch(&markers.join("cleanup")),
            on_failure = touch(&markers.join("on_failure")),
        ),
    )
    .unwrap();
    let mut composite = local_action_step("cleanup", ".github/actions/cleanup");
    composite["condition"] = serde_json::json!("always()");
    let manifest = manifest_with_steps(vec![composite], &env.mock_server.uri());

    let begin = std::time::Instant::now();
    let (conclusion, _) = env
        .run_with_cancel(&manifest, cancel_once_exists(&markers.join("long"), GRACE))
        .await
        .unwrap();

    assert_eq!(conclusion, JobConclusion::Cancelled);
    assert!(
        begin.elapsed() < Duration::from_secs(20),
        "the running success() step was not stopped"
    );
    assert!(
        !markers.join("next").exists(),
        "success() step ran after cancel"
    );
    assert!(
        markers.join("on_cancel").exists(),
        "cancelled() step did not run"
    );
    assert!(
        markers.join("cleanup").exists(),
        "always() step did not run"
    );
    assert!(
        !markers.join("on_failure").exists(),
        "failure() step ran after cancel"
    );
}
