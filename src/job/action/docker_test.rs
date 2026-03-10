use std::collections::HashMap;

use super::*;
use crate::job::action::metadata::{ActionRuns, ActionRuntime};

// ── split_shell_args ────────────────────────────────────────────

#[test]
fn split_args_simple() {
    assert_eq!(split_shell_args("echo hello"), vec!["echo", "hello"]);
}

#[test]
fn split_args_double_quotes() {
    assert_eq!(
        split_shell_args(r#"-c "echo hello && uname -a""#),
        vec!["-c", "echo hello && uname -a"]
    );
}

#[test]
fn split_args_single_quotes() {
    assert_eq!(
        split_shell_args("-c 'echo hello world'"),
        vec!["-c", "echo hello world"]
    );
}

#[test]
fn split_args_empty() {
    assert!(split_shell_args("").is_empty());
    assert!(split_shell_args("   ").is_empty());
}

#[test]
fn split_args_mixed_quotes() {
    assert_eq!(
        split_shell_args(r#"-e "console.log('hi')""#),
        vec!["-e", "console.log('hi')"]
    );
}

// ── resolve_image ───────────────────────────────────────────────

fn make_docker_metadata(image: &str) -> ActionMetadata {
    ActionMetadata {
        name: None,
        inputs: HashMap::new(),
        runs: ActionRuns {
            using: ActionRuntime::Docker,
            main: None,
            pre: None,
            post: None,
            steps: None,
            image: Some(image.into()),
            entrypoint: None,
            args: None,
            pre_entrypoint: None,
            post_entrypoint: None,
            env: None,
        },
    }
}

#[test]
fn resolve_image_strips_docker_prefix() {
    let m = make_docker_metadata("docker://node:18");
    assert_eq!(resolve_image(&m).unwrap(), "node:18");

    let m = make_docker_metadata("docker://alpine:latest");
    assert_eq!(resolve_image(&m).unwrap(), "alpine:latest");
}

#[test]
fn resolve_image_no_prefix() {
    let m = make_docker_metadata("node:18");
    assert_eq!(resolve_image(&m).unwrap(), "node:18");

    let m = make_docker_metadata("alpine");
    assert_eq!(resolve_image(&m).unwrap(), "alpine");
}

#[test]
fn resolve_image_dockerfile_error() {
    let m = make_docker_metadata("Dockerfile");
    let err = resolve_image(&m).unwrap_err();
    assert!(err.to_string().contains("Dockerfile"));
    assert!(err.to_string().contains("not supported"));
}

#[test]
fn resolve_image_path_dockerfile_error() {
    let m = make_docker_metadata("path/to/Dockerfile");
    let err = resolve_image(&m).unwrap_err();
    assert!(err.to_string().contains("not supported"));
}

#[test]
fn resolve_image_registry_with_prefix() {
    let m = make_docker_metadata("docker://ghcr.io/owner/image:v1");
    assert_eq!(resolve_image(&m).unwrap(), "ghcr.io/owner/image:v1");
}

#[test]
fn resolve_image_missing_field() {
    let m = ActionMetadata {
        name: None,
        inputs: HashMap::new(),
        runs: ActionRuns {
            using: ActionRuntime::Docker,
            main: None,
            pre: None,
            post: None,
            steps: None,
            image: None,
            entrypoint: None,
            args: None,
            pre_entrypoint: None,
            post_entrypoint: None,
            env: None,
        },
    };
    assert!(resolve_image(&m).is_err());
}

// ── resolve_entry_point ─────────────────────────────────────────

fn make_metadata_with_entrypoints(
    entrypoint: Option<&str>,
    pre: Option<&str>,
    post: Option<&str>,
    args: Option<Vec<String>>,
) -> ActionMetadata {
    ActionMetadata {
        name: None,
        inputs: HashMap::new(),
        runs: ActionRuns {
            using: ActionRuntime::Docker,
            main: None,
            pre: None,
            post: None,
            steps: None,
            image: Some("alpine".into()),
            entrypoint: entrypoint.map(|s| s.into()),
            args,
            pre_entrypoint: pre.map(|s| s.into()),
            post_entrypoint: post.map(|s| s.into()),
            env: None,
        },
    }
}

#[test]
fn entry_point_main_with_entrypoint_and_args() {
    let m = make_metadata_with_entrypoints(
        Some("/entrypoint.sh"),
        None,
        None,
        Some(vec!["--flag".into()]),
    );
    let (ep, args) = resolve_entry_point(&m, "main").unwrap();
    assert_eq!(ep.as_deref(), Some("/entrypoint.sh"));
    assert_eq!(args, vec!["--flag"]);
}

#[test]
fn entry_point_main_no_entrypoint() {
    let m = make_metadata_with_entrypoints(None, None, None, None);
    let (ep, args) = resolve_entry_point(&m, "main").unwrap();
    assert!(ep.is_none());
    assert!(args.is_empty());
}

#[test]
fn entry_point_pre_present() {
    let m = make_metadata_with_entrypoints(None, Some("/pre.sh"), None, None);
    let (ep, args) = resolve_entry_point(&m, "pre").unwrap();
    assert_eq!(ep.as_deref(), Some("/pre.sh"));
    assert!(args.is_empty());
}

#[test]
fn entry_point_pre_absent_returns_none() {
    let m = make_metadata_with_entrypoints(None, None, None, None);
    assert!(resolve_entry_point(&m, "pre").is_none());
}

#[test]
fn entry_point_post_present() {
    let m = make_metadata_with_entrypoints(None, None, Some("/post.sh"), None);
    let (ep, args) = resolve_entry_point(&m, "post").unwrap();
    assert_eq!(ep.as_deref(), Some("/post.sh"));
    assert!(args.is_empty());
}

#[test]
fn entry_point_post_absent_returns_none() {
    let m = make_metadata_with_entrypoints(None, None, None, None);
    assert!(resolve_entry_point(&m, "post").is_none());
}

// ── build_container_env ─────────────────────────────────────────

#[test]
fn container_env_remaps_github_paths() {
    let mut host = HashMap::new();
    host.insert("GITHUB_WORKSPACE".into(), "/home/runner/work".into());
    host.insert("CUSTOM_VAR".into(), "kept".into());

    let env = build_container_env(&host);

    assert_eq!(env["GITHUB_WORKSPACE"], "/github/workspace");
    assert_eq!(env["GITHUB_ENV"], "/github/workflow/_env");
    assert_eq!(env["GITHUB_OUTPUT"], "/github/workflow/_output");
    assert_eq!(env["GITHUB_STATE"], "/github/workflow/_state");
    assert_eq!(env["RUNNER_TEMP"], "/github/tmp");
    assert_eq!(env["RUNNER_TOOL_CACHE"], "/github/tool-cache");
    assert_eq!(env["CUSTOM_VAR"], "kept");
}
