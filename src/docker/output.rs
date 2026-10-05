use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::RwLock;

use crate::job::commands::{ALLOW_UNSECURE_COMMANDS_ENV, WorkflowCommand, parse_command};
use crate::job::execute::JobState;
use crate::job::logs::LogSender;

/// Bundles the buffers and settings needed to process stdout/stderr output lines.
///
/// Shared between `run_process()` (host mode), `docker_exec()` (container mode),
/// and docker action log streaming.
#[derive(Clone)]
pub struct OutputProcessor {
    sender: LogSender,
    masks: Arc<RwLock<Vec<String>>>,
    env_buf: Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
    path_buf: Arc<tokio::sync::Mutex<Vec<String>>>,
    output_buf: Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
    state_buf: Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
    /// Set by `::stop-commands::TOKEN`; while present, lines are logged verbatim until `::TOKEN::`.
    stop_token: Arc<std::sync::Mutex<Option<String>>>,
    /// A rejected command fails the step, like the official runner.
    command_failed: Arc<AtomicBool>,
    debug_enabled: bool,
    allow_unsecure_commands: bool,
}

impl OutputProcessor {
    pub fn new(
        sender: LogSender,
        masks: Arc<RwLock<Vec<String>>>,
        debug_enabled: bool,
        allow_unsecure_commands: bool,
    ) -> Self {
        Self {
            sender,
            masks,
            env_buf: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            path_buf: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            output_buf: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            state_buf: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            stop_token: Arc::new(std::sync::Mutex::new(None)),
            command_failed: Arc::new(AtomicBool::new(false)),
            debug_enabled,
            allow_unsecure_commands,
        }
    }

    /// Whether a workflow command was rejected, which must fail the step.
    pub fn command_failed(&self) -> bool {
        self.command_failed.load(Ordering::Relaxed)
    }

    /// Process a single output line: parse workflow commands and forward to log sender.
    pub async fn process_line(&self, line: &str) {
        if self.commands_stopped(line) {
            self.sender.send(line.to_string()).await;
            return;
        }

        let Some(cmd) = parse_command(line) else {
            self.sender.send(line.to_string()).await;
            return;
        };

        match cmd {
            WorkflowCommand::SetEnv { name, value } => {
                if self.reject_unsecure("set-env").await {
                    return;
                }
                self.env_buf.lock().await.push((name, value));
            }
            WorkflowCommand::AddPath(p) => {
                if self.reject_unsecure("add-path").await {
                    return;
                }
                self.path_buf.lock().await.push(p);
            }
            WorkflowCommand::SetOutput { name, value } => {
                self.output_buf.lock().await.push((name, value));
            }
            WorkflowCommand::AddMask(secret) => {
                self.masks.write().await.push(secret);
            }
            WorkflowCommand::Debug(msg) => {
                if self.debug_enabled {
                    self.sender.send(format!("##[debug]{msg}")).await;
                }
            }
            WorkflowCommand::Warning(msg) => {
                self.sender.send(format!("##[warning]{msg}")).await;
            }
            WorkflowCommand::Error(msg) => {
                self.sender.send(format!("##[error]{msg}")).await;
            }
            WorkflowCommand::Group(title) => {
                self.sender.send(format!("##[group]{title}")).await;
            }
            WorkflowCommand::EndGroup => {
                self.sender.send("##[endgroup]".into()).await;
            }
            WorkflowCommand::SaveState { name, value } => {
                self.state_buf.lock().await.push((name, value));
            }
            WorkflowCommand::StopCommands(token) => {
                self.stop_commands(line, token).await;
            }
        }
    }

    /// Returns true while command processing is paused, and resumes it on the `::TOKEN::` line.
    fn commands_stopped(&self, line: &str) -> bool {
        let mut stop_token = self
            .stop_token
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(token) = stop_token.as_deref() else {
            return false;
        };
        if line.trim_end_matches(['\r', '\n']) == format!("::{token}::") {
            *stop_token = None;
        }
        true
    }

    async fn stop_commands(&self, line: &str, token: String) {
        // An empty or well-known token would let any output resume command processing.
        let token_is_weak = token.is_empty() || token.eq_ignore_ascii_case("pause-logging");
        if token_is_weak && !self.allow_unsecure_commands {
            self.fail_command(format!(
                "Invalid stop-commands token. Use a unique, unguessable token, or set \
                 {ALLOW_UNSECURE_COMMANDS_ENV}=true to allow it"
            ))
            .await;
            return;
        }

        self.sender.send(line.to_string()).await;
        *self
            .stop_token
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(token);
    }

    /// Rejects `set-env` / `add-path` unless explicitly allowed. Returns true if rejected.
    async fn reject_unsecure(&self, command: &str) -> bool {
        if self.allow_unsecure_commands {
            return false;
        }
        self.fail_command(format!(
            "The `{command}` command is disabled. Please upgrade to using Environment Files \
             or opt into unsecure command execution by setting the \
             `{ALLOW_UNSECURE_COMMANDS_ENV}` environment variable to `true`."
        ))
        .await;
        true
    }

    async fn fail_command(&self, message: String) {
        self.command_failed.store(true, Ordering::Relaxed);
        self.sender.send(format!("##[error]{message}")).await;
    }

    /// Drain collected state mutations into the job state.
    pub async fn apply_to_job_state(&self, job_state: &mut JobState) {
        for (k, v) in self.env_buf.lock().await.drain(..) {
            job_state.env.insert(k, v);
        }
        job_state
            .path_prepends
            .extend(self.path_buf.lock().await.drain(..));
        for (k, v) in self.output_buf.lock().await.drain(..) {
            job_state.outputs.insert(k, v);
        }
        for (k, v) in self.state_buf.lock().await.drain(..) {
            job_state
                .action_states
                .entry(String::new())
                .or_default()
                .insert(k, v);
        }
    }
}

#[cfg(test)]
#[path = "output_test.rs"]
mod output_test;
