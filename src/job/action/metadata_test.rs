use super::*;

#[test]
fn parse_node_action() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("action.yml"),
        r#"
name: 'Test Action'
inputs:
  token:
    description: 'GitHub token'
    required: true
    default: '${{ github.token }}'
  path:
    description: 'Path to checkout'
    required: false
outputs:
  result:
    description: 'The result'
runs:
  using: 'node20'
  main: 'dist/index.js'
  post: 'dist/cleanup.js'
  post-if: 'always()'
"#,
    )
    .unwrap();

    let metadata = load_action_metadata(tmp.path()).unwrap();
    assert_eq!(metadata.name.as_deref(), Some("Test Action"));
    assert!(metadata.runs.is_node());
    assert!(!metadata.runs.is_composite());
    assert!(!metadata.runs.is_docker());
    assert_eq!(metadata.runs.main.as_deref(), Some("dist/index.js"));
    assert_eq!(metadata.runs.post.as_deref(), Some("dist/cleanup.js"));
    assert!(metadata.runs.pre.is_none());

    assert_eq!(metadata.inputs.len(), 2);
    let token_input = &metadata.inputs["token"];
    assert_eq!(token_input.default.as_deref(), Some("${{ github.token }}"));
}

#[test]
fn parse_composite_action() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("action.yml"),
        r#"
name: 'Composite Action'
runs:
  using: 'composite'
  steps:
    - run: echo "step 1"
      shell: bash
    - run: echo "step 2"
      shell: bash
"#,
    )
    .unwrap();

    let metadata = load_action_metadata(tmp.path()).unwrap();
    assert!(metadata.runs.is_composite());
    assert!(!metadata.runs.is_node());
    assert!(metadata.runs.steps.is_some());
    assert_eq!(metadata.runs.steps.as_ref().unwrap().len(), 2);
}

#[test]
fn parse_docker_action() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("action.yml"),
        r#"
name: 'Docker Action'
runs:
  using: 'docker'
  image: 'Dockerfile'
"#,
    )
    .unwrap();

    let metadata = load_action_metadata(tmp.path()).unwrap();
    assert!(metadata.runs.is_docker());
    assert_eq!(metadata.runs.image.as_deref(), Some("Dockerfile"));
}

#[test]
fn parse_docker_action_full_fields() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("action.yml"),
        r#"
name: 'Full Docker Action'
inputs:
  greeting:
    description: 'Who to greet'
    default: 'World'
runs:
  using: 'docker'
  image: 'docker://node:18-alpine'
  entrypoint: '/entrypoint.sh'
  args:
    - '--name'
    - '${{ inputs.greeting }}'
  pre-entrypoint: '/pre.sh'
  post-entrypoint: '/post.sh'
  env:
    MY_VAR: 'hello'
    ANOTHER: 'world'
"#,
    )
    .unwrap();

    let metadata = load_action_metadata(tmp.path()).unwrap();
    assert!(metadata.runs.is_docker());
    assert_eq!(
        metadata.runs.image.as_deref(),
        Some("docker://node:18-alpine")
    );
    assert_eq!(metadata.runs.entrypoint.as_deref(), Some("/entrypoint.sh"));
    assert_eq!(
        metadata.runs.args.as_deref(),
        Some(&["--name".to_string(), "${{ inputs.greeting }}".to_string()][..])
    );
    assert_eq!(metadata.runs.pre_entrypoint.as_deref(), Some("/pre.sh"));
    assert_eq!(metadata.runs.post_entrypoint.as_deref(), Some("/post.sh"));

    let env = metadata.runs.env.as_ref().unwrap();
    assert_eq!(env.get("MY_VAR").unwrap(), "hello");
    assert_eq!(env.get("ANOTHER").unwrap(), "world");
}

#[test]
fn parse_docker_action_minimal() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("action.yml"),
        r#"
name: 'Minimal Docker Action'
runs:
  using: 'docker'
  image: 'alpine:latest'
"#,
    )
    .unwrap();

    let metadata = load_action_metadata(tmp.path()).unwrap();
    assert!(metadata.runs.is_docker());
    assert_eq!(metadata.runs.image.as_deref(), Some("alpine:latest"));
    assert!(metadata.runs.entrypoint.is_none());
    assert!(metadata.runs.args.is_none());
    assert!(metadata.runs.pre_entrypoint.is_none());
    assert!(metadata.runs.post_entrypoint.is_none());
    assert!(metadata.runs.env.is_none());
}

#[test]
fn inputs_with_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("action.yml"),
        r#"
name: 'Defaults'
inputs:
  flavor:
    default: 'vanilla'
  size:
    description: 'size of widget'
runs:
  using: 'node20'
  main: 'index.js'
"#,
    )
    .unwrap();

    let metadata = load_action_metadata(tmp.path()).unwrap();
    assert_eq!(
        metadata.inputs["flavor"].default.as_deref(),
        Some("vanilla")
    );
    assert!(metadata.inputs["size"].default.is_none());
}

#[test]
fn pre_and_post_scripts() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("action.yml"),
        r#"
name: 'Full Lifecycle'
runs:
  using: 'node20'
  pre: 'dist/pre.js'
  pre-if: 'always()'
  main: 'dist/main.js'
  post: 'dist/post.js'
  post-if: 'success()'
"#,
    )
    .unwrap();

    let metadata = load_action_metadata(tmp.path()).unwrap();
    assert_eq!(metadata.runs.pre.as_deref(), Some("dist/pre.js"));
    assert_eq!(metadata.runs.main.as_deref(), Some("dist/main.js"));
    assert_eq!(metadata.runs.post.as_deref(), Some("dist/post.js"));
}

#[test]
fn missing_file_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    let result = load_action_metadata(tmp.path());
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("no action.yml"));
}

#[test]
fn yaml_alternative_extension() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("action.yaml"),
        r#"
name: 'YAML Extension'
runs:
  using: 'node16'
  main: 'index.js'
"#,
    )
    .unwrap();

    let metadata = load_action_metadata(tmp.path()).unwrap();
    assert_eq!(metadata.name.as_deref(), Some("YAML Extension"));
    assert!(metadata.runs.is_node());
}
