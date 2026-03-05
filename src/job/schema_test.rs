use super::*;

fn load_fixture() -> JobManifest {
    let json = include_str!("../../tests/fixtures/job_manifest.json");
    serde_json::from_str(json).expect("fixture should parse")
}

#[test]
fn parse_minimal_manifest_with_two_steps() {
    let manifest = load_fixture();
    assert_eq!(manifest.steps.len(), 2);
    assert_eq!(manifest.steps[0].display_name, "Run echo hello");
    assert_eq!(manifest.steps[0].reference.r#type, "script");
    assert_eq!(manifest.steps[1].inputs["script"], "echo $MY_VAR");
    assert_eq!(manifest.plan.plan_id, "plan-001");
    assert_eq!(manifest.plan.job_id, "job-001");
    assert_eq!(manifest.plan.timeline_id, "timeline-001");
}

#[test]
fn manifest_with_container_field() {
    let json = r#"{
        "plan": { "planId": "p", "jobId": "j", "timelineId": "t" },
        "steps": [],
        "variables": {},
        "resources": { "endpoints": [] },
        "contextData": {},
        "jobContainer": { "image": "ubuntu:latest" },
        "serviceContainers": [{ "image": "postgres:15" }]
    }"#;
    let manifest: JobManifest = serde_json::from_str(json).unwrap();
    assert!(manifest.job_container.is_some());
    assert_eq!(manifest.service_containers.unwrap().len(), 1);
}

#[test]
fn unknown_fields_ignored() {
    let json = r#"{
        "plan": { "planId": "p", "jobId": "j", "timelineId": "t" },
        "steps": [],
        "variables": {},
        "resources": { "endpoints": [] },
        "contextData": {},
        "totallyNewField": "should be ignored",
        "jobContainer": null,
        "serviceContainers": null
    }"#;
    let manifest: JobManifest = serde_json::from_str(json).unwrap();
    assert_eq!(manifest.plan.plan_id, "p");
}

#[test]
fn variable_extraction() {
    let manifest = load_fixture();
    let token_var = &manifest.variables["system.github.token"];
    assert!(token_var.is_secret);
    assert_eq!(token_var.value, "ghp_secret123");

    let name_var = &manifest.variables["system.runner.name"];
    assert!(!name_var.is_secret);
    assert_eq!(name_var.value, "chimera-0");
}

#[test]
fn server_url_and_access_token_helpers() {
    let manifest = load_fixture();
    assert_eq!(
        manifest.server_url().unwrap(),
        "https://pipelines.actions.githubusercontent.com/abc123"
    );
    assert_eq!(manifest.access_token().unwrap(), "job-token-xyz");
    assert_eq!(manifest.repository().unwrap(), "owner/test-repo");
}
