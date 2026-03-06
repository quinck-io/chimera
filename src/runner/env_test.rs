use super::*;
use crate::job::schema::JobManifest;
use serde_json::json;
fn minimal_manifest() -> JobManifest {
    serde_json::from_value(json!({
        "plan": { "planId": "p", "jobId": "j", "timelineId": "t" },
        "contextData": {
            "github": {
                "repository": "owner/repo",
                "sha": "abc123",
                "ref": "refs/heads/main",
                "server_url": "https://github.com",
                "api_url": "https://api.github.com",
                "actor": "octocat",
                "workflow": "CI",
                "run_id": "12345",
                "run_number": "1",
                "job": "build",
                "event_name": "push"
            }
        },
        "variables": {
            "system.github.token": { "value": "ghs_test123", "isSecret": true },
            "MY_VAR": { "value": "hello" }
        },
        "resources": {
            "endpoints": [{
                "name": "SystemVssConnection",
                "url": "https://pipelines.actions.githubusercontent.com/abc/",
                "authorization": {
                    "parameters": { "AccessToken": "runtime-token" },
                    "scheme": "OAuth"
                }
            }]
        }
    }))
    .unwrap()
}

fn test_workspace() -> (tempfile::TempDir, Workspace) {
    let tmp = tempfile::tempdir().unwrap();
    let ws = Workspace::create(
        tmp.path(),
        &tmp.path().join("tmp"),
        &tmp.path().join("tool_cache"),
        "test-runner",
        "owner/repo",
    )
    .unwrap();
    (tmp, ws)
}

#[test]
fn sets_github_context_vars() {
    let manifest = minimal_manifest();
    let (_tmp, ws) = test_workspace();

    let env = build_base_env(&manifest, &ws, "test-runner");

    assert_eq!(env.get("GITHUB_REPOSITORY").unwrap(), "owner/repo");
    assert_eq!(env.get("GITHUB_SHA").unwrap(), "abc123");
    assert_eq!(env.get("GITHUB_REF").unwrap(), "refs/heads/main");
    assert_eq!(env.get("GITHUB_SERVER_URL").unwrap(), "https://github.com");
    assert_eq!(env.get("GITHUB_API_URL").unwrap(), "https://api.github.com");
    assert_eq!(env.get("GITHUB_ACTOR").unwrap(), "octocat");
    assert_eq!(env.get("GITHUB_WORKFLOW").unwrap(), "CI");
    assert_eq!(env.get("GITHUB_RUN_ID").unwrap(), "12345");
    assert_eq!(env.get("GITHUB_EVENT_NAME").unwrap(), "push");
}

#[test]
fn sets_github_token() {
    let manifest = minimal_manifest();
    let (_tmp, ws) = test_workspace();

    let env = build_base_env(&manifest, &ws, "test-runner");

    assert_eq!(env.get("GITHUB_TOKEN").unwrap(), "ghs_test123");
}

#[test]
fn sets_runner_vars() {
    let manifest = minimal_manifest();
    let (_tmp, ws) = test_workspace();

    let env = build_base_env(&manifest, &ws, "my-runner");

    assert_eq!(env.get("RUNNER_NAME").unwrap(), "my-runner");
    assert!(env.contains_key("RUNNER_OS"));
    assert!(env.contains_key("RUNNER_ARCH"));
    assert!(env.contains_key("RUNNER_TEMP"));
    assert!(env.contains_key("RUNNER_TOOL_CACHE"));
}

#[test]
fn sets_workspace_paths() {
    let manifest = minimal_manifest();
    let (_tmp, ws) = test_workspace();

    let env = build_base_env(&manifest, &ws, "test-runner");

    assert_eq!(env.get("GITHUB_ACTIONS").unwrap(), "true");
    assert!(!env.get("GITHUB_WORKSPACE").unwrap().is_empty());
    assert!(env.contains_key("GITHUB_ENV"));
    assert!(env.contains_key("GITHUB_PATH"));
    assert!(env.contains_key("GITHUB_OUTPUT"));
    assert!(env.contains_key("GITHUB_STATE"));
    assert!(env.contains_key("GITHUB_STEP_SUMMARY"));
}

#[test]
fn sets_non_secret_variables() {
    let manifest = minimal_manifest();
    let (_tmp, ws) = test_workspace();

    let env = build_base_env(&manifest, &ws, "test-runner");

    assert_eq!(env.get("MY_VAR").unwrap(), "hello");
    // Secret variables should NOT appear as their raw key
    assert!(!env.contains_key("system.github.token"));
}

#[test]
fn sets_actions_runtime() {
    let manifest = minimal_manifest();
    let (_tmp, ws) = test_workspace();

    let env = build_base_env(&manifest, &ws, "test-runner");

    assert_eq!(
        env.get("ACTIONS_RUNTIME_URL").unwrap(),
        "https://pipelines.actions.githubusercontent.com/abc/"
    );
    assert_eq!(env.get("ACTIONS_RUNTIME_TOKEN").unwrap(), "runtime-token");
}
