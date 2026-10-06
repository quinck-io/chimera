use super::*;

#[test]
fn parse_set_output() {
    let cmd = parse_command("::set-output name=result::hello world").unwrap();
    assert_eq!(
        cmd,
        WorkflowCommand::SetOutput {
            name: "result".into(),
            value: "hello world".into()
        }
    );
}

#[test]
fn parse_set_env() {
    let cmd = parse_command("::set-env name=MY_VAR::some value").unwrap();
    assert_eq!(
        cmd,
        WorkflowCommand::SetEnv {
            name: "MY_VAR".into(),
            value: "some value".into()
        }
    );
}

#[test]
fn parse_add_path() {
    let cmd = parse_command("::add-path::/usr/local/bin").unwrap();
    assert_eq!(cmd, WorkflowCommand::AddPath("/usr/local/bin".into()));
}

#[test]
fn parse_add_mask() {
    let cmd = parse_command("::add-mask::supersecret").unwrap();
    assert_eq!(cmd, WorkflowCommand::AddMask("supersecret".into()));
}

#[test]
fn parse_debug() {
    let cmd = parse_command("::debug::some debug info").unwrap();
    assert_eq!(cmd, WorkflowCommand::Debug("some debug info".into()));
}

#[test]
fn parse_warning() {
    let cmd = parse_command("::warning::something fishy").unwrap();
    assert_eq!(cmd, WorkflowCommand::Warning("something fishy".into()));
}

#[test]
fn parse_error() {
    let cmd = parse_command("::error::oh no").unwrap();
    assert_eq!(cmd, WorkflowCommand::Error("oh no".into()));
}

#[test]
fn parse_group_endgroup() {
    let cmd = parse_command("::group::My Group Title").unwrap();
    assert_eq!(cmd, WorkflowCommand::Group("My Group Title".into()));

    let cmd = parse_command("::endgroup::").unwrap();
    assert_eq!(cmd, WorkflowCommand::EndGroup);
}

#[test]
fn parse_save_state() {
    let cmd = parse_command("::save-state name=key::value123").unwrap();
    assert_eq!(
        cmd,
        WorkflowCommand::SaveState {
            name: "key".into(),
            value: "value123".into()
        }
    );
}

#[test]
fn non_command_returns_none() {
    assert!(parse_command("just a normal line").is_none());
    assert!(parse_command("echo hello").is_none());
    assert!(parse_command("").is_none());
}

#[test]
fn malformed_returns_none() {
    // Missing closing ::
    assert!(parse_command("::set-output name=x").is_none());
    // Unknown command
    assert!(parse_command("::unknown-command::value").is_none());
    // set-output without name param
    assert!(parse_command("::set-output::value").is_none());
}

#[test]
fn special_characters_in_values() {
    // Value containing ::
    let cmd = parse_command("::set-output name=x::value::with::colons").unwrap();
    assert_eq!(
        cmd,
        WorkflowCommand::SetOutput {
            name: "x".into(),
            value: "value::with::colons".into()
        }
    );

    // Value containing =
    let cmd = parse_command("::set-env name=KEY::A=B=C").unwrap();
    assert_eq!(
        cmd,
        WorkflowCommand::SetEnv {
            name: "KEY".into(),
            value: "A=B=C".into()
        }
    );
}

#[test]
fn parse_stop_commands() {
    let cmd = parse_command("::stop-commands::abc123").unwrap();
    assert_eq!(cmd, WorkflowCommand::StopCommands("abc123".into()));
}

#[test]
fn command_policy_disabled_by_default() {
    let policy = CommandPolicy::from_env(&HashMap::new());

    assert_eq!(policy, CommandPolicy::default());
}

#[test]
fn command_policy_flags_enabled_only_by_true() {
    let enabled = HashMap::from([
        (ALLOW_UNSECURE_COMMANDS_ENV.to_string(), "TRUE".to_string()),
        (
            ALLOW_UNSECURE_STOP_TOKENS_ENV.to_string(),
            " true ".to_string(),
        ),
    ]);
    let other = HashMap::from([
        (ALLOW_UNSECURE_COMMANDS_ENV.to_string(), "1".to_string()),
        (
            ALLOW_UNSECURE_STOP_TOKENS_ENV.to_string(),
            "yes".to_string(),
        ),
    ]);

    let enabled_policy = CommandPolicy::from_env(&enabled);
    let other_policy = CommandPolicy::from_env(&other);

    assert!(enabled_policy.allow_unsecure_commands);
    assert!(enabled_policy.allow_unsecure_stop_tokens);
    assert_eq!(other_policy, CommandPolicy::default());
}

#[test]
fn command_policy_flags_are_independent() {
    let env = HashMap::from([(ALLOW_UNSECURE_COMMANDS_ENV.to_string(), "true".to_string())]);

    let policy = CommandPolicy::from_env(&env);

    assert!(policy.allow_unsecure_commands);
    assert!(!policy.allow_unsecure_stop_tokens);
}

#[test]
fn command_policy_reads_runner_env() {
    let runner_env = |name: &str| (name == ALLOW_UNSECURE_COMMANDS_ENV).then(|| "true".to_string());

    let policy = CommandPolicy::from_step_and_runner_env(&HashMap::new(), runner_env);

    assert!(policy.allow_unsecure_commands);
    assert!(!policy.allow_unsecure_stop_tokens);
}

#[test]
fn command_policy_step_cannot_disable_runner_opt_in() {
    let step_env = HashMap::from([(ALLOW_UNSECURE_COMMANDS_ENV.to_string(), "false".to_string())]);
    let runner_env = |name: &str| (name == ALLOW_UNSECURE_COMMANDS_ENV).then(|| "true".to_string());

    let policy = CommandPolicy::from_step_and_runner_env(&step_env, runner_env);

    assert!(policy.allow_unsecure_commands);
}

#[test]
fn weak_stop_tokens() {
    for token in [
        "",
        "pause-logging",
        "PAUSE-LOGGING",
        "set-output",
        "Add-Mask",
        "warning",
    ] {
        assert!(is_weak_stop_token(token), "{token:?} should be weak");
    }
}

#[test]
fn unique_stop_token_is_not_weak() {
    assert!(!is_weak_stop_token("f3c9a1e2-unique"));
}

#[test]
fn resume_matches_token_case_insensitively() {
    assert!(resumes_commands("::TOK::", "tok"));
}

#[test]
fn resume_allows_leading_whitespace_trailing_data_and_params() {
    assert!(resumes_commands("  ::tok::anything", "tok"));
    assert!(resumes_commands("::tok param=1::", "tok"));
    assert!(resumes_commands("::tok::\r\n", "tok"));
}

#[test]
fn resume_rejects_other_lines() {
    assert!(!resumes_commands("tok", "tok"));
    assert!(!resumes_commands("::tok", "tok"));
    assert!(!resumes_commands("::tokx::", "tok"));
    assert!(!resumes_commands("echo ::tok::", "tok"));
}

#[test]
fn add_mask_unescapes_its_value() {
    let cmd = parse_command("::add-mask::50%25 off%0Asecond line");

    assert_eq!(
        cmd,
        Some(WorkflowCommand::AddMask("50% off\nsecond line".into()))
    );
}

#[test]
fn properties_unescape_their_delimiters() {
    let cmd = parse_command("::set-output name=a%3Ab%2Cc::value");

    assert_eq!(
        cmd,
        Some(WorkflowCommand::SetOutput {
            name: "a:b,c".into(),
            value: "value".into(),
        })
    );
}
