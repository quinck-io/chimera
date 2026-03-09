use std::collections::HashMap;

use crate::job::schema::JobManifest;
use crate::job::workspace::Workspace;
use crate::utils::{arch_label, os_label};

/// Build the base environment variables for step execution.
///
/// Combines workspace paths, runner metadata, context data from the manifest,
/// non-secret variables, and actions runtime URLs into a single env map.
pub fn build_base_env(
    manifest: &JobManifest,
    workspace: &Workspace,
    runner_name: &str,
) -> HashMap<String, String> {
    let mut env = HashMap::new();

    // Workspace and runner paths
    env.insert("GITHUB_ACTIONS".into(), "true".into());
    env.insert(
        "GITHUB_WORKSPACE".into(),
        workspace.workspace_dir().to_string_lossy().into_owned(),
    );
    env.insert(
        "GITHUB_ENV".into(),
        workspace.env_file().to_string_lossy().into_owned(),
    );
    env.insert(
        "GITHUB_PATH".into(),
        workspace.path_file().to_string_lossy().into_owned(),
    );
    env.insert(
        "GITHUB_OUTPUT".into(),
        workspace.output_file().to_string_lossy().into_owned(),
    );
    env.insert(
        "GITHUB_STATE".into(),
        workspace.state_file().to_string_lossy().into_owned(),
    );
    env.insert(
        "GITHUB_STEP_SUMMARY".into(),
        workspace.step_summary_file().to_string_lossy().into_owned(),
    );
    env.insert("RUNNER_OS".into(), os_label().into());
    env.insert("RUNNER_ARCH".into(), arch_label().into());
    env.insert("RUNNER_NAME".into(), runner_name.into());
    env.insert(
        "RUNNER_TEMP".into(),
        workspace.runner_temp().to_string_lossy().into_owned(),
    );
    env.insert(
        "RUNNER_TOOL_CACHE".into(),
        workspace.tool_cache().to_string_lossy().into_owned(),
    );

    // GITHUB_TOKEN from manifest variables
    if let Some(token) = manifest.github_token() {
        env.insert("GITHUB_TOKEN".into(), token.into());
    }

    // Extract fields from context_data.github
    if let Some(github) = manifest.context_data.get("github") {
        let mappings = [
            ("workflow", "GITHUB_WORKFLOW"),
            ("run_id", "GITHUB_RUN_ID"),
            ("run_number", "GITHUB_RUN_NUMBER"),
            ("run_attempt", "GITHUB_RUN_ATTEMPT"),
            ("job", "GITHUB_JOB"),
            ("action", "GITHUB_ACTION"),
            ("actor", "GITHUB_ACTOR"),
            ("repository", "GITHUB_REPOSITORY"),
            ("repository_owner", "GITHUB_REPOSITORY_OWNER"),
            ("event_name", "GITHUB_EVENT_NAME"),
            ("sha", "GITHUB_SHA"),
            ("ref", "GITHUB_REF"),
            ("server_url", "GITHUB_SERVER_URL"),
            ("api_url", "GITHUB_API_URL"),
            ("graphql_url", "GITHUB_GRAPHQL_URL"),
        ];

        for (json_key, env_key) in mappings {
            if let Some(val) = github.get(json_key).and_then(|v| v.as_str()) {
                env.insert(env_key.into(), val.into());
            }
        }
    }

    // Add non-secret variables
    for (key, var) in &manifest.variables {
        if !var.is_secret {
            let env_key = key.replace('.', "_").to_uppercase();
            env.insert(env_key, var.value.clone());
        }
    }

    // Server URL and token for actions runtime
    if let Ok(server_url) = manifest.server_url() {
        env.insert("ACTIONS_RUNTIME_URL".into(), server_url.into());
    }
    if let Ok(token) = manifest.access_token() {
        env.insert("ACTIONS_RUNTIME_TOKEN".into(), token.into());
    }

    env
}

/// Build environment variables for container-mode execution.
///
/// Same as `build_base_env()` but remaps workspace paths to container-internal paths
/// where bind mounts map host files to the container filesystem.
pub fn build_container_env(
    manifest: &JobManifest,
    workspace: &Workspace,
    runner_name: &str,
) -> HashMap<String, String> {
    let mut env = build_base_env(manifest, workspace, runner_name);

    // Container is always Linux — override host OS/arch so actions
    // don't try to use macOS tools like brew inside a Linux container.
    env.insert("RUNNER_OS".into(), "Linux".into());
    env.insert("ImageOS".into(), "ubuntu22".into());

    // Remap paths to container-internal layout
    env.insert("GITHUB_WORKSPACE".into(), "/github/workspace".into());
    env.insert("GITHUB_ENV".into(), "/github/workflow/_env".into());
    env.insert("GITHUB_PATH".into(), "/github/workflow/_path".into());
    env.insert("GITHUB_OUTPUT".into(), "/github/workflow/_output".into());
    env.insert("GITHUB_STATE".into(), "/github/workflow/_state".into());
    env.insert(
        "GITHUB_STEP_SUMMARY".into(),
        "/github/workflow/_step_summary".into(),
    );
    env.insert("RUNNER_TEMP".into(), "/github/tmp".into());
    env.insert("RUNNER_TOOL_CACHE".into(), "/github/tool-cache".into());

    // Set a proper default PATH for the Linux container. In host mode PATH
    // is inherited from the process environment, but docker exec only gets
    // what we explicitly pass. Without this, any GITHUB_PATH additions
    // would replace PATH entirely, losing /usr/bin etc.
    env.insert(
        "PATH".into(),
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into(),
    );

    env
}

#[cfg(test)]
#[path = "env_test.rs"]
mod env_test;
