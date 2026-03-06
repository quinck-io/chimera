use std::collections::HashMap;

use super::*;
use crate::job::schema::{Step, StepReference, StepReferenceKind};

fn make_action_step(name: &str, kind: StepReferenceKind) -> Step {
    Step {
        id: "1".into(),
        display_name: name.into(),
        reference: StepReference {
            name: name.into(),
            kind,
            ..Default::default()
        },
        inputs: HashMap::new(),
        condition: None,
        timeout_in_minutes: None,
        continue_on_error: false,
        order: 1,
        environment: None,
        context_name: None,
    }
}

#[test]
fn resolve_remote_repository() {
    let mut step = make_action_step("actions/checkout", StepReferenceKind::Repository);
    step.reference.git_ref = Some("v4".into());

    let source = resolve_action(&step).unwrap();
    match source {
        ActionSource::Remote {
            owner,
            repo,
            git_ref,
            path,
        } => {
            assert_eq!(owner, "actions");
            assert_eq!(repo, "checkout");
            assert_eq!(git_ref, "v4");
            assert!(path.is_none());
        }
        _ => panic!("expected Remote"),
    }
}

#[test]
fn resolve_remote_with_path() {
    let mut step = make_action_step("actions/aws", StepReferenceKind::Repository);
    step.reference.git_ref = Some("v1".into());
    step.reference.path = Some("configure-credentials".into());

    let source = resolve_action(&step).unwrap();
    match source {
        ActionSource::Remote { path, .. } => {
            assert_eq!(path.as_deref(), Some("configure-credentials"));
        }
        _ => panic!("expected Remote"),
    }
}

#[test]
fn resolve_local_self_repository() {
    let mut step = make_action_step(".github/actions/my-action", StepReferenceKind::Repository);
    step.reference.repository_type = Some("self".into());
    step.reference.path = Some(".github/actions/my-action".into());

    let source = resolve_action(&step).unwrap();
    match source {
        ActionSource::Local { path } => {
            assert_eq!(path.to_string_lossy(), ".github/actions/my-action");
        }
        _ => panic!("expected Local"),
    }
}

#[test]
fn resolve_docker_container_registry() {
    let mut step = make_action_step("docker://node:18", StepReferenceKind::ContainerRegistry);
    step.reference.image = Some("node:18".into());

    let source = resolve_action(&step).unwrap();
    match source {
        ActionSource::Docker { image } => {
            assert_eq!(image, "node:18");
        }
        _ => panic!("expected Docker"),
    }
}

#[test]
fn resolve_fallback_name_parsing() {
    let step = make_action_step("actions/checkout@v4", StepReferenceKind::default());

    let source = resolve_action(&step).unwrap();
    match source {
        ActionSource::Remote {
            owner,
            repo,
            git_ref,
            path,
        } => {
            assert_eq!(owner, "actions");
            assert_eq!(repo, "checkout");
            assert_eq!(git_ref, "v4");
            assert!(path.is_none());
        }
        _ => panic!("expected Remote"),
    }
}

#[test]
fn resolve_fallback_with_subpath() {
    let step = make_action_step(
        "actions/aws/configure-credentials@v1",
        StepReferenceKind::default(),
    );

    let source = resolve_action(&step).unwrap();
    match source {
        ActionSource::Remote {
            owner,
            repo,
            git_ref,
            path,
        } => {
            assert_eq!(owner, "actions");
            assert_eq!(repo, "aws");
            assert_eq!(git_ref, "v1");
            assert_eq!(path.as_deref(), Some("configure-credentials"));
        }
        _ => panic!("expected Remote"),
    }
}
