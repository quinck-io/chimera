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
