use super::*;

#[test]
fn each_stage_runs_its_own_entry_point() {
    assert_eq!(ActionStage::Pre.entry_point(), "pre");
    assert_eq!(ActionStage::Main.entry_point(), "main");
    assert_eq!(ActionStage::Post.entry_point(), "post");
}

#[test]
fn pre_and_post_stages_prefix_the_main_display_name() {
    assert_eq!(ActionStage::Pre.display_name("Run cache"), "Pre Run cache");
    assert_eq!(ActionStage::Main.display_name("Run cache"), "Run cache");
    assert_eq!(
        ActionStage::Post.display_name("Run cache"),
        "Post Run cache"
    );
}

fn metadata(yaml: &str) -> ActionMetadata {
    serde_yaml::from_str(yaml).unwrap()
}

const NODE_WITH_PRE: &str = "runs:\n  using: node20\n  main: index.js\n  pre: pre.js\n";

#[test]
fn local_action_with_pre_warns_that_pre_is_unsupported() {
    let source = ActionSource::Local {
        path: "./my-action".into(),
    };

    let warning = local_pre_warning(&source, &metadata(NODE_WITH_PRE));

    assert_eq!(
        warning.as_deref(),
        Some("##[warning]`pre` execution is not supported for local action from './my-action'")
    );
}

#[test]
fn local_action_without_pre_does_not_warn() {
    let source = ActionSource::Local {
        path: "./my-action".into(),
    };
    let no_pre = metadata("runs:\n  using: node20\n  main: index.js\n");

    assert_eq!(local_pre_warning(&source, &no_pre), None);
}

#[test]
fn remote_action_with_pre_does_not_warn() {
    let source = ActionSource::remote("actions", "cache", "v4", None).unwrap();

    assert_eq!(local_pre_warning(&source, &metadata(NODE_WITH_PRE)), None);
}

fn action_step(inputs: &[(&str, &str)]) -> Step {
    Step {
        id: "1".into(),
        display_name: "Run action".into(),
        reference: Default::default(),
        inputs: inputs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
        condition: None,
        timeout_in_minutes: None,
        continue_on_error: None,
        order: 1,
        environment: None,
        context_name: None,
    }
}

#[test]
fn action_inputs_take_the_evaluated_with_verbatim_and_evaluate_defaults() {
    let metadata = metadata(
        "inputs:\n  value: {}\n  target:\n    default: ${{ env.BASE }}/out\n  unset: {}\nruns:\n  using: node20\n  main: index.js\n",
    );
    let step = action_step(&[("value", "${{ secrets.MY_SECRET }}")]);
    let state = crate::job::execute::JobState::new(
        Default::default(),
        HashMap::from([("MY_SECRET".to_string(), "leaked".to_string())]),
        serde_json::json!({}),
    );
    let env = HashMap::from([("BASE".to_string(), "dist".to_string())]);

    let inputs = build_action_inputs(
        &metadata,
        &step,
        &ExprContext::new(&env, &state, false, false),
    );

    assert_eq!(inputs["INPUT_VALUE"], "${{ secrets.MY_SECRET }}");
    assert_eq!(inputs["INPUT_TARGET"], "dist/out");
    assert!(!inputs.contains_key("INPUT_UNSET"));
}
