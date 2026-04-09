mod common;

use chimera::job::client::JobConclusion;
use common::*;

#[tokio::test]
async fn set_env_command() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step("s1", "echo '::set-env name=CMD_VAR::cmd_value'"),
            script_step("s2", r#"test "$CMD_VAR" = "cmd_value" || exit 1"#),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn set_output_command() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step("s1", "echo '::set-output name=result::42'"),
            script_step(
                "s2",
                r#"test "${{ steps.s1.outputs.result }}" = "42" || exit 1"#,
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn add_path_command() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![
            script_step("s1", "echo '::add-path::/opt/wc/bin'"),
            script_step("s2", r#"echo "$PATH" | grep -q "/opt/wc/bin" || exit 1"#),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn add_mask_command() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            "echo '::add-mask::my-secret-value'\necho 'mask set'",
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn debug_command_with_step_debug_enabled() {
    let env = TestEnv::setup().await;
    // ACTIONS_STEP_DEBUG secret enables ::debug:: output
    let manifest = manifest_with_steps_and_context(
        vec![script_step(
            "s1",
            "echo '::debug::This is debug info'\necho 'done'",
        )],
        &env.mock_server.uri(),
        serde_json::json!({
            "secrets": { "ACTIONS_STEP_DEBUG": "true" }
        }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn save_state_command() {
    let env = TestEnv::setup().await;
    // save-state persists values — the step succeeds if the command is parsed
    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            "echo '::save-state name=mykey::myvalue'\necho 'state saved'",
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn warning_and_error_commands() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            "echo '::warning::This is a warning'\necho '::error::This is an error'\necho 'done'",
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn group_commands() {
    let env = TestEnv::setup().await;
    let manifest = manifest_with_steps(
        vec![script_step(
            "s1",
            "echo '::group::My Group'\necho 'inside group'\necho '::endgroup::'",
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}
