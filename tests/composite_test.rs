mod common;

use chimera::job::client::JobConclusion;
use common::*;

fn create_greet_action(workspace_dir: &std::path::Path) {
    let action_dir = workspace_dir.join(".github/actions/greet");
    std::fs::create_dir_all(&action_dir).unwrap();
    std::fs::write(
        action_dir.join("action.yml"),
        r#"
name: 'Greet'
description: 'A test composite action'
inputs:
  name:
    description: 'Who to greet'
    required: true
  loud:
    description: 'Uppercase the greeting'
    default: 'false'
runs:
  using: 'composite'
  steps:
    - run: |
        if [ "$INPUT_LOUD" = "true" ]; then
          echo "HELLO $INPUT_NAME!!!" | tr '[:lower:]' '[:upper:]'
        else
          echo "Hello, $INPUT_NAME!"
        fi
      shell: bash
    - run: echo "Greeting delivered"
      shell: bash
"#,
    )
    .unwrap();
}

fn composite_step(id: &str, action_path: &str, inputs: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "displayName": format!("Run: {id}"),
        "reference": {
            "name": action_path,
            "type": "repository",
            "repositoryType": "self",
            "path": action_path
        },
        "inputs": inputs,
        "condition": null,
        "timeoutInMinutes": null,
        "continueOnError": false,
        "order": 1,
        "environment": null,
        "contextName": id
    })
}

#[tokio::test]
async fn composite_action_runs() {
    let env = TestEnv::setup().await;
    create_greet_action(env.workspace.workspace_dir());

    let manifest = manifest_with_steps(
        vec![composite_step(
            "greet",
            ".github/actions/greet",
            serde_json::json!({"name": "chimera"}),
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn composite_action_with_input_variation() {
    let env = TestEnv::setup().await;
    create_greet_action(env.workspace.workspace_dir());

    let manifest = manifest_with_steps(
        vec![
            composite_step(
                "quiet",
                ".github/actions/greet",
                serde_json::json!({"name": "chimera"}),
            ),
            composite_step(
                "loud",
                ".github/actions/greet",
                serde_json::json!({"name": "chimera", "loud": "true"}),
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

/// A composite action that forwards a hyphenated input of its own to a sub-step,
/// the way `pnpm/action-setup`'s `dest:` is wired up in real workflows.
fn create_forwarding_action(workspace_dir: &std::path::Path) {
    let action_dir = workspace_dir.join(".github/actions/forward");
    std::fs::create_dir_all(&action_dir).unwrap();
    std::fs::write(
        action_dir.join("action.yml"),
        r#"
name: 'Forward'
description: 'Forwards a hyphenated input to a sub-step'
inputs:
  pnpm-dest:
    description: 'Where pnpm goes'
    required: false
    default: '~/setup-pnpm'
runs:
  using: 'composite'
  steps:
    - shell: bash
      run: |
        echo "dest=${{ inputs.pnpm-dest }}"
        test "${{ inputs.pnpm-dest }}" = "/tmp/pnpm-here" || exit 1
"#,
    )
    .unwrap();
}

#[tokio::test]
async fn composite_action_forwards_hyphenated_input() {
    let env = TestEnv::setup().await;
    create_forwarding_action(env.workspace.workspace_dir());

    let manifest = manifest_with_steps(
        vec![composite_step(
            "forward",
            ".github/actions/forward",
            serde_json::json!({"pnpm-dest": "/tmp/pnpm-here"}),
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn composite_action_runs_a_script_from_its_own_path() {
    use std::os::unix::fs::PermissionsExt;

    let env = TestEnv::setup().await;
    let action_dir = env.workspace.workspace_dir().join(".github/actions/entry");
    std::fs::create_dir_all(&action_dir).unwrap();
    std::fs::write(
        action_dir.join("action.yml"),
        r#"
name: 'Entry'
description: 'Adds its own path to PATH, then calls a script from it'
runs:
  using: 'composite'
  steps:
    - run: echo "$ACTION_PATH" >> $GITHUB_PATH
      shell: bash
      env:
        ACTION_PATH: ${{ github.action_path }}
    - run: entrypoint.sh
      shell: bash
"#,
    )
    .unwrap();
    let script = action_dir.join("entrypoint.sh");
    std::fs::write(&script, "#!/usr/bin/env bash\necho entrypoint ran\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let manifest = manifest_with_steps(
        vec![composite_step(
            "entry",
            ".github/actions/entry",
            serde_json::json!({}),
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();
    assert_eq!(conclusion, JobConclusion::Succeeded);
}

/// GitHub reads every `with:` and `env:` value of a composite sub-step as a string, so
/// an unquoted `true` or `3` reaches the nested action like its quoted form.
#[tokio::test]
async fn composite_action_passes_non_string_scalars_as_strings() {
    let env = TestEnv::setup().await;
    let actions_dir = env.workspace.workspace_dir().join(".github/actions");
    std::fs::create_dir_all(actions_dir.join("inner")).unwrap();
    std::fs::write(
        actions_dir.join("inner/action.yml"),
        r#"
name: 'Inner'
description: 'Checks the inputs it receives'
inputs:
  cache:
    default: 'false'
  retries:
    default: '1'
runs:
  using: 'composite'
  steps:
    - run: |
        test "${{ inputs.cache }}" = "true" || exit 1
        test "${{ inputs.retries }}" = "3" || exit 1
      shell: bash
"#,
    )
    .unwrap();
    std::fs::create_dir_all(actions_dir.join("outer")).unwrap();
    std::fs::write(
        actions_dir.join("outer/action.yml"),
        r#"
name: 'Outer'
description: 'Passes unquoted scalars down'
runs:
  using: 'composite'
  steps:
    - uses: ./.github/actions/inner
      with:
        cache: true
        retries: 3
    - run: |
        test "$VERBOSE" = "false" || exit 1
        test "$PORT" = "8080" || exit 1
      shell: bash
      env:
        VERBOSE: false
        PORT: 8080
"#,
    )
    .unwrap();

    let manifest = manifest_with_steps(
        vec![composite_step(
            "outer",
            ".github/actions/outer",
            serde_json::json!({}),
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Succeeded);
}
