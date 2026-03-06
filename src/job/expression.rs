mod eval;
mod parser;
mod token;
mod value;

use std::collections::HashMap;

use serde_json::Value as JsonValue;
use tracing::debug;

use eval::eval;
use parser::Parser;
use token::tokenize;
use value::Value;

/// All data available for evaluating GitHub Actions expressions.
pub struct ExprContext<'a> {
    pub env: &'a HashMap<String, String>,
    pub secrets: &'a HashMap<String, String>,
    pub step_outputs: &'a HashMap<String, HashMap<String, String>>,
    pub context_data: &'a JsonValue,
    pub job_failed: bool,
    pub job_cancelled: bool,
}

/// Evaluate a step `if:` condition. Returns whether the step should run.
/// `None` condition defaults to `success()` behavior.
pub fn evaluate_condition(condition: Option<&str>, ctx: &ExprContext) -> bool {
    let expr_str = match condition {
        Some(c) => c.trim(),
        None => return !ctx.job_failed && !ctx.job_cancelled,
    };

    match parse_and_eval(expr_str, ctx) {
        Ok(val) => val.is_truthy(),
        Err(e) => {
            debug!(condition = expr_str, error = %e, "condition parse error, defaulting to success()");
            !ctx.job_failed && !ctx.job_cancelled
        }
    }
}

/// Resolve a single `${{ ... }}` expression to a string.
/// If the string is not a `${{ }}` wrapper, returns it unchanged.
pub fn resolve_expression(expr: &str, ctx: &ExprContext) -> String {
    let trimmed = expr.trim();

    if let Some(inner) = trimmed
        .strip_prefix("${{")
        .and_then(|s| s.strip_suffix("}}"))
    {
        match parse_and_eval(inner.trim(), ctx) {
            Ok(val) => return val.to_display(),
            Err(e) => {
                debug!(expression = inner.trim(), error = %e, "failed to resolve expression");
            }
        }
    }

    expr.to_string()
}

/// Resolve all embedded `${{ ... }}` expressions in a template string.
pub fn resolve_template(template: &str, ctx: &ExprContext) -> String {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("${{") {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + 3..];

        if let Some(end) = after_open.find("}}") {
            let expr = after_open[..end].trim();
            match parse_and_eval(expr, ctx) {
                Ok(val) => result.push_str(&val.to_display()),
                Err(e) => {
                    debug!(expression = expr, error = %e, "failed to resolve template expression");
                }
            }
            rest = &after_open[end + 2..];
        } else {
            // Unclosed — pass through remainder
            result.push_str(&rest[start..]);
            rest = "";
        }
    }
    result.push_str(rest);
    result
}

fn parse_and_eval(expr: &str, ctx: &ExprContext) -> Result<Value, String> {
    let tokens = tokenize(expr)?;
    let ast = Parser::new(&tokens).parse()?;
    eval(&ast, ctx)
}

#[cfg(test)]
#[path = "expression_test.rs"]
mod expression_test;
