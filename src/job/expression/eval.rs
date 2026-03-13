use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use tracing::debug;

use super::ExprContext;
use super::parser::{BinOp, Expr, PropertySegment};
use super::value::Value;

pub(crate) fn eval(expr: &Expr, ctx: &ExprContext) -> Result<Value, String> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Property(segments) => resolve_property(segments, ctx),
        Expr::Call(name, args) => eval_function(name, args, ctx),
        Expr::Binary(op, left, right) => eval_binary(op, left, right, ctx),
        Expr::Not(inner) => {
            let val = eval(inner, ctx)?;
            Ok(Value::Bool(!val.is_truthy()))
        }
    }
}

fn eval_binary(op: &BinOp, left: &Expr, right: &Expr, ctx: &ExprContext) -> Result<Value, String> {
    match op {
        BinOp::And => {
            let l = eval(left, ctx)?;
            if !l.is_truthy() {
                return Ok(l);
            }
            return eval(right, ctx);
        }
        BinOp::Or => {
            let l = eval(left, ctx)?;
            if l.is_truthy() {
                return Ok(l);
            }
            return eval(right, ctx);
        }
        _ => {}
    }

    let l = eval(left, ctx)?;
    let r = eval(right, ctx)?;

    let result = match op {
        BinOp::Eq => l == r,
        BinOp::Neq => l != r,
        BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
            let cmp = match (&l, &r) {
                (Value::Number(a), Value::Number(b)) => a.partial_cmp(b),
                _ => l.to_display().partial_cmp(&r.to_display()),
            };
            matches!(
                (op, cmp),
                (BinOp::Lt, Some(std::cmp::Ordering::Less))
                    | (BinOp::Gt, Some(std::cmp::Ordering::Greater))
                    | (
                        BinOp::Le,
                        Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                    )
                    | (
                        BinOp::Ge,
                        Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                    )
            )
        }
        BinOp::And | BinOp::Or => unreachable!(),
    };

    Ok(Value::Bool(result))
}

// ── Property resolution ─────────────────────────────────────────────

fn segment_to_key(seg: &PropertySegment, ctx: &ExprContext) -> Result<String, String> {
    match seg {
        PropertySegment::Dot(name) => Ok(name.clone()),
        PropertySegment::Bracket(expr) => {
            let val = eval(expr, ctx)?;
            Ok(val.to_display())
        }
        PropertySegment::Wildcard => Err("wildcard cannot be used as a key".into()),
    }
}

fn resolve_property(segments: &[PropertySegment], ctx: &ExprContext) -> Result<Value, String> {
    if segments.is_empty() {
        return Ok(Value::Null);
    }

    let root = match &segments[0] {
        PropertySegment::Dot(name) => name.as_str(),
        _ => return Ok(Value::Null),
    };
    let rest = &segments[1..];

    match root {
        "github" => {
            // Try env lookup for the first key
            if let Some(seg) = rest.first()
                && let Ok(key) = segment_to_key(seg, ctx)
            {
                let env_key = format!("GITHUB_{}", key.to_uppercase().replace(['.', '-'], "_"));
                if let Some(val) = ctx.env.get(&env_key) {
                    if rest.len() == 1 {
                        return Ok(Value::String(val.clone()));
                    }
                    return Ok(Value::Null);
                }
            }
            walk_json(ctx.context_data, segments, ctx)
        }
        "runner" => {
            if let Some(seg) = rest.first()
                && let Ok(key) = segment_to_key(seg, ctx)
            {
                let env_key = format!("RUNNER_{}", key.to_uppercase().replace(['.', '-'], "_"));
                if let Some(val) = ctx.env.get(&env_key) {
                    return Ok(Value::String(val.clone()));
                }
            }
            Ok(Value::Null)
        }
        "env" => {
            let key = rest
                .first()
                .map(|s| segment_to_key(s, ctx))
                .transpose()?
                .unwrap_or_default();
            Ok(ctx
                .env
                .get(&key)
                .map(|v| Value::String(v.clone()))
                .unwrap_or(Value::Null))
        }
        "inputs" => {
            if let Some(seg) = rest.first() {
                let key = segment_to_key(seg, ctx)?;
                let env_key = format!("INPUT_{}", key.to_uppercase().replace([' ', '-'], "_"));
                Ok(Value::String(
                    ctx.env.get(&env_key).cloned().unwrap_or_default(),
                ))
            } else {
                Ok(Value::Null)
            }
        }
        "secrets" => {
            let key = rest
                .first()
                .map(|s| segment_to_key(s, ctx))
                .transpose()?
                .unwrap_or_default();
            Ok(ctx
                .secrets
                .get(&key)
                .map(|v| Value::String(v.clone()))
                .unwrap_or(Value::Null))
        }
        "steps" => resolve_steps(rest, ctx),
        "vars" => {
            if let Some(seg) = rest.first() {
                let key = segment_to_key(seg, ctx)?;
                if let Some(vars) = ctx.context_data.get("vars")
                    && let Some(val) = vars.get(&key)
                {
                    return Ok(json_to_value(val));
                }
            }
            Ok(Value::Null)
        }
        "needs" | "matrix" | "strategy" | "job" => walk_json(ctx.context_data, segments, ctx),
        _ => {
            debug!(context = root, "unknown expression context");
            Ok(Value::Null)
        }
    }
}

fn resolve_steps(rest: &[PropertySegment], ctx: &ExprContext) -> Result<Value, String> {
    if rest.len() < 2 {
        return Ok(Value::Null);
    }
    let step_id = segment_to_key(&rest[0], ctx)?;
    let field = segment_to_key(&rest[1], ctx)?;

    match field.as_str() {
        "outputs" if rest.len() >= 3 => {
            let output_key = segment_to_key(&rest[2], ctx)?;
            Ok(ctx
                .step_outputs
                .get(&step_id)
                .and_then(|m| m.get(&output_key))
                .map(|v| Value::String(v.clone()))
                .unwrap_or(Value::Null))
        }
        "outcome" => Ok(ctx
            .step_outcomes
            .get(&step_id)
            .map(|s| Value::String(s.outcome.clone()))
            .unwrap_or(Value::Null)),
        "conclusion" => Ok(ctx
            .step_outcomes
            .get(&step_id)
            .map(|s| Value::String(s.conclusion.clone()))
            .unwrap_or(Value::Null)),
        _ => Ok(Value::Null),
    }
}

/// Walk a JSON tree following PropertySegments, supporting wildcards.
fn walk_json(
    data: &JsonValue,
    segments: &[PropertySegment],
    ctx: &ExprContext,
) -> Result<Value, String> {
    walk_json_inner(data, segments, ctx)
}

fn walk_json_inner(
    current: &JsonValue,
    segments: &[PropertySegment],
    ctx: &ExprContext,
) -> Result<Value, String> {
    if segments.is_empty() {
        return Ok(json_to_value(current));
    }

    let seg = &segments[0];
    let rest = &segments[1..];

    match seg {
        PropertySegment::Wildcard => {
            // Fan out over all children, collect results
            let children: Vec<&JsonValue> = match current {
                JsonValue::Object(map) => map.values().collect(),
                JsonValue::Array(arr) => arr.iter().collect(),
                _ => return Ok(Value::Null),
            };
            let mut results = Vec::new();
            for child in children {
                let val = walk_json_inner(child, rest, ctx)?;
                results.push(val);
            }
            Ok(Value::Array(results))
        }
        _ => {
            let key = segment_to_key(seg, ctx)?;
            match current {
                JsonValue::Object(map) => match map.get(&key) {
                    Some(v) => walk_json_inner(v, rest, ctx),
                    None => Ok(Value::Null),
                },
                JsonValue::Array(arr) => match key.parse::<usize>() {
                    Ok(idx) => match arr.get(idx) {
                        Some(v) => walk_json_inner(v, rest, ctx),
                        None => Ok(Value::Null),
                    },
                    Err(_) => Ok(Value::Null),
                },
                _ => Ok(Value::Null),
            }
        }
    }
}

fn json_to_value(v: &JsonValue) -> Value {
    match v {
        JsonValue::String(s) => Value::String(s.clone()),
        JsonValue::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
        JsonValue::Bool(b) => Value::Bool(*b),
        JsonValue::Null => Value::Null,
        JsonValue::Array(arr) => Value::Array(arr.iter().map(json_to_value).collect()),
        JsonValue::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect(),
        ),
    }
}

// ── Built-in Functions ──────────────────────────────────────────────

fn eval_function(name: &str, args: &[Expr], ctx: &ExprContext) -> Result<Value, String> {
    // Status functions are not case-insensitive (always lowercase in practice)
    match name {
        "success" => return Ok(Value::Bool(!ctx.job_failed && !ctx.job_cancelled)),
        "failure" => return Ok(Value::Bool(ctx.job_failed)),
        "always" => return Ok(Value::Bool(true)),
        "cancelled" => return Ok(Value::Bool(ctx.job_cancelled)),
        _ => {}
    }

    match name.to_ascii_lowercase().as_str() {
        "contains" => {
            check_args(name, args, 2)?;
            let haystack = eval(&args[0], ctx)?;
            let needle = eval(&args[1], ctx)?.to_display().to_lowercase();
            match haystack {
                Value::Array(items) => Ok(Value::Bool(
                    items
                        .iter()
                        .any(|item| item.to_display().to_lowercase() == needle),
                )),
                other => {
                    let h = other.to_display().to_lowercase();
                    Ok(Value::Bool(h.contains(&needle)))
                }
            }
        }
        "startswith" => {
            check_args(name, args, 2)?;
            let s = eval(&args[0], ctx)?.to_display().to_lowercase();
            let prefix = eval(&args[1], ctx)?.to_display().to_lowercase();
            Ok(Value::Bool(s.starts_with(&prefix)))
        }
        "endswith" => {
            check_args(name, args, 2)?;
            let s = eval(&args[0], ctx)?.to_display().to_lowercase();
            let suffix = eval(&args[1], ctx)?.to_display().to_lowercase();
            Ok(Value::Bool(s.ends_with(&suffix)))
        }
        "format" => {
            if args.is_empty() {
                return Err("format() requires at least 1 argument".into());
            }
            let mut result = eval(&args[0], ctx)?.to_display();
            for (i, arg) in args[1..].iter().enumerate() {
                let val = eval(arg, ctx)?.to_display();
                result = result.replace(&format!("{{{i}}}"), &val);
            }
            result = result.replace("{{", "{").replace("}}", "}");
            Ok(Value::String(result))
        }
        "join" => {
            if args.is_empty() || args.len() > 2 {
                return Err("join() requires 1-2 arguments".into());
            }
            let val = eval(&args[0], ctx)?;
            let sep = if args.len() == 2 {
                eval(&args[1], ctx)?.to_display()
            } else {
                ",".into()
            };
            match val {
                Value::Array(items) => {
                    let strs: Vec<String> = items.iter().map(|v| v.to_display()).collect();
                    Ok(Value::String(strs.join(&sep)))
                }
                other => {
                    let s = other.to_display();
                    // Backward compat: try parsing as JSON array
                    if let Ok(JsonValue::Array(arr)) = serde_json::from_str::<JsonValue>(&s) {
                        let items: Vec<String> = arr
                            .iter()
                            .map(|v| match v {
                                JsonValue::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .collect();
                        Ok(Value::String(items.join(&sep)))
                    } else {
                        Ok(Value::String(s))
                    }
                }
            }
        }
        "tojson" => {
            check_args(name, args, 1)?;
            let val = eval(&args[0], ctx)?;
            let json_val = val.to_json();
            let pretty =
                serde_json::to_string_pretty(&json_val).unwrap_or_else(|_| val.to_display());
            Ok(Value::String(pretty))
        }
        "fromjson" => {
            check_args(name, args, 1)?;
            let s = eval(&args[0], ctx)?.to_display();
            match serde_json::from_str::<JsonValue>(&s) {
                Ok(json) => Ok(json_to_value(&json)),
                Err(_) => Ok(Value::String(s)),
            }
        }
        "hashfiles" => {
            if args.is_empty() {
                return Err("hashFiles() requires at least 1 argument".into());
            }

            let workspace_ref;
            let workspace = if let Some(ref wp) = ctx.workspace_path {
                wp.as_str()
            } else {
                workspace_ref = ctx
                    .env
                    .get("GITHUB_WORKSPACE")
                    .ok_or("hashFiles() requires GITHUB_WORKSPACE to be set")?;
                workspace_ref.as_str()
            };

            let mut patterns = Vec::new();
            for arg in args {
                patterns.push(eval(arg, ctx)?.to_display());
            }

            hash_files(workspace, &patterns)
        }

        _ => Err(format!("unknown function: {name}()")),
    }
}

fn check_args(name: &str, args: &[Expr], expected: usize) -> Result<(), String> {
    if args.len() != expected {
        Err(format!(
            "{name}() requires {expected} arguments, got {}",
            args.len()
        ))
    } else {
        Ok(())
    }
}

fn hash_files(workspace: &str, patterns: &[String]) -> Result<Value, String> {
    let workspace_path = std::path::Path::new(workspace);
    let mut matched_paths = Vec::new();

    for pattern in patterns {
        let full_pattern = workspace_path.join(pattern).to_string_lossy().to_string();
        let entries = glob::glob(&full_pattern)
            .map_err(|e| format!("invalid glob pattern '{pattern}': {e}"))?;

        for entry in entries {
            let path = entry.map_err(|e| format!("glob error: {e}"))?;
            if path.is_file() {
                matched_paths.push(path);
            }
        }
    }

    if matched_paths.is_empty() {
        return Ok(Value::String(String::new()));
    }

    matched_paths.sort();
    matched_paths.dedup();

    let mut hasher = Sha256::new();
    for path in &matched_paths {
        let contents =
            std::fs::read(path).map_err(|e| format!("failed to read '{}': {e}", path.display()))?;
        hasher.update(&contents);
    }

    Ok(Value::String(format!("{:x}", hasher.finalize())))
}
