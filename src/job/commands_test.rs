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
