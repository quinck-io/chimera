use std::collections::HashMap;

/// Opt-in env var that re-enables `set-env` / `add-path` (CVE-2020-15228), matching the official runner.
pub const ALLOW_UNSECURE_COMMANDS_ENV: &str = "ACTIONS_ALLOW_UNSECURE_COMMANDS";

/// Opt-in env var that allows guessable `stop-commands` tokens, matching the official runner.
pub const ALLOW_UNSECURE_STOP_TOKENS_ENV: &str = "ACTIONS_ALLOW_UNSECURE_STOPCOMMAND_TOKENS";

/// Every command the official runner registers. A stop token equal to one of them
/// would let ordinary command output resume processing, so it counts as weak.
const REGISTERED_COMMANDS: &[&str] = &[
    "add-mask",
    "add-matcher",
    "add-path",
    "debug",
    "echo",
    "endgroup",
    "error",
    "group",
    "notice",
    "remove-matcher",
    "save-state",
    "set-env",
    "set-output",
    "stop-commands",
    "warning",
];

#[derive(Debug, PartialEq)]
pub enum WorkflowCommand {
    SetOutput { name: String, value: String },
    SetEnv { name: String, value: String },
    AddPath(String),
    AddMask(String),
    Debug(String),
    Warning(String),
    Error(String),
    Group(String),
    EndGroup,
    SaveState { name: String, value: String },
    StopCommands(String),
}

/// Which insecure workflow command behaviours a step has opted into.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CommandPolicy {
    pub allow_unsecure_commands: bool,
    pub allow_unsecure_stop_tokens: bool,
}

impl CommandPolicy {
    /// An opt-in counts when set on the step or on chimera's own process, as with the
    /// official runner, where admins enable it for every job through the runner's environment.
    pub fn from_env(env: &HashMap<String, String>) -> Self {
        Self::from_step_and_runner_env(env, |name| std::env::var(name).ok())
    }

    fn from_step_and_runner_env(
        step_env: &HashMap<String, String>,
        runner_env: impl Fn(&str) -> Option<String>,
    ) -> Self {
        let enabled = |name: &str| {
            is_true(step_env.get(name).map(String::as_str)) || is_true(runner_env(name).as_deref())
        };
        Self {
            allow_unsecure_commands: enabled(ALLOW_UNSECURE_COMMANDS_ENV),
            allow_unsecure_stop_tokens: enabled(ALLOW_UNSECURE_STOP_TOKENS_ENV),
        }
    }
}

fn is_true(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
}

/// A token that output could guess or emit by accident: empty, `pause-logging`,
/// or the name of a workflow command.
pub fn is_weak_stop_token(token: &str) -> bool {
    token.is_empty()
        || token.eq_ignore_ascii_case("pause-logging")
        || REGISTERED_COMMANDS
            .iter()
            .any(|command| command.eq_ignore_ascii_case(token))
}

/// Whether `line` is the `::TOKEN::` command that resumes processing after `stop-commands`.
/// Like the official runner, the name is matched case-insensitively and anything
/// after the closing `::` is ignored.
pub fn resumes_commands(line: &str, token: &str) -> bool {
    split_command(line.trim_start()).is_some_and(|(name, _, _)| name.eq_ignore_ascii_case(token))
}

/// Parse a workflow command from a line of stdout.
/// Format: `::command-name param=value::message`
pub fn parse_command(line: &str) -> Option<WorkflowCommand> {
    let (cmd_name, params, message) = split_command(line)?;

    match cmd_name {
        "set-output" => {
            let name = extract_param(params?, "name")?;
            Some(WorkflowCommand::SetOutput {
                name,
                value: message.to_string(),
            })
        }
        "set-env" => {
            let name = extract_param(params?, "name")?;
            Some(WorkflowCommand::SetEnv {
                name,
                value: message.to_string(),
            })
        }
        "add-path" => Some(WorkflowCommand::AddPath(message.to_string())),
        "add-mask" => Some(WorkflowCommand::AddMask(message.to_string())),
        "debug" => Some(WorkflowCommand::Debug(message.to_string())),
        "warning" => Some(WorkflowCommand::Warning(message.to_string())),
        "error" => Some(WorkflowCommand::Error(message.to_string())),
        "group" => Some(WorkflowCommand::Group(message.to_string())),
        "endgroup" => Some(WorkflowCommand::EndGroup),
        "stop-commands" => Some(WorkflowCommand::StopCommands(message.to_string())),
        "save-state" => {
            let name = extract_param(params?, "name")?;
            Some(WorkflowCommand::SaveState {
                name,
                value: message.to_string(),
            })
        }
        _ => None,
    }
}

/// Splits `::name params::message` into its three parts.
fn split_command(line: &str) -> Option<(&str, Option<&str>, &str)> {
    let line = line.trim_end_matches(['\r', '\n']);
    let rest = line.strip_prefix("::")?;
    let (command_part, message) = rest.split_once("::")?;

    match command_part.split_once(' ') {
        Some((name, params)) => Some((name, Some(params), message)),
        None => Some((command_part, None, message)),
    }
}

fn extract_param(params: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    for part in params.split(',') {
        let part = part.trim();
        if let Some(value) = part.strip_prefix(&prefix) {
            return Some(value.to_string());
        }
    }
    None
}

#[cfg(test)]
#[path = "commands_test.rs"]
mod commands_test;
