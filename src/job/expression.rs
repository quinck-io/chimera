mod eval;
mod parser;
mod token;
mod value;

use std::collections::HashMap;

use anyhow::{Result, anyhow};
use serde_json::Value as JsonValue;

use super::execute::{JobState, StepConclusion, StepOutcome};
use eval::eval;
use parser::Parser;
use token::{Token, tokenize};
use value::Value;

/// All data available for evaluating GitHub Actions expressions.
pub struct ExprContext<'a> {
    pub env: &'a HashMap<String, String>,
    pub secrets: &'a HashMap<String, String>,
    pub step_outputs: &'a HashMap<String, HashMap<String, String>>,
    pub step_outcomes: &'a HashMap<String, StepOutcome>,
    pub context_data: &'a JsonValue,
    pub job_failed: bool,
    pub job_cancelled: bool,
    /// The status of the composite action a step runs in. Inside a composite,
    /// `success()` and `failure()` follow the action rather than the job, while
    /// `cancelled()` still follows the job.
    pub action_status: Option<StepConclusion>,
    /// Host filesystem workspace path for hashFiles(). In container mode,
    /// GITHUB_WORKSPACE points to the container path (/github/workspace), but
    /// hashFiles runs on the host and needs the real path.
    pub workspace_path: Option<String>,
}

impl<'a> ExprContext<'a> {
    pub fn new(
        env: &'a HashMap<String, String>,
        job_state: &'a JobState,
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
            action_status: None,
            workspace_path: job_state.host_workspace.clone(),
        }
    }

    pub fn with_action_status(mut self, status: StepConclusion) -> Self {
        self.action_status = Some(status);
        self
    }

    fn success(&self) -> bool {
        match self.action_status {
            Some(status) => status == StepConclusion::Succeeded,
            None => !self.job_failed && !self.job_cancelled,
        }
    }

    // A cancel takes over the job status, as in the official runner, so a failed
    // job that is then cancelled no longer counts as failed.
    fn failure(&self) -> bool {
        match self.action_status {
            Some(status) => status == StepConclusion::Failed,
            None => self.job_failed && !self.job_cancelled,
        }
    }
}

/// Evaluate a step `if:` condition. Returns whether the step should run.
/// `None` condition defaults to `success()` behavior. Like the official runner, a
/// condition that cannot be evaluated is an error, which fails the step.
pub fn evaluate_condition(condition: Option<&str>, ctx: &ExprContext) -> Result<bool> {
    let Some(condition) = condition.map(str::trim) else {
        return Ok(ctx.success());
    };
    Ok(evaluate(condition, ctx)?.is_truthy())
}

const STATUS_FUNCTIONS: [&str; 4] = ["success", "failure", "cancelled", "always"];

/// The `if:` of a step in an `action.yml`, read as the official runner reads it.
/// Unlike a workflow step's, it reaches the runner as written, so no condition
/// means `success()` and one that calls no status function only holds on success.
pub fn action_step_condition(condition: Option<&str>) -> String {
    let Some(condition) = condition.map(str::trim).filter(|c| !c.is_empty()) else {
        return "success()".into();
    };
    let condition = condition
        .strip_prefix("${{")
        .and_then(|c| c.strip_suffix("}}"))
        .map_or(condition, str::trim);
    if calls_status_function(condition) {
        condition.to_string()
    } else {
        format!("success() && ({condition})")
    }
}

fn calls_status_function(expr: &str) -> bool {
    let Ok(tokens) = tokenize(expr) else {
        return false;
    };
    tokens.windows(2).any(|pair| match pair {
        [Token::Ident(name), Token::LParen] => STATUS_FUNCTIONS
            .iter()
            .any(|status| name.eq_ignore_ascii_case(status)),
        _ => false,
    })
}

/// Resolve a single `${{ ... }}` expression to a string.
/// If the string is not a `${{ }}` wrapper, returns it unchanged.
pub fn resolve_expression(expr: &str, ctx: &ExprContext) -> Result<String> {
    let inner = expr
        .trim()
        .strip_prefix("${{")
        .and_then(|s| s.strip_suffix("}}"));
    match inner {
        Some(inner) => Ok(evaluate(inner.trim(), ctx)?.to_display()),
        None => Ok(expr.to_string()),
    }
}

/// Resolve all embedded `${{ ... }}` expressions in a template string. As on the
/// official runner, an expression that cannot be evaluated fails the whole template.
pub fn resolve_template(template: &str, ctx: &ExprContext) -> Result<String> {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;

    while let Some(start) = rest.find("${{") {
        result.push_str(&rest[..start]);
        let after_open = &rest[start + 3..];

        if let Some(end) = find_closing_braces(after_open) {
            let expr = after_open[..end].trim();
            result.push_str(&evaluate(expr, ctx)?.to_display());
            rest = &after_open[end + 2..];
        } else {
            // Unclosed — pass through remainder
            result.push_str(&rest[start..]);
            rest = "";
        }
    }
    result.push_str(rest);
    Ok(result)
}

/// Resolve every value of an `env:` or `with:` mapping with `resolve_template`.
pub fn resolve_template_map(
    values: &HashMap<String, String>,
    ctx: &ExprContext,
) -> Result<HashMap<String, String>> {
    values
        .iter()
        .map(|(key, value)| Ok((key.clone(), resolve_template(value, ctx)?)))
        .collect()
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

fn evaluate(expr: &str, ctx: &ExprContext) -> Result<Value> {
    parse_and_eval(expr, ctx).map_err(|e| anyhow!("failed to evaluate `${{{{ {expr} }}}}`: {e}"))
}

fn parse_and_eval(expr: &str, ctx: &ExprContext) -> std::result::Result<Value, String> {
    let tokens = tokenize(expr)?;
    let ast = Parser::new(&tokens).parse()?;
    eval(&ast, ctx)
}

#[cfg(test)]
#[path = "expression_test.rs"]
mod expression_test;
