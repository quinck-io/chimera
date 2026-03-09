use super::*;

#[test]
fn deserialize_job_container_spec_full() {
    let json = r#"{
        "image": "ubuntu:latest",
        "environment": { "FOO": "bar" },
        "ports": ["8080:8080"],
        "volumes": ["/host:/container"],
        "options": "--cpus 2",
        "credentials": { "username": "user", "password": "pass" }
    }"#;
    let spec: JobContainerSpec = serde_json::from_str(json).unwrap();
    assert_eq!(spec.image, "ubuntu:latest");
    assert_eq!(spec.environment.get("FOO").unwrap(), "bar");
    assert_eq!(spec.ports, vec!["8080:8080"]);
    assert_eq!(spec.volumes, vec!["/host:/container"]);
    assert_eq!(spec.options.as_deref(), Some("--cpus 2"));
    assert!(spec.credentials.is_some());
}

#[test]
fn deserialize_job_container_spec_minimal() {
    let json = r#"{ "image": "node:18" }"#;
    let spec: JobContainerSpec = serde_json::from_str(json).unwrap();
    assert_eq!(spec.image, "node:18");
    assert!(spec.environment.is_empty());
    assert!(spec.ports.is_empty());
    assert!(spec.volumes.is_empty());
    assert!(spec.options.is_none());
    assert!(spec.credentials.is_none());
}

#[test]
fn deserialize_service_container_spec() {
    let json = r#"{
        "image": "postgres:15",
        "ports": ["5432:5432"],
        "environment": { "POSTGRES_PASSWORD": "test" },
        "alias": "db"
    }"#;
    let spec: ServiceContainerSpec = serde_json::from_str(json).unwrap();
    assert_eq!(spec.image, "postgres:15");
    assert_eq!(spec.ports, vec!["5432:5432"]);
    assert_eq!(spec.environment.get("POSTGRES_PASSWORD").unwrap(), "test");
    assert_eq!(spec.alias.as_deref(), Some("db"));
}
