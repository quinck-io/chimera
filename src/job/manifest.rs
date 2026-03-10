use serde_json::{Map, Value, json};
use tracing::debug;

/// Convert a raw job manifest (template token format) into normalized plain JSON
/// that our JobManifest struct can deserialize.
pub fn normalize_manifest(raw: &Value) -> Value {
    let obj = match raw.as_object() {
        Some(o) => o,
        None => return raw.clone(),
    };

    let mut result = Map::new();

    // Plan: combine plan.planId + jobId + timeline.id
    let plan_id = obj
        .get("plan")
        .and_then(|p| p.get("planId"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let job_id = obj
        .get("jobId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let timeline_id = obj
        .get("timeline")
        .and_then(|t| t.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    result.insert(
        "plan".into(),
        json!({
            "planId": plan_id,
            "jobId": job_id,
            "timelineId": timeline_id,
        }),
    );

    // Steps — assign incrementing order (1-based) when not present in raw manifest
    if let Some(steps) = obj.get("steps").and_then(|s| s.as_array()) {
        let normalized_steps: Vec<Value> = steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut step = normalize_step(s);
                if let Some(obj) = step.as_object_mut() {
                    obj.entry("order").or_insert(json!(i + 1));
                }
                step
            })
            .collect();
        result.insert("steps".into(), Value::Array(normalized_steps));
    } else {
        result.insert("steps".into(), json!([]));
    }

    // Variables: already {key: {value: "...", isSecret: bool}} — just pass through
    // but some might have just {value: "..."} without isSecret
    if let Some(vars) = obj.get("variables") {
        result.insert("variables".into(), vars.clone());
    }

    // Resources: endpoints are already in the right format
    if let Some(resources) = obj.get("resources") {
        result.insert("resources".into(), resources.clone());
    }

    // ContextData: uses PipelineContextData format {t: 2, d: [{k: ..., v: ...}]}
    if let Some(ctx) = obj.get("contextData") {
        result.insert("contextData".into(), normalize_context_data(ctx));
    } else {
        result.insert("contextData".into(), json!({}));
    }

    // Normalize container fields — environment maps inside may be template tokens
    if let Some(jc) = obj.get("jobContainer") {
        let normalized_jc = normalize_container_spec(jc);
        debug!(raw = %jc, normalized = %normalized_jc, "normalizing jobContainer");
        result.insert("jobContainer".into(), normalized_jc);
    } else {
        debug!("no jobContainer field in raw manifest");
    }
    if let Some(sc) = obj
        .get("jobServiceContainers")
        .or_else(|| obj.get("serviceContainers"))
    {
        let normalized_sc = normalize_service_containers(sc);
        if !normalized_sc.is_null() {
            result.insert("serviceContainers".into(), normalized_sc);
        }
    }

    // Pass through mask, fileTable, and actionsDownloadInfos
    if let Some(mask) = obj.get("mask") {
        result.insert("mask".into(), mask.clone());
    }
    if let Some(ft) = obj.get("fileTable") {
        result.insert("fileTable".into(), ft.clone());
    }
    if let Some(adi) = obj.get("actionsDownloadInfos") {
        result.insert("actionsDownloadInfos".into(), adi.clone());
    }

    Value::Object(result)
}

fn normalize_step(step: &Value) -> Value {
    let obj = match step.as_object() {
        Some(o) => o,
        None => return step.clone(),
    };

    let mut result = Map::new();

    // id
    if let Some(id) = obj.get("id") {
        result.insert("id".into(), id.clone());
    }

    // displayName: extract from displayNameToken.lit
    let display_name = obj
        .get("displayNameToken")
        .and_then(|t| t.get("lit"))
        .and_then(|v| v.as_str())
        .or_else(|| obj.get("name").and_then(|v| v.as_str()))
        .unwrap_or("(unnamed step)")
        .to_string();
    result.insert("displayName".into(), Value::String(display_name));

    // reference
    if let Some(reference) = obj.get("reference") {
        result.insert("reference".into(), reference.clone());
    }

    // inputs: convert from template token to plain map
    if let Some(inputs) = obj.get("inputs") {
        result.insert("inputs".into(), template_token_to_map(inputs));
    }

    // condition: might be a template token (expression or string)
    if let Some(cond) = obj.get("condition") {
        result.insert("condition".into(), template_token_to_value(cond));
    }

    // continueOnError: might be null, bool, template token, or absent
    if let Some(coe) = obj.get("continueOnError") {
        result.insert("continueOnError".into(), template_token_to_value(coe));
    }

    // timeoutInMinutes: might be a template token
    if let Some(t) = obj.get("timeoutInMinutes") {
        result.insert("timeoutInMinutes".into(), template_token_to_value(t));
    }

    // order
    if let Some(o) = obj.get("order") {
        result.insert("order".into(), o.clone());
    }

    // environment: might be template token format
    if let Some(env) = obj.get("environment") {
        result.insert("environment".into(), template_token_to_map(env));
    }

    // step-level type (e.g. "action")
    if let Some(t) = obj.get("type") {
        result.insert("type".into(), t.clone());
    }

    // contextName
    if let Some(cn) = obj.get("contextName") {
        result.insert("contextName".into(), cn.clone());
    }

    Value::Object(result)
}

/// Convert a TemplateToken to a plain serde_json::Value.
///
/// Token types:
/// 0 = String: {type: 0, lit: "value"}
/// 1 = Sequence: {type: 1, seq: [...]}
/// 2 = Mapping: {type: 2, map: [{Key: {...}, Value: {...}}]}
/// 3 = Expression: {type: 3, expr: "..."}
/// 5 = Boolean: {type: 5, bool: true/false}
/// 6 = Number: {type: 6, num: 42}
/// 7 = Null: {type: 7}
fn template_token_to_value(token: &Value) -> Value {
    // If it's already a plain string/number/bool/null, return as-is
    if token.is_string() || token.is_number() || token.is_boolean() || token.is_null() {
        return token.clone();
    }

    // If it's an array, convert each element
    if let Some(arr) = token.as_array() {
        return Value::Array(arr.iter().map(template_token_to_value).collect());
    }

    let obj = match token.as_object() {
        Some(o) => o,
        None => return token.clone(),
    };

    let token_type = obj.get("type").and_then(|t| t.as_u64()).unwrap_or(999);

    match token_type {
        0 => {
            // String token
            obj.get("lit")
                .cloned()
                .unwrap_or(Value::String(String::new()))
        }
        1 => {
            // Sequence — either a string template (contains expression tokens)
            // or a literal array (all elements are non-expression tokens).
            if let Some(seq) = obj.get("seq").and_then(|s| s.as_array()) {
                let has_expression = seq.iter().any(|el| {
                    el.as_object()
                        .and_then(|o| o.get("type"))
                        .and_then(|t| t.as_u64())
                        == Some(3)
                });

                if has_expression {
                    // Template string: concatenate all parts into a single string,
                    // preserving ${{ }} wrappers around expression tokens so that
                    // resolve_template can evaluate them later.
                    let concatenated: String = seq
                        .iter()
                        .map(|el| match template_token_to_value(el) {
                            Value::String(s) => s,
                            other => other.to_string(),
                        })
                        .collect();
                    Value::String(concatenated)
                } else {
                    Value::Array(seq.iter().map(template_token_to_value).collect())
                }
            } else {
                Value::Array(vec![])
            }
        }
        2 => {
            // Mapping
            template_token_to_map(token)
        }
        3 => {
            // Expression — return as string with ${{ }} wrapper
            let expr = obj.get("expr").and_then(|e| e.as_str()).unwrap_or("");
            Value::String(format!("${{{{ {expr} }}}}"))
        }
        5 => {
            // Boolean
            obj.get("bool").cloned().unwrap_or(Value::Bool(false))
        }
        6 => {
            // Number
            obj.get("num").cloned().unwrap_or(json!(0))
        }
        7 => Value::Null,
        _ => {
            // Unknown token type or plain object — pass through
            token.clone()
        }
    }
}

/// Convert a Mapping TemplateToken to a plain JSON object.
fn template_token_to_map(token: &Value) -> Value {
    let obj = match token.as_object() {
        Some(o) => o,
        None => return json!({}),
    };

    // Check if this is a mapping token (type 2 with map array)
    if let Some(map_arr) = obj.get("map").and_then(|m| m.as_array()) {
        let mut result = Map::new();
        for entry in map_arr {
            let key = entry
                .get("Key")
                .or_else(|| entry.get("key"))
                .map(template_token_to_value)
                .and_then(|v| match v {
                    Value::String(s) => Some(s),
                    _ => v.as_str().map(|s| s.to_string()),
                });
            let value = entry
                .get("Value")
                .or_else(|| entry.get("value"))
                .map(template_token_to_value)
                .unwrap_or(Value::Null);

            if let Some(k) = key {
                result.insert(k, value);
            }
        }
        return Value::Object(result);
    }

    // Not a mapping token — maybe it's already a plain object
    if obj.get("type").is_none() {
        return token.clone();
    }

    // Fallback: try to convert as a generic token
    template_token_to_value(token)
}

/// Normalize service containers from the raw manifest.
///
/// GitHub sends service containers as either:
/// - A JSON array of container specs (already normalized)
/// - A template token map (type=2) where each key is the service alias
///   and each value is a container spec template token
fn normalize_service_containers(sc: &Value) -> Value {
    // Already a plain array
    if let Some(arr) = sc.as_array() {
        let normalized: Vec<Value> = arr.iter().map(normalize_container_spec).collect();
        return Value::Array(normalized);
    }

    // Template token map: {type: 2, map: [{Key: "redis", Value: {spec...}}]}
    // Convert to array, injecting the key as the "alias" field.
    let resolved = template_token_to_map(sc);
    if let Some(obj) = resolved.as_object() {
        let normalized: Vec<Value> = obj
            .iter()
            .filter_map(|(alias, spec)| {
                let mut container = normalize_container_spec(spec);
                if let Some(c) = container.as_object_mut()
                    && c.contains_key("image")
                {
                    c.entry("alias").or_insert(Value::String(alias.clone()));
                    return Some(container);
                }
                None
            })
            .collect();
        if normalized.is_empty() {
            return Value::Null;
        }
        return Value::Array(normalized);
    }

    Value::Null
}

/// Normalize a container spec (jobContainer or service container).
///
/// GitHub always sends container specs as mapping template tokens (type=2),
/// or null when no container is configured. The token's map entries contain
/// keys like "image", "environment", etc. as nested template tokens.
fn normalize_container_spec(spec: &Value) -> Value {
    if spec.is_null() {
        return Value::Null;
    }

    // Resolve the template token to a plain map (handles type=2 mapping tokens
    // and passes through plain objects unchanged)
    let resolved = template_token_to_map(spec);

    let obj = match resolved.as_object() {
        Some(o) => o,
        None => return Value::Null,
    };

    if obj.is_empty() || !obj.contains_key("image") {
        return Value::Null;
    }

    // The resolved map values may themselves still be template tokens
    // (e.g. environment as a nested type=2 mapping), so resolve each field.
    let mut result = Map::new();

    if let Some(image) = obj.get("image") {
        result.insert("image".into(), template_token_to_value(image));
    }

    if let Some(env) = obj.get("environment") {
        result.insert("environment".into(), template_token_to_map(env));
    }

    if let Some(ports) = obj.get("ports") {
        result.insert("ports".into(), template_token_to_value(ports));
    }

    if let Some(volumes) = obj.get("volumes") {
        result.insert("volumes".into(), template_token_to_value(volumes));
    }

    if let Some(options) = obj.get("options") {
        result.insert("options".into(), template_token_to_value(options));
    }

    if let Some(creds) = obj.get("credentials") {
        result.insert("credentials".into(), creds.clone());
    }

    if let Some(alias) = obj.get("alias") {
        result.insert("alias".into(), template_token_to_value(alias));
    }

    Value::Object(result)
}

/// Convert PipelineContextData to plain JSON.
///
/// Context data types:
/// t=0: String {t: 0, s: "value"}
/// t=1: Array {t: 1, a: [...]}
/// t=2: Dictionary {t: 2, d: [{k: "key", v: {...}}]}
/// t=3: Bool {t: 3, b: true/false}
/// t=4: Number {t: 4, n: 42}
fn normalize_context_data(ctx: &Value) -> Value {
    // Plain values pass through
    if ctx.is_string() || ctx.is_number() || ctx.is_boolean() || ctx.is_null() {
        return ctx.clone();
    }

    if let Some(arr) = ctx.as_array() {
        return Value::Array(arr.iter().map(normalize_context_data).collect());
    }

    let obj = match ctx.as_object() {
        Some(o) => o,
        None => return ctx.clone(),
    };

    // Check for PipelineContextData format (has "t" field)
    if let Some(t) = obj.get("t").and_then(|t| t.as_u64()) {
        return match t {
            0 => {
                // String
                obj.get("s")
                    .cloned()
                    .unwrap_or(Value::String(String::new()))
            }
            1 => {
                // Array
                if let Some(arr) = obj.get("a").and_then(|a| a.as_array()) {
                    Value::Array(arr.iter().map(normalize_context_data).collect())
                } else {
                    Value::Array(vec![])
                }
            }
            2 => {
                // Dictionary
                let mut result = Map::new();
                if let Some(entries) = obj.get("d").and_then(|d| d.as_array()) {
                    for entry in entries {
                        if let Some(k) = entry.get("k").and_then(|k| k.as_str()) {
                            let v = entry
                                .get("v")
                                .map(normalize_context_data)
                                .unwrap_or(Value::Null);
                            result.insert(k.to_string(), v);
                        }
                    }
                }
                Value::Object(result)
            }
            3 => {
                // Bool
                obj.get("b").cloned().unwrap_or(Value::Bool(false))
            }
            4 => {
                // Number
                obj.get("n").cloned().unwrap_or(json!(0))
            }
            _ => ctx.clone(),
        };
    }

    // No "t" field — might be a plain object, recurse into values
    let mut result = Map::new();
    for (k, v) in obj {
        result.insert(k.clone(), normalize_context_data(v));
    }
    Value::Object(result)
}

#[cfg(test)]
#[path = "manifest_test.rs"]
mod manifest_test;
