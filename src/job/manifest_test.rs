use super::*;
use serde_json::json;

#[test]
fn normalize_basic_manifest() {
    let raw = json!({
        "plan": {
            "planType": "actions",
            "planId": "plan-123",
            "version": 0
        },
        "jobId": "job-456",
        "timeline": { "id": "timeline-789" },
        "steps": [
            {
                "type": "action",
                "reference": { "type": "script" },
                "id": "step-1",
                "displayNameToken": { "type": 0, "lit": "Hello" },
                "condition": "success()",
                "continueOnError": null,
                "timeoutInMinutes": null,
                "inputs": {
                    "type": 2,
                    "map": [
                        {
                            "Key": { "type": 0, "lit": "script" },
                            "Value": { "type": 0, "lit": "echo hello" }
                        }
                    ]
                }
            }
        ],
        "variables": {
            "MY_VAR": { "value": "hello" }
        },
        "resources": {
            "endpoints": [
                {
                    "name": "SystemVssConnection",
                    "url": "https://example.com/",
                    "authorization": {
                        "parameters": { "AccessToken": "tok" },
                        "scheme": "OAuth"
                    }
                }
            ]
        },
        "contextData": {
            "github": {
                "t": 2,
                "d": [
                    { "k": "repository", "v": "owner/repo" },
                    { "k": "sha", "v": "abc123" }
                ]
            }
        },
        "mask": [],
        "fileTable": [".github/workflows/test.yml"]
    });

    let normalized = normalize_manifest(&raw);
    let manifest: crate::job::schema::JobManifest =
        serde_json::from_value(normalized).expect("should deserialize");

    assert_eq!(manifest.plan.plan_id, "plan-123");
    assert_eq!(manifest.plan.job_id, "job-456");
    assert_eq!(manifest.plan.timeline_id, "timeline-789");
    assert_eq!(manifest.steps.len(), 1);
    assert_eq!(manifest.steps[0].display_name, "Hello");
    assert_eq!(
        manifest.steps[0].inputs.get("script").unwrap(),
        "echo hello"
    );
    assert_eq!(
        manifest
            .context_data
            .get("github")
            .unwrap()
            .get("repository")
            .unwrap(),
        "owner/repo"
    );
}

#[test]
fn normalize_step_extracts_display_name_from_token() {
    let step = json!({
        "id": "s1",
        "displayNameToken": { "type": 0, "lit": "My Step" },
        "reference": { "type": "script" },
        "inputs": { "type": 2, "map": [] }
    });

    let result = normalize_step(&step);
    assert_eq!(result.get("displayName").unwrap(), "My Step");
}

#[test]
fn normalize_step_falls_back_to_name() {
    let step = json!({
        "id": "s1",
        "name": "__run",
        "reference": { "type": "script" },
        "inputs": { "type": 2, "map": [] }
    });

    let result = normalize_step(&step);
    assert_eq!(result.get("displayName").unwrap(), "__run");
}

#[test]
fn template_token_string() {
    let token = json!({ "type": 0, "lit": "hello" });
    assert_eq!(template_token_to_value(&token), json!("hello"));
}

#[test]
fn template_token_boolean() {
    let token = json!({ "type": 5, "bool": true });
    assert_eq!(template_token_to_value(&token), json!(true));
}

#[test]
fn template_token_number() {
    let token = json!({ "type": 6, "num": 42 });
    assert_eq!(template_token_to_value(&token), json!(42));
}

#[test]
fn template_token_null() {
    let token = json!({ "type": 7 });
    assert_eq!(template_token_to_value(&token), Value::Null);
}

#[test]
fn template_token_expression() {
    let token = json!({ "type": 3, "expr": "github.ref" });
    assert_eq!(template_token_to_value(&token), json!("${{ github.ref }}"));
}

#[test]
fn template_token_sequence() {
    let token = json!({
        "type": 1,
        "seq": [
            { "type": 0, "lit": "a" },
            { "type": 0, "lit": "b" }
        ]
    });
    assert_eq!(template_token_to_value(&token), json!(["a", "b"]));
}

#[test]
fn template_token_mapping() {
    let token = json!({
        "type": 2,
        "map": [
            {
                "Key": { "type": 0, "lit": "script" },
                "Value": { "type": 0, "lit": "echo hi" }
            }
        ]
    });
    let result = template_token_to_map(&token);
    assert_eq!(result, json!({ "script": "echo hi" }));
}

#[test]
fn template_token_mapping_lowercase_keys() {
    let token = json!({
        "type": 2,
        "map": [
            {
                "key": { "type": 0, "lit": "x" },
                "value": { "type": 0, "lit": "y" }
            }
        ]
    });
    let result = template_token_to_map(&token);
    assert_eq!(result, json!({ "x": "y" }));
}

#[test]
fn template_token_plain_passthrough() {
    let token = json!("just a string");
    assert_eq!(template_token_to_value(&token), json!("just a string"));
}

#[test]
fn context_data_string() {
    let ctx = json!({ "t": 0, "s": "hello" });
    assert_eq!(normalize_context_data(&ctx), json!("hello"));
}

#[test]
fn context_data_bool() {
    let ctx = json!({ "t": 3, "b": true });
    assert_eq!(normalize_context_data(&ctx), json!(true));
}

#[test]
fn context_data_number() {
    let ctx = json!({ "t": 4, "n": 99 });
    assert_eq!(normalize_context_data(&ctx), json!(99));
}

#[test]
fn context_data_array() {
    let ctx = json!({
        "t": 1,
        "a": [
            { "t": 0, "s": "a" },
            { "t": 0, "s": "b" }
        ]
    });
    assert_eq!(normalize_context_data(&ctx), json!(["a", "b"]));
}

#[test]
fn context_data_dictionary() {
    let ctx = json!({
        "t": 2,
        "d": [
            { "k": "repository", "v": "owner/repo" },
            { "k": "sha", "v": "abc" }
        ]
    });
    let result = normalize_context_data(&ctx);
    assert_eq!(result, json!({ "repository": "owner/repo", "sha": "abc" }));
}

#[test]
fn context_data_nested_dict_with_plain_string_values() {
    // In real manifests, simple string values inside dict don't have {t: 0, s: ...}
    // — they're just bare strings
    let ctx = json!({
        "t": 2,
        "d": [
            { "k": "ref", "v": "refs/heads/main" },
            { "k": "sha", "v": "abc123" }
        ]
    });
    let result = normalize_context_data(&ctx);
    assert_eq!(
        result.get("ref").unwrap().as_str().unwrap(),
        "refs/heads/main"
    );
}

#[test]
fn context_data_plain_object_passthrough() {
    let ctx = json!({ "key": "value" });
    assert_eq!(normalize_context_data(&ctx), json!({ "key": "value" }));
}

#[test]
fn normalize_preserves_resources() {
    let raw = json!({
        "plan": { "planId": "p" },
        "jobId": "j",
        "timeline": { "id": "t" },
        "resources": {
            "endpoints": [{
                "name": "SystemVssConnection",
                "url": "https://example.com/",
                "authorization": {
                    "parameters": { "AccessToken": "tok" },
                    "scheme": "OAuth"
                }
            }]
        }
    });

    let normalized = normalize_manifest(&raw);
    let manifest: crate::job::schema::JobManifest =
        serde_json::from_value(normalized).expect("should deserialize");

    assert_eq!(manifest.access_token().unwrap(), "tok");
}

#[test]
fn normalize_passes_through_mask_and_file_table() {
    let raw = json!({
        "plan": { "planId": "p" },
        "jobId": "j",
        "timeline": { "id": "t" },
        "mask": [{ "type": "regex", "value": "secret.*" }],
        "fileTable": ["file1.yml", "file2.yml"]
    });

    let normalized = normalize_manifest(&raw);
    let manifest: crate::job::schema::JobManifest =
        serde_json::from_value(normalized).expect("should deserialize");

    assert_eq!(manifest.mask.len(), 1);
    assert_eq!(manifest.file_table.len(), 2);
}

#[test]
fn normalize_handles_missing_optional_fields() {
    let raw = json!({
        "plan": { "planId": "p" },
        "jobId": "j",
        "timeline": { "id": "t" }
    });

    let normalized = normalize_manifest(&raw);
    let manifest: crate::job::schema::JobManifest =
        serde_json::from_value(normalized).expect("should deserialize");

    assert!(manifest.steps.is_empty());
    assert!(manifest.variables.is_empty());
}

#[test]
fn normalize_step_preserves_all_fields() {
    let step = json!({
        "id": "s1",
        "type": "action",
        "displayNameToken": { "type": 0, "lit": "My Step" },
        "reference": { "type": "script" },
        "condition": "success()",
        "continueOnError": null,
        "timeoutInMinutes": 10,
        "order": 3,
        "contextName": "__run",
        "inputs": {
            "type": 2,
            "map": [
                {
                    "Key": { "type": 0, "lit": "script" },
                    "Value": { "type": 0, "lit": "echo test" }
                }
            ]
        },
        "environment": {
            "type": 2,
            "map": [
                {
                    "Key": { "type": 0, "lit": "FOO" },
                    "Value": { "type": 0, "lit": "bar" }
                }
            ]
        }
    });

    let result = normalize_step(&step);
    assert_eq!(result.get("id").unwrap(), "s1");
    assert_eq!(result.get("type").unwrap(), "action");
    assert_eq!(result.get("displayName").unwrap(), "My Step");
    assert_eq!(result.get("condition").unwrap(), "success()");
    assert!(result.get("continueOnError").unwrap().is_null());
    assert_eq!(result.get("timeoutInMinutes").unwrap(), 10);
    assert_eq!(result.get("order").unwrap(), 3);
    assert_eq!(result.get("contextName").unwrap(), "__run");
    assert_eq!(
        result.get("inputs").unwrap().get("script").unwrap(),
        "echo test"
    );
    assert_eq!(
        result.get("environment").unwrap().get("FOO").unwrap(),
        "bar"
    );
}
