mod common;

use std::path::Path;
use std::time::Duration;

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
