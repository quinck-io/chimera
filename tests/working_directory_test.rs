mod common;

use chimera::job::client::JobConclusion;
use common::*;

/// Creates `<workspace>/apps/mobile/marker.txt`.
fn nested_dir(env: &TestEnv) {
    let dir = env.workspace.workspace_dir().join("apps/mobile");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("marker.txt"), "here").unwrap();
}

#[tokio::test]
async fn step_runs_in_its_working_directory() {
    let env = TestEnv::setup().await;
    nested_dir(&env);

    let manifest = manifest_with_steps(
        vec![script_step_in_dir(
            "s1",
            // Both halves matter: the right directory, reached without cd'ing.
            r#"test -f marker.txt || exit 1
               case "$PWD" in */apps/mobile) ;; *) echo "cwd=$PWD"; exit 1 ;; esac"#,
            "apps/mobile",
        )],
        &env.mock_server.uri(),
    );

    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn step_without_working_directory_runs_at_the_workspace_root() {
    let env = TestEnv::setup().await;
    nested_dir(&env);

    let manifest = manifest_with_steps(
        vec![script_step("s1", r#"test ! -f marker.txt || exit 1"#)],
        &env.mock_server.uri(),
    );

    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn working_directory_resolves_expressions() {
    let env = TestEnv::setup().await;
    nested_dir(&env);

    let manifest = manifest_with_steps(
        vec![script_step_in_dir(
            "s1",
            r#"test -f marker.txt || exit 1"#,
            "apps/${{ env.APP_NAME }}",
        )],
        &env.mock_server.uri(),
    );
    let mut manifest = manifest;
    manifest.steps[0].environment = Some(std::collections::HashMap::from([(
        "APP_NAME".to_string(),
        "mobile".to_string(),
    )]));

    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

/// An absolute path replaces the workspace rather than nesting under it.
#[tokio::test]
async fn absolute_working_directory_is_used_as_is() {
    let env = TestEnv::setup().await;
    let outside = env.tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("elsewhere.txt"), "x").unwrap();

    let manifest = manifest_with_steps(
        vec![script_step_in_dir(
            "s1",
            r#"test -f elsewhere.txt || exit 1"#,
            outside.to_str().unwrap(),
        )],
        &env.mock_server.uri(),
    );

    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

/// `defaults.run.working-directory` applies to steps that set nothing themselves.
#[tokio::test]
async fn job_default_working_directory_applies() {
    let env = TestEnv::setup().await;
    nested_dir(&env);

    let mut manifest = manifest_with_steps(
        vec![script_step("s1", r#"test -f marker.txt || exit 1"#)],
        &env.mock_server.uri(),
    );
    manifest.defaults.insert(
        "run".into(),
        std::collections::HashMap::from([("working-directory".into(), "apps/mobile".into())]),
    );

    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn step_working_directory_beats_the_job_default() {
    let env = TestEnv::setup().await;
    nested_dir(&env);
    let other = env.workspace.workspace_dir().join("apps/web");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("web-only.txt"), "x").unwrap();

    let mut manifest = manifest_with_steps(
        vec![script_step_in_dir(
            "s1",
            r#"test -f web-only.txt || exit 1"#,
            "apps/web",
        )],
        &env.mock_server.uri(),
    );
    manifest.defaults.insert(
        "run".into(),
        std::collections::HashMap::from([("working-directory".into(), "apps/mobile".into())]),
    );

    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

/// The body-vision shape end to end: a reusable workflow whose job default is
/// `working-directory: ${{ inputs.app-directory }}`, with the input arriving in
/// context data rather than as an env var.
#[tokio::test]
async fn job_default_working_directory_from_a_workflow_call_input() {
    let env = TestEnv::setup().await;
    nested_dir(&env);

    let mut manifest = manifest_with_steps_and_context(
        vec![script_step("s1", r#"test -f marker.txt || exit 1"#)],
        &env.mock_server.uri(),
        serde_json::json!({ "inputs": { "app-directory": "apps/mobile" } }),
    );
    manifest.defaults.insert(
        "run".into(),
        std::collections::HashMap::from([(
            "working-directory".into(),
            "${{ inputs.app-directory }}".into(),
        )]),
    );

    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

/// A called workflow reads its inputs in ordinary step bodies too, not just in
/// `defaults`.
#[tokio::test]
async fn workflow_call_input_resolves_inside_a_step_script() {
    let env = TestEnv::setup().await;

    let manifest = manifest_with_steps_and_context(
        vec![script_step(
            "s1",
            r#"test "${{ inputs.app-directory }}" = "apps/mobile" || exit 1"#,
        )],
        &env.mock_server.uri(),
        serde_json::json!({ "inputs": { "app-directory": "apps/mobile" } }),
    );

    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}
