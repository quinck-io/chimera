use super::*;

fn empty_ctx() -> ExprContext<'static> {
    static EMPTY_MAP: std::sync::LazyLock<HashMap<String, String>> =
        std::sync::LazyLock::new(HashMap::new);
    static EMPTY_STEPS: std::sync::LazyLock<HashMap<String, HashMap<String, String>>> =
        std::sync::LazyLock::new(HashMap::new);
    static NULL_JSON: std::sync::LazyLock<serde_json::Value> =
        std::sync::LazyLock::new(|| serde_json::json!({}));

    ExprContext {
        env: &EMPTY_MAP,
        secrets: &EMPTY_MAP,
        step_outputs: &EMPTY_STEPS,
        context_data: &NULL_JSON,
        job_failed: false,
        job_cancelled: false,
    }
}

fn ctx_with_env(env: &HashMap<String, String>) -> ExprContext<'_> {
    static EMPTY_MAP: std::sync::LazyLock<HashMap<String, String>> =
        std::sync::LazyLock::new(HashMap::new);
    static EMPTY_STEPS: std::sync::LazyLock<HashMap<String, HashMap<String, String>>> =
        std::sync::LazyLock::new(HashMap::new);
    static NULL_JSON: std::sync::LazyLock<serde_json::Value> =
        std::sync::LazyLock::new(|| serde_json::json!({}));

    ExprContext {
        env,
        secrets: &EMPTY_MAP,
        step_outputs: &EMPTY_STEPS,
        context_data: &NULL_JSON,
        job_failed: false,
        job_cancelled: false,
    }
}

// ── evaluate_condition ──────────────────────────────────────────────

#[test]
fn condition_none_defaults_to_success() {
    let ctx = empty_ctx();
    assert!(evaluate_condition(None, &ctx));
}

#[test]
fn condition_none_fails_when_job_failed() {
    let mut ctx = empty_ctx();
    ctx.job_failed = true;
    assert!(!evaluate_condition(None, &ctx));
}

#[test]
fn condition_success() {
    let ctx = empty_ctx();
    assert!(evaluate_condition(Some("success()"), &ctx));
}

#[test]
fn condition_success_false_when_failed() {
    let mut ctx = empty_ctx();
    ctx.job_failed = true;
    assert!(!evaluate_condition(Some("success()"), &ctx));
}

#[test]
fn condition_failure() {
    let ctx = empty_ctx();
    assert!(!evaluate_condition(Some("failure()"), &ctx));
}

#[test]
fn condition_failure_true_when_failed() {
    let mut ctx = empty_ctx();
    ctx.job_failed = true;
    assert!(evaluate_condition(Some("failure()"), &ctx));
}

#[test]
fn condition_always() {
    let mut ctx = empty_ctx();
    assert!(evaluate_condition(Some("always()"), &ctx));
    ctx.job_failed = true;
    assert!(evaluate_condition(Some("always()"), &ctx));
    ctx.job_cancelled = true;
    assert!(evaluate_condition(Some("always()"), &ctx));
}

#[test]
fn condition_cancelled() {
    let ctx = empty_ctx();
    assert!(!evaluate_condition(Some("cancelled()"), &ctx));
}

#[test]
fn condition_cancelled_true_when_cancelled() {
    let mut ctx = empty_ctx();
    ctx.job_cancelled = true;
    assert!(evaluate_condition(Some("cancelled()"), &ctx));
}

#[test]
fn condition_not_cancelled() {
    let ctx = empty_ctx();
    assert!(evaluate_condition(Some("!cancelled()"), &ctx));
}

#[test]
fn condition_equality() {
    let mut env = HashMap::new();
    env.insert("GITHUB_EVENT_NAME".into(), "push".into());
    let ctx = ctx_with_env(&env);
    assert!(evaluate_condition(
        Some("github.event_name == 'push'"),
        &ctx
    ));
    assert!(!evaluate_condition(
        Some("github.event_name == 'pull_request'"),
        &ctx
    ));
}

#[test]
fn condition_and_or() {
    let mut env = HashMap::new();
    env.insert("GITHUB_REF".into(), "refs/heads/main".into());
    env.insert("GITHUB_EVENT_NAME".into(), "push".into());
    let ctx = ctx_with_env(&env);

    assert!(evaluate_condition(
        Some("github.ref == 'refs/heads/main' && github.event_name == 'push'"),
        &ctx,
    ));
    assert!(evaluate_condition(
        Some("github.ref == 'refs/heads/dev' || github.event_name == 'push'"),
        &ctx,
    ));
    assert!(!evaluate_condition(
        Some("github.ref == 'refs/heads/dev' && github.event_name == 'push'"),
        &ctx,
    ));
}

#[test]
fn condition_contains() {
    let mut env = HashMap::new();
    env.insert("GITHUB_REF".into(), "refs/tags/v1.0.0".into());
    let ctx = ctx_with_env(&env);

    assert!(evaluate_condition(
        Some("contains(github.ref, 'tags')"),
        &ctx
    ));
    assert!(!evaluate_condition(
        Some("contains(github.ref, 'heads')"),
        &ctx
    ));
}

#[test]
fn condition_starts_with() {
    let mut env = HashMap::new();
    env.insert("GITHUB_REF".into(), "refs/heads/main".into());
    let ctx = ctx_with_env(&env);

    assert!(evaluate_condition(
        Some("startsWith(github.ref, 'refs/heads/')"),
        &ctx,
    ));
}

#[test]
fn condition_ends_with() {
    let mut env = HashMap::new();
    env.insert("GITHUB_REF".into(), "refs/heads/release-v2".into());
    let ctx = ctx_with_env(&env);

    assert!(evaluate_condition(
        Some("endsWith(github.ref, '-v2')"),
        &ctx,
    ));
}

#[test]
fn condition_parse_error_defaults_to_success() {
    let ctx = empty_ctx();
    // Invalid syntax falls back to success() behavior
    assert!(evaluate_condition(Some("??? invalid"), &ctx));
}

// ── resolve_expression ──────────────────────────────────────────────

#[test]
fn resolve_github_repository() {
    let mut env = HashMap::new();
    env.insert("GITHUB_REPOSITORY".into(), "owner/repo".into());
    let ctx = ctx_with_env(&env);

    assert_eq!(
        resolve_expression("${{ github.repository }}", &ctx),
        "owner/repo"
    );
}

#[test]
fn resolve_input_with_default() {
    let mut env = HashMap::new();
    env.insert("INPUT_TOOLCHAIN".into(), "stable".into());
    let ctx = ctx_with_env(&env);

    assert_eq!(
        resolve_expression("${{ inputs.toolchain }}", &ctx),
        "stable"
    );
}

#[test]
fn resolve_plain_string_unchanged() {
    let ctx = empty_ctx();
    assert_eq!(resolve_expression("hello", &ctx), "hello");
}

#[test]
fn resolve_unsupported_context_unchanged() {
    let ctx = empty_ctx();
    // Whole-string expression that fails to parse — returned as-is
    assert_eq!(resolve_expression("${{ ??? }}", &ctx), "${{ ??? }}");
}

// ── resolve_template ────────────────────────────────────────────────

#[test]
fn template_simple_input() {
    let mut env = HashMap::new();
    env.insert("INPUT_TOOLCHAIN".into(), "stable".into());
    let ctx = ctx_with_env(&env);

    assert_eq!(
        resolve_template("rustup default ${{ inputs.toolchain }}", &ctx),
        "rustup default stable"
    );
}

#[test]
fn template_multiple_expressions() {
    let mut env = HashMap::new();
    env.insert("GITHUB_REPOSITORY".into(), "owner/repo".into());
    env.insert("GITHUB_SHA".into(), "abc123".into());
    let ctx = ctx_with_env(&env);

    assert_eq!(
        resolve_template("echo ${{ github.repository }} at ${{ github.sha }}", &ctx),
        "echo owner/repo at abc123"
    );
}

#[test]
fn template_env_context() {
    let mut env = HashMap::new();
    env.insert("FOO".into(), "bar".into());
    let ctx = ctx_with_env(&env);

    assert_eq!(resolve_template("echo ${{ env.FOO }}", &ctx), "echo bar");
}

#[test]
fn template_string_literal() {
    let ctx = empty_ctx();
    assert_eq!(resolve_template("x=${{ 'hello' }}", &ctx), "x=hello");
}

#[test]
fn template_complex_expression_resolved() {
    let mut env = HashMap::new();
    env.insert("INPUT_TOOLCHAIN".into(), "nightly".into());
    env.insert("INPUT_COMPONENTS".into(), "clippy".into());
    let ctx = ctx_with_env(&env);

    let result = resolve_template(
        "downgrade=${{contains(inputs.toolchain, 'nightly') && inputs.components && ' --allow-downgrade' || ''}}",
        &ctx,
    );
    assert_eq!(result, "downgrade= --allow-downgrade");
}

#[test]
fn template_complex_expression_false_branch() {
    let mut env = HashMap::new();
    env.insert("INPUT_TOOLCHAIN".into(), "stable".into());
    env.insert("INPUT_COMPONENTS".into(), "".into());
    let ctx = ctx_with_env(&env);

    let result = resolve_template(
        "downgrade=${{contains(inputs.toolchain, 'nightly') && ' --allow-downgrade' || ''}}",
        &ctx,
    );
    assert_eq!(result, "downgrade=");
}

#[test]
fn template_no_expressions_unchanged() {
    let ctx = empty_ctx();
    assert_eq!(
        resolve_template("echo hello world", &ctx),
        "echo hello world"
    );
}

#[test]
fn template_unclosed_expression() {
    let ctx = empty_ctx();
    assert_eq!(
        resolve_template("echo ${{ oops no close", &ctx),
        "echo ${{ oops no close"
    );
}

// ── secrets ─────────────────────────────────────────────────────────

#[test]
fn secrets_lookup() {
    let env = HashMap::new();
    let mut secrets = HashMap::new();
    secrets.insert("MY_TOKEN".into(), "s3cret".into());

    let empty_steps = HashMap::new();
    let null_json = serde_json::json!({});

    let ctx = ExprContext {
        env: &env,
        secrets: &secrets,
        step_outputs: &empty_steps,
        context_data: &null_json,
        job_failed: false,
        job_cancelled: false,
    };

    assert_eq!(
        resolve_expression("${{ secrets.MY_TOKEN }}", &ctx),
        "s3cret"
    );
}

#[test]
fn secrets_missing_is_null() {
    let ctx = empty_ctx();
    // Missing secret resolves to empty (Null → "")
    assert_eq!(resolve_expression("${{ secrets.NOPE }}", &ctx), "");
}

// ── steps ───────────────────────────────────────────────────────────

#[test]
fn steps_output_lookup() {
    let env = HashMap::new();
    let secrets = HashMap::new();
    let mut step_outputs = HashMap::new();
    step_outputs.insert(
        "build".into(),
        HashMap::from([("version".into(), "1.2.3".into())]),
    );
    let null_json = serde_json::json!({});

    let ctx = ExprContext {
        env: &env,
        secrets: &secrets,
        step_outputs: &step_outputs,
        context_data: &null_json,
        job_failed: false,
        job_cancelled: false,
    };

    assert_eq!(
        resolve_expression("${{ steps.build.outputs.version }}", &ctx),
        "1.2.3"
    );
}

#[test]
fn steps_output_missing() {
    let ctx = empty_ctx();
    assert_eq!(
        resolve_expression("${{ steps.build.outputs.version }}", &ctx),
        ""
    );
}

// ── needs ───────────────────────────────────────────────────────────

#[test]
fn needs_output_lookup() {
    let env = HashMap::new();
    let secrets = HashMap::new();
    let empty_steps = HashMap::new();
    let context_data = serde_json::json!({
        "needs": {
            "setup": {
                "outputs": { "version": "3.0" },
                "result": "success"
            }
        }
    });

    let ctx = ExprContext {
        env: &env,
        secrets: &secrets,
        step_outputs: &empty_steps,
        context_data: &context_data,
        job_failed: false,
        job_cancelled: false,
    };

    assert_eq!(
        resolve_expression("${{ needs.setup.outputs.version }}", &ctx),
        "3.0"
    );
    assert_eq!(
        resolve_expression("${{ needs.setup.result }}", &ctx),
        "success"
    );
}

#[test]
fn needs_condition_check() {
    let env = HashMap::new();
    let secrets = HashMap::new();
    let empty_steps = HashMap::new();
    let context_data = serde_json::json!({
        "needs": {
            "build": { "result": "success" }
        }
    });

    let ctx = ExprContext {
        env: &env,
        secrets: &secrets,
        step_outputs: &empty_steps,
        context_data: &context_data,
        job_failed: false,
        job_cancelled: false,
    };

    assert!(evaluate_condition(
        Some("needs.build.result == 'success'"),
        &ctx,
    ));
}

// ── functions ───────────────────────────────────────────────────────

#[test]
fn format_function() {
    let ctx = empty_ctx();
    let result = parse_and_eval("format('Hello {0}, you are {1}!', 'World', 'great')", &ctx)
        .unwrap()
        .to_display();
    assert_eq!(result, "Hello World, you are great!");
}

#[test]
fn contains_case_insensitive() {
    let ctx = empty_ctx();
    assert!(
        parse_and_eval("contains('Hello World', 'hello')", &ctx)
            .unwrap()
            .is_truthy()
    );
}

// ── operators ───────────────────────────────────────────────────────

#[test]
fn number_comparison() {
    let ctx = empty_ctx();
    assert!(parse_and_eval("42 > 10", &ctx).unwrap().is_truthy());
    assert!(parse_and_eval("10 <= 10", &ctx).unwrap().is_truthy());
    assert!(!parse_and_eval("10 > 10", &ctx).unwrap().is_truthy());
}

#[test]
fn inequality() {
    let ctx = empty_ctx();
    assert!(parse_and_eval("'a' != 'b'", &ctx).unwrap().is_truthy());
    assert!(!parse_and_eval("'a' != 'A'", &ctx).unwrap().is_truthy());
}

#[test]
fn boolean_logic() {
    let ctx = empty_ctx();
    assert!(parse_and_eval("true && true", &ctx).unwrap().is_truthy());
    assert!(!parse_and_eval("true && false", &ctx).unwrap().is_truthy());
    assert!(parse_and_eval("false || true", &ctx).unwrap().is_truthy());
    assert!(!parse_and_eval("!true", &ctx).unwrap().is_truthy());
}

#[test]
fn null_is_falsy() {
    let ctx = empty_ctx();
    assert!(!parse_and_eval("null", &ctx).unwrap().is_truthy());
}

#[test]
fn empty_string_is_falsy() {
    let ctx = empty_ctx();
    assert!(!parse_and_eval("''", &ctx).unwrap().is_truthy());
}

#[test]
fn parentheses() {
    let ctx = empty_ctx();
    assert!(
        parse_and_eval("(true || false) && true", &ctx)
            .unwrap()
            .is_truthy()
    );
}
