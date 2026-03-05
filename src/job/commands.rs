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
}

/// Parse a workflow command from a line of stdout.
/// Format: `::command-name param=value::message`
pub fn parse_command(line: &str) -> Option<WorkflowCommand> {
    let line = line.trim_end_matches(['\r', '\n']);

    if !line.starts_with("::") {
        return None;
    }

    // Find the closing `::`
    let rest = &line[2..];
    let closing = rest.find("::")?;
    let command_part = &rest[..closing];
    let message = &rest[closing + 2..];

    // Split command_part into command name and parameters
    let (cmd_name, params) = match command_part.find(' ') {
        Some(pos) => (&command_part[..pos], Some(&command_part[pos + 1..])),
        None => (command_part, None),
    };

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
