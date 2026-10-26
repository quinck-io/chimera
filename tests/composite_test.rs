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
        test -n "${{ inputs.name }}" || exit 1
        if [ "${{ inputs.loud }}" = "true" ]; then
          echo "HELLO ${{ inputs.name }}!!!" | tr '[:lower:]' '[:upper:]'
        else
          echo "Hello, ${{ inputs.name }}!"
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

fn write_action(workspace_dir: &std::path::Path, name: &str, action_yml: &str) {
    let action_dir = workspace_dir.join(".github/actions").join(name);
    std::fs::create_dir_all(&action_dir).unwrap();
    std::fs::write(action_dir.join("action.yml"), action_yml).unwrap();
}

#[tokio::test]
async fn composite_sub_step_reads_output_of_earlier_sub_step() {
    let env = TestEnv::setup().await;
    write_action(
        env.workspace.workspace_dir(),
        "repro",
        r#"
name: repro
runs:
  using: composite
  steps:
    - id: mark
      shell: bash
      run: echo "name=hello" >> "$GITHUB_OUTPUT"
    - shell: bash
      run: |
        echo "name is '${{ steps.mark.outputs.name }}'"
        test "${{ steps.mark.outputs.name }}" = "hello" || exit 1
"#,
    );

    let manifest = manifest_with_steps(
        vec![composite_step(
            "repro",
            ".github/actions/repro",
            serde_json::json!({}),
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Succeeded);
}

/// The workflow reads a composite action's outputs through its `outputs:` mapping,
/// not through whatever its sub-steps happened to write.
#[tokio::test]
async fn composite_action_outputs_mapping_reaches_the_workflow() {
    let env = TestEnv::setup().await;
    write_action(
        env.workspace.workspace_dir(),
        "mapped",
        r#"
name: mapped
inputs:
  suffix:
    default: 'world'
outputs:
  greeting:
    description: 'The greeting'
    value: ${{ steps.make.outputs.raw }}-${{ inputs.suffix }}
runs:
  using: composite
  steps:
    - id: make
      shell: bash
      run: echo "raw=hello" >> "$GITHUB_OUTPUT"
"#,
    );

    let manifest = manifest_with_steps(
        vec![
            composite_step("mapped", ".github/actions/mapped", serde_json::json!({})),
            script_step(
                "check",
                r#"
                test "${{ steps.mapped.outputs.greeting }}" = "hello-world" || exit 1
                test -z "${{ steps.mapped.outputs.raw }}" || exit 1
                test -z "${{ steps.make.outputs.raw }}" || exit 1
                "#,
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Succeeded);
}

#[tokio::test]
async fn composite_sub_step_sees_env_and_path_from_earlier_sub_step() {
    let env = TestEnv::setup().await;
    write_action(
        env.workspace.workspace_dir(),
        "env-chain",
        r#"
name: env-chain
runs:
  using: composite
  steps:
    - shell: bash
      run: |
        echo "GREETING=hi" >> "$GITHUB_ENV"
        echo "/opt/composite-bin" >> "$GITHUB_PATH"
    - shell: bash
      run: |
        test "$GREETING" = "hi" || exit 1
        case ":$PATH:" in *:/opt/composite-bin:*) ;; *) exit 1 ;; esac
"#,
    );

    let manifest = manifest_with_steps(
        vec![
            composite_step(
                "env-chain",
                ".github/actions/env-chain",
                serde_json::json!({}),
            ),
            script_step("after", r#"test "$GREETING" = "hi" || exit 1"#),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Succeeded);
}

/// The shape of the reported failure: a sub-step's output is handed to a nested
/// action's `with:`, and the nested action's mapped output is read afterwards.
#[tokio::test]
async fn composite_passes_sub_step_output_to_nested_action() {
    let env = TestEnv::setup().await;
    let workspace_dir = env.workspace.workspace_dir();
    write_action(
        workspace_dir,
        "echo-name",
        r#"
name: echo-name
inputs:
  name:
    required: true
outputs:
  echoed:
    value: ${{ steps.echo.outputs.echoed }}
runs:
  using: composite
  steps:
    - id: echo
      shell: bash
      run: |
        test -n "${{ inputs.name }}" || exit 1
        echo "echoed=${{ inputs.name }}" >> "$GITHUB_OUTPUT"
"#,
    );
    write_action(
        workspace_dir,
        "outer",
        r#"
name: outer
outputs:
  final:
    value: ${{ steps.nested.outputs.echoed }}
runs:
  using: composite
  steps:
    - id: mark
      shell: bash
      run: echo "name=artifact-1" >> "$GITHUB_OUTPUT"
    - id: nested
      uses: ./.github/actions/echo-name
      with:
        name: ${{ steps.mark.outputs.name }}
    - shell: bash
      run: test "${{ steps.nested.outputs.echoed }}" = "artifact-1" || exit 1
"#,
    );

    let manifest = manifest_with_steps(
        vec![
            composite_step("outer", ".github/actions/outer", serde_json::json!({})),
            script_step(
                "check",
                r#"test "${{ steps.outer.outputs.final }}" = "artifact-1" || exit 1"#,
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Succeeded);
}

/// A composite action has its own `steps` context, and the workflow's comes back
/// once the action is done.
#[tokio::test]
async fn composite_action_scopes_the_steps_context() {
    let env = TestEnv::setup().await;
    write_action(
        env.workspace.workspace_dir(),
        "scoped",
        r#"
name: scoped
runs:
  using: composite
  steps:
    - shell: bash
      run: test -z "${{ steps.first.outputs.value }}" || exit 1
    - id: flaky
      shell: bash
      continue-on-error: true
      run: exit 1
    - shell: bash
      run: |
        test "${{ steps.flaky.outcome }}" = "failure" || exit 1
        test "${{ steps.flaky.conclusion }}" = "success" || exit 1
"#,
    );

    let manifest = manifest_with_steps(
        vec![
            script_step("first", r#"echo "value=kept" >> "$GITHUB_OUTPUT""#),
            composite_step("scoped", ".github/actions/scoped", serde_json::json!({})),
            script_step(
                "check",
                r#"
                test "${{ steps.first.outputs.value }}" = "kept" || exit 1
                test -z "${{ steps.flaky.outcome }}" || exit 1
                "#,
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Succeeded);
}

/// A failed step does not end the action: `always()` and `failure()` steps after it
/// still run, while steps that need success are skipped.
#[tokio::test]
async fn composite_runs_cleanup_steps_after_a_failed_step() {
    let env = TestEnv::setup().await;
    let markers = env.tmp.path().join("markers");
    std::fs::create_dir_all(&markers).unwrap();
    let touch = |name: &str| format!("touch '{}'", markers.join(name).display());
    write_action(
        env.workspace.workspace_dir(),
        "cleanup",
        &format!(
            r#"
name: cleanup
runs:
  using: composite
  steps:
    - shell: bash
      run: exit 1
    - shell: bash
      run: {next}
    - if: always()
      shell: bash
      run: {always}
    - if: failure()
      shell: bash
      run: {failure}
    - if: env.ANYTHING != 'x'
      shell: bash
      run: {implicit}
"#,
            next = touch("next"),
            always = touch("always"),
            failure = touch("failure"),
            implicit = touch("implicit"),
        ),
    );

    let manifest = manifest_with_steps(
        vec![
            composite_step("cleanup", ".github/actions/cleanup", serde_json::json!({})),
            script_step_if(
                "check",
                &format!(
                    r#"test "${{{{ steps.cleanup.outcome }}}}" = "failure" && {}"#,
                    touch("checked")
                ),
                "always()",
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Failed);
    assert!(markers.join("always").exists(), "always() step did not run");
    assert!(
        markers.join("failure").exists(),
        "failure() step did not run"
    );
    assert!(
        !markers.join("next").exists(),
        "a default step ran after a failure"
    );
    assert!(
        !markers.join("implicit").exists(),
        "a step without a status function ran after a failure"
    );
    assert!(
        markers.join("checked").exists(),
        "the composite step did not conclude as failed"
    );
}

/// A node action that logs the environment it runs in, and on its post step logs
/// its `marker` input and the state its main step saved.
fn write_node_action_with_post(workspace_dir: &std::path::Path, log: &std::path::Path) {
    let action_dir = workspace_dir.join(".github/actions/with-post");
    std::fs::create_dir_all(&action_dir).unwrap();
    std::fs::write(
        action_dir.join("action.yml"),
        r#"
name: with-post
inputs:
  marker:
    required: true
runs:
  using: node20
  main: main.js
  post: post.js
"#,
    )
    .unwrap();
    std::fs::write(
        action_dir.join("main.js"),
        format!(
            r#"
const fs = require('fs');
const marker = process.env.INPUT_MARKER;
fs.appendFileSync(process.env.GITHUB_STATE, `saved=state-of-${{marker}}\n`);
fs.appendFileSync({log:?}, `main ${{marker}} action=${{process.env.GITHUB_ACTION}} mode=${{process.env.MODE}}\n`);
"#,
            log = log.to_string_lossy()
        ),
    )
    .unwrap();
    std::fs::write(
        action_dir.join("post.js"),
        format!(
            r#"
const fs = require('fs');
fs.appendFileSync({log:?}, `post ${{process.env.INPUT_MARKER}} ${{process.env.STATE_saved}}\n`);
"#,
            log = log.to_string_lossy()
        ),
    )
    .unwrap();
}

fn read_log(log: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Every step inside a composite action reports the workflow step in
/// `GITHUB_ACTION`, as it inherits that step's `github` context on the official
/// runner.
#[tokio::test]
async fn composite_steps_report_the_workflow_step_as_github_action() {
    let env = TestEnv::setup().await;
    let workspace_dir = env.workspace.workspace_dir();
    let log = env.tmp.path().join("log");
    write_node_action_with_post(workspace_dir, &log);
    write_action(
        workspace_dir,
        "outer",
        r#"
name: outer
runs:
  using: composite
  steps:
    - shell: bash
      run: test "$GITHUB_ACTION" = "deploy" || exit 1
    - uses: ./.github/actions/with-post
      with:
        marker: one
"#,
    );

    let manifest = manifest_with_steps(
        vec![composite_step(
            "deploy",
            ".github/actions/outer",
            serde_json::json!({}),
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Succeeded);
    assert_eq!(read_log(&log)[0], "main one action=deploy mode=undefined");
}

/// A `uses:` step inside a composite action gets its own `env:`, and it wins over
/// the job's env like any step's.
#[tokio::test]
async fn composite_nested_action_gets_its_own_env() {
    let env = TestEnv::setup().await;
    let workspace_dir = env.workspace.workspace_dir();
    let log = env.tmp.path().join("log");
    write_node_action_with_post(workspace_dir, &log);
    write_action(
        workspace_dir,
        "outer",
        r#"
name: outer
inputs:
  mode:
    default: fast
runs:
  using: composite
  steps:
    - shell: bash
      run: echo "MODE=slow" >> "$GITHUB_ENV"
    - uses: ./.github/actions/with-post
      env:
        MODE: ${{ inputs.mode }}
      with:
        marker: one
"#,
    );

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
    assert_eq!(read_log(&log)[0], "main one action=outer mode=fast");
}

/// Post steps of actions used inside composite actions run at the end of the job,
/// last one first, with the inputs and state their main step had, including
/// through a composite action nested in another. One whose main step was skipped
/// has no post step.
#[tokio::test]
async fn composite_nested_actions_run_their_post_steps() {
    let env = TestEnv::setup().await;
    let workspace_dir = env.workspace.workspace_dir();
    let log = env.tmp.path().join("log");
    write_node_action_with_post(workspace_dir, &log);
    write_action(
        workspace_dir,
        "inner",
        r#"
name: inner
runs:
  using: composite
  steps:
    - uses: ./.github/actions/with-post
      with:
        marker: two
"#,
    );
    write_action(
        workspace_dir,
        "outer",
        r#"
name: outer
runs:
  using: composite
  steps:
    - id: first
      uses: ./.github/actions/with-post
      with:
        marker: one
    - uses: ./.github/actions/inner
    - if: failure()
      uses: ./.github/actions/with-post
      with:
        marker: never
"#,
    );

    let manifest = manifest_with_steps(
        vec![
            composite_step("outer", ".github/actions/outer", serde_json::json!({})),
            script_step(
                "after",
                &format!("echo after >> '{}'", log.to_string_lossy()),
            ),
        ],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Succeeded);
    assert_eq!(
        read_log(&log),
        [
            "main one action=outer mode=undefined",
            "main two action=outer mode=undefined",
            "after",
            "post two state-of-two",
            "post one state-of-one",
        ]
    );
}

/// As on the official runner, a composite action's inputs reach its steps only as
/// `inputs.<name>`: neither its own `run:` steps nor the actions it uses get its
/// INPUT_* variables, and each action it uses gets only its own inputs.
#[tokio::test]
async fn composite_inputs_do_not_leak_into_step_env() {
    let env = TestEnv::setup().await;
    let workspace_dir = env.workspace.workspace_dir();
    let node_dir = workspace_dir.join(".github/actions/input-dump");
    std::fs::create_dir_all(&node_dir).unwrap();
    std::fs::write(
        node_dir.join("action.yml"),
        "name: input-dump\ninputs:\n  own: {}\nruns:\n  using: node20\n  main: main.js\n",
    )
    .unwrap();
    std::fs::write(
        node_dir.join("main.js"),
        "const keys = Object.keys(process.env).filter(k => k.startsWith('INPUT_')).sort();\n\
         require('fs').appendFileSync(process.env.GITHUB_WORKSPACE + '/inputs.log', 'node ' + keys + ' ' + process.env.INPUT_OWN + '\\n');\n",
    )
    .unwrap();
    write_action(
        workspace_dir,
        "inner",
        r#"
name: inner
inputs:
  inner: {}
runs:
  using: composite
  steps:
    - shell: bash
      run: echo "inner ${INPUT_OUTER-unset} ${INPUT_INNER-unset} ${{ inputs.inner }}" >> "$GITHUB_WORKSPACE/inputs.log"
"#,
    );
    write_action(
        workspace_dir,
        "outer",
        r#"
name: outer
inputs:
  outer:
    default: leak
runs:
  using: composite
  steps:
    - shell: bash
      run: echo "outer ${INPUT_OUTER-unset} ${{ inputs.outer }}" >> "$GITHUB_WORKSPACE/inputs.log"
    - uses: ./.github/actions/inner
      with:
        inner: ${{ inputs.outer }}-in
    - uses: ./.github/actions/input-dump
      with:
        own: ${{ inputs.outer }}-own
"#,
    );

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
    assert_eq!(
        read_log(&workspace_dir.join("inputs.log")),
        [
            "outer unset leak",
            "inner unset unset leak-in",
            "node INPUT_OWN leak-own",
        ]
    );
}

/// A step output that happens to contain `${{ }}`, written without the runner
/// ever evaluating it.
const LITERAL_SECRET: &str = "${{ secrets.MY_SECRET }}";
const WRITE_LITERAL_SECRET_OUTPUT: &str =
    r#"printf 'payload=$%s\n' '{{ secrets.MY_SECRET }}' >> "$GITHUB_OUTPUT""#;

/// `with:` and `env:` inside a composite action are evaluated exactly once, so a
/// value that merely contains `${{ }}` reaches a nested action, its post step and a
/// nested composite action verbatim instead of being evaluated into a secret.
#[tokio::test]
async fn composite_nested_action_inputs_are_evaluated_once() {
    let env = TestEnv::setup().await;
    let workspace_dir = env.workspace.workspace_dir();
    let log = env.tmp.path().join("log");
    write_node_action_with_post(workspace_dir, &log);
    write_action(
        workspace_dir,
        "record",
        r#"
name: record
inputs:
  value: {}
runs:
  using: composite
  steps:
    - shell: bash
      env:
        VALUE: ${{ inputs.value }}
      run: printf '%s\n%s\n' "$VALUE" "$FROM_ENV" > "$GITHUB_WORKSPACE/recorded"
"#,
    );
    write_action(
        workspace_dir,
        "outer",
        &format!(
            r#"
name: outer
runs:
  using: composite
  steps:
    - id: mark
      shell: bash
      run: {WRITE_LITERAL_SECRET_OUTPUT}
    - uses: ./.github/actions/with-post
      with:
        marker: ${{{{ steps.mark.outputs.payload }}}}
    - uses: ./.github/actions/record
      env:
        FROM_ENV: ${{{{ steps.mark.outputs.payload }}}}
      with:
        value: ${{{{ steps.mark.outputs.payload }}}}
"#
        ),
    );

    let manifest = manifest_with_steps_and_context(
        vec![composite_step(
            "outer",
            ".github/actions/outer",
            serde_json::json!({}),
        )],
        &env.mock_server.uri(),
        serde_json::json!({ "secrets": { "MY_SECRET": "leaked" } }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Succeeded);
    assert_eq!(
        read_log(&log),
        [
            format!("main {LITERAL_SECRET} action=outer mode=undefined"),
            format!("post {LITERAL_SECRET} state-of-{LITERAL_SECRET}"),
        ]
    );
    let recorded = std::fs::read_to_string(workspace_dir.join("recorded")).unwrap();
    assert_eq!(recorded, format!("{LITERAL_SECRET}\n{LITERAL_SECRET}\n"));
}

/// The same holds for a workflow step's own `with:` and `env:`, in its main and
/// its post step.
#[tokio::test]
async fn action_inputs_are_evaluated_once() {
    let env = TestEnv::setup().await;
    let log = env.tmp.path().join("log");
    write_node_action_with_post(env.workspace.workspace_dir(), &log);
    let mut action = local_action_step("act", "./.github/actions/with-post");
    action["inputs"] = serde_json::json!({ "marker": "${{ steps.mark.outputs.payload }}" });
    action["environment"] = serde_json::json!({ "MODE": "${{ steps.mark.outputs.payload }}" });
    let write_token_output = r#"printf 'payload=$%s\n' '{{ github.token }}' >> "$GITHUB_OUTPUT""#;

    let manifest = manifest_with_steps_and_context(
        vec![script_step("mark", write_token_output), action],
        &env.mock_server.uri(),
        serde_json::json!({ "github": { "token": "ghs_leaked" } }),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Succeeded);
    let literal = "${{ github.token }}";
    assert_eq!(
        read_log(&log),
        [
            format!("main {literal} action=act mode={literal}"),
            format!("post {literal} state-of-{literal}"),
        ]
    );
}

/// Inside a composite action too, a failed expression fails its sub-step, and the
/// action's later steps see that through `failure()`.
#[tokio::test]
async fn composite_sub_step_with_failed_expression_fails_the_action() {
    let env = TestEnv::setup().await;
    let marker = env.tmp.path().join("cleanup-ran");
    write_action(
        env.workspace.workspace_dir(),
        "broken",
        &format!(
            r#"
name: broken
runs:
  using: composite
  steps:
    - shell: bash
      env:
        X: ${{{{ fromJSON('{{}}').a. }}}}
      run: echo never
    - if: failure()
      shell: bash
      run: touch '{}'
"#,
            marker.display()
        ),
    );

    let manifest = manifest_with_steps(
        vec![composite_step(
            "broken",
            ".github/actions/broken",
            serde_json::json!({}),
        )],
        &env.mock_server.uri(),
    );
    let (conclusion, _) = env.run(&manifest).await.unwrap();

    assert_eq!(conclusion, JobConclusion::Failed);
    assert!(marker.exists(), "the failure() sub-step should run");
}
