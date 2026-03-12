use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use tracing::debug;

use super::ExprContext;
use super::parser::{BinOp, Expr};
use super::value::Value;

pub(crate) fn eval(expr: &Expr, ctx: &ExprContext) -> Result<Value, String> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Property(parts) => resolve_property(parts, ctx),
        Expr::Call(name, args) => eval_function(name, args, ctx),
        Expr::Binary(op, left, right) => eval_binary(op, left, right, ctx),
        Expr::Not(inner) => {
            let val = eval(inner, ctx)?;
            Ok(Value::Bool(!val.is_truthy()))
        }
    }
}

fn eval_binary(op: &BinOp, left: &Expr, right: &Expr, ctx: &ExprContext) -> Result<Value, String> {
    // Short-circuit for && and ||
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

fn resolve_property(parts: &[String], ctx: &ExprContext) -> Result<Value, String> {
    if parts.is_empty() {
        return Ok(Value::Null);
    }

    let rest = &parts[1..];

    match parts[0].as_str() {
        "github" => {
            if let Some(key) = rest.first() {
                let env_key = format!("GITHUB_{}", key.to_uppercase().replace(['.', '-'], "_"));
                if let Some(val) = ctx.env.get(&env_key) {
                    return Ok(Value::String(val.clone()));
                }
            }
            resolve_json_path(ctx.context_data, parts)
        }
        "runner" => {
            if let Some(key) = rest.first() {
                let env_key = format!("RUNNER_{}", key.to_uppercase().replace(['.', '-'], "_"));
                if let Some(val) = ctx.env.get(&env_key) {
                    return Ok(Value::String(val.clone()));
                }
            }
            Ok(Value::Null)
        }
        "env" => {
            let key = rest.first().map(|s| s.as_str()).unwrap_or("");
            Ok(ctx
                .env
                .get(key)
                .map(|v| Value::String(v.clone()))
                .unwrap_or(Value::Null))
        }
        "inputs" => {
            if let Some(key) = rest.first() {
                let env_key = format!("INPUT_{}", key.to_uppercase().replace([' ', '-'], "_"));
                Ok(Value::String(
                    ctx.env.get(&env_key).cloned().unwrap_or_default(),
                ))
            } else {
                Ok(Value::Null)
            }
        }
        "secrets" => {
            let key = rest.first().map(|s| s.as_str()).unwrap_or("");
            Ok(ctx
                .secrets
                .get(key)
                .map(|v| Value::String(v.clone()))
                .unwrap_or(Value::Null))
        }
        "steps" => {
            if rest.len() < 2 {
                return Ok(Value::Null);
            }
            let step_id = rest[0].as_str();
            let field = rest[1].as_str();

            match field {
                "outputs" if rest.len() >= 3 => Ok(ctx
                    .step_outputs
                    .get(step_id)
                    .and_then(|m| m.get(rest[2].as_str()))
                    .map(|v| Value::String(v.clone()))
                    .unwrap_or(Value::Null)),
                "outcome" => Ok(ctx
                    .step_outcomes
                    .get(step_id)
                    .map(|s| Value::String(s.outcome.clone()))
                    .unwrap_or(Value::Null)),
                "conclusion" => Ok(ctx
                    .step_outcomes
                    .get(step_id)
                    .map(|s| Value::String(s.conclusion.clone()))
                    .unwrap_or(Value::Null)),
                _ => Ok(Value::Null),
            }
        }
        "needs" | "matrix" | "strategy" | "job" => resolve_json_path(ctx.context_data, parts),
        _ => {
            debug!(context = parts[0].as_str(), "unknown expression context");
            Ok(Value::Null)
        }
    }
}

fn resolve_json_path(data: &JsonValue, parts: &[String]) -> Result<Value, String> {
    let mut current = data;
    for part in parts {
        match current {
            JsonValue::Object(map) => match map.get(part.as_str()) {
                Some(v) => current = v,
                None => return Ok(Value::Null),
            },
            JsonValue::Array(arr) => match part.parse::<usize>() {
                Ok(idx) => match arr.get(idx) {
                    Some(v) => current = v,
                    None => return Ok(Value::Null),
                },
                Err(_) => return Ok(Value::Null),
            },
            _ => return Ok(Value::Null),
        }
    }
    Ok(json_to_value(current))
}

fn json_to_value(v: &JsonValue) -> Value {
    match v {
        JsonValue::String(s) => Value::String(s.clone()),
        JsonValue::Number(n) => Value::Number(n.as_f64().unwrap_or(0.0)),
        JsonValue::Bool(b) => Value::Bool(*b),
        JsonValue::Null => Value::Null,
        other => Value::String(other.to_string()),
    }
}

// ── Built-in Functions ──────────────────────────────────────────────

fn eval_function(name: &str, args: &[Expr], ctx: &ExprContext) -> Result<Value, String> {
    match name {
        "success" => Ok(Value::Bool(!ctx.job_failed && !ctx.job_cancelled)),
        "failure" => Ok(Value::Bool(ctx.job_failed)),
        "always" => Ok(Value::Bool(true)),
        "cancelled" => Ok(Value::Bool(ctx.job_cancelled)),

        "contains" => {
            check_args(name, args, 2)?;
            let haystack = eval(&args[0], ctx)?.to_display().to_lowercase();
            let needle = eval(&args[1], ctx)?.to_display().to_lowercase();
            Ok(Value::Bool(haystack.contains(&needle)))
        }
        "startsWith" => {
            check_args(name, args, 2)?;
            let s = eval(&args[0], ctx)?.to_display().to_lowercase();
            let prefix = eval(&args[1], ctx)?.to_display().to_lowercase();
            Ok(Value::Bool(s.starts_with(&prefix)))
        }
        "endsWith" => {
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
            // Unescape doubled braces: {{ → { and }} → }
            result = result.replace("{{", "{").replace("}}", "}");
            Ok(Value::String(result))
        }
        "join" => {
            if args.is_empty() || args.len() > 2 {
                return Err("join() requires 1-2 arguments".into());
            }
            let val = eval(&args[0], ctx)?.to_display();
            let sep = if args.len() == 2 {
                eval(&args[1], ctx)?.to_display()
            } else {
                ",".into()
            };
            if let Ok(JsonValue::Array(arr)) = serde_json::from_str::<JsonValue>(&val) {
                let items: Vec<String> = arr
                    .iter()
                    .map(|v| match v {
                        JsonValue::String(s) => s.clone(),
                        other => other.to_string(),
                    })
                    .collect();
                Ok(Value::String(items.join(&sep)))
            } else {
                Ok(Value::String(val))
            }
        }
        "toJSON" => {
            check_args(name, args, 1)?;
            let val = eval(&args[0], ctx)?;
            Ok(Value::String(match &val {
                Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| s.clone()),
                other => other.to_display(),
            }))
        }
        "fromJSON" => {
            check_args(name, args, 1)?;
            let s = eval(&args[0], ctx)?.to_display();
            match serde_json::from_str::<JsonValue>(&s) {
                Ok(json) => Ok(json_to_value(&json)),
                Err(_) => Ok(Value::String(s)),
            }
        }
        "hashFiles" => {
            if args.is_empty() {
                return Err("hashFiles() requires at least 1 argument".into());
            }

            // Use the explicit workspace path (host filesystem) if available,
            // falling back to GITHUB_WORKSPACE from env. In container mode,
            // GITHUB_WORKSPACE points to a container path that doesn't exist on the host.
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
