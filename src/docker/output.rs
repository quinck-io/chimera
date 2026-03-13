use std::sync::Arc;

use tokio::sync::RwLock;

use crate::job::commands::{WorkflowCommand, parse_command};
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
    debug_enabled: bool,
}

impl OutputProcessor {
    pub fn new(sender: LogSender, masks: Arc<RwLock<Vec<String>>>, debug_enabled: bool) -> Self {
        Self {
            sender,
            masks,
            env_buf: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            path_buf: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            output_buf: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            state_buf: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            debug_enabled,
        }
    }

    /// Process a single output line: parse workflow commands and forward to log sender.
    pub async fn process_line(&self, line: &str) {
        if let Some(cmd) = parse_command(line) {
            match cmd {
                WorkflowCommand::SetEnv { name, value } => {
                    self.env_buf.lock().await.push((name, value));
                }
                WorkflowCommand::AddPath(p) => {
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
            }
        } else {
            self.sender.send(line.to_string()).await;
        }
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
