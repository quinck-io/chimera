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
    pub step_outcomes: &'a HashMap<String, super::execute::StepOutcome>,
    pub context_data: &'a JsonValue,
    pub job_failed: bool,
    pub job_cancelled: bool,
}

impl<'a> ExprContext<'a> {
    pub fn new(
        env: &'a HashMap<String, String>,
        job_state: &'a super::execute::JobState,
        job_failed: bool,
        job_cancelled: bool,
    ) -> Self {
        Self {
            env,
            secrets: &job_state.secrets,
            step_outputs: &job_state.step_outputs,
            step_outcomes: &job_state.step_outcomes,
            context_data: &job_state.context_data,
            job_failed,
            job_cancelled,
        }
    }
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

        if let Some(end) = find_closing_braces(after_open) {
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

/// Find the closing `}}` of a `${{ ... }}` expression, skipping over `}}`
/// that appears inside single-quoted strings in the expression language.
pub fn find_closing_braces(s: &str) -> Option<usize> {
    let mut chars = s.char_indices().peekable();
    let mut in_string = false;

    while let Some((i, ch)) = chars.next() {
        match (in_string, ch) {
            // Inside a string: '' is an escaped quote, lone ' ends the string
            (true, '\'') => {
                if chars.peek().is_some_and(|&(_, c)| c == '\'') {
                    chars.next();
                } else {
                    in_string = false;
                }
            }
            (true, _) => {}
            // Outside a string: ' opens one, }} closes the expression
            (false, '\'') => in_string = true,
            (false, '}') if chars.peek().is_some_and(|&(_, c)| c == '}') => return Some(i),
            _ => {}
        }
    }
    None
}

fn parse_and_eval(expr: &str, ctx: &ExprContext) -> Result<Value, String> {
    let tokens = tokenize(expr)?;
    let ast = Parser::new(&tokens).parse()?;
    eval(&ast, ctx)
}

#[cfg(test)]
#[path = "expression_test.rs"]
mod expression_test;
