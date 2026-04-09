use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::warn;

pub struct Workspace {
    workspace_dir: PathBuf,
    runner_temp: PathBuf,
    tool_cache: PathBuf,
    env_file: PathBuf,
    path_file: PathBuf,
    output_file: PathBuf,
    state_file: PathBuf,
    step_summary_file: PathBuf,
    event_file: PathBuf,
}

impl Workspace {
    pub fn create(
        work_dir: &Path,
        tmp_dir: &Path,
        tool_cache_dir: &Path,
        runner_name: &str,
        repo_full_name: &str,
    ) -> Result<Self> {
        // Layout: work/{runner}/{repo}/{repo}/ (doubled repo matches GitHub convention)
        let repo_short = repo_full_name
            .split('/')
            .next_back()
            .unwrap_or(repo_full_name);
        let workspace_dir = work_dir.join(runner_name).join(repo_short).join(repo_short);
        let runner_temp = tmp_dir.join(runner_name);
        let tool_cache = tool_cache_dir.to_path_buf();

        // Files live one level above workspace
        let parent = workspace_dir.parent().context("workspace has no parent")?;
        let env_file = parent.join("_env");
        let path_file = parent.join("_path");
        let output_file = parent.join("_output");
        let state_file = parent.join("_state");
        let step_summary_file = parent.join("_step_summary");
        let event_file = parent.join("_event.json");

        std::fs::create_dir_all(&workspace_dir)
            .with_context(|| format!("creating workspace dir {}", workspace_dir.display()))?;
        std::fs::create_dir_all(&runner_temp)
            .with_context(|| format!("creating runner temp dir {}", runner_temp.display()))?;
        std::fs::create_dir_all(&tool_cache)
            .with_context(|| format!("creating tool cache dir {}", tool_cache.display()))?;

        // Create empty env/path/output/state/summary files
        std::fs::write(&env_file, "")
            .with_context(|| format!("creating env file {}", env_file.display()))?;
        std::fs::write(&path_file, "")
            .with_context(|| format!("creating path file {}", path_file.display()))?;
        std::fs::write(&output_file, "")
            .with_context(|| format!("creating output file {}", output_file.display()))?;
        std::fs::write(&state_file, "")
            .with_context(|| format!("creating state file {}", state_file.display()))?;
        std::fs::write(&step_summary_file, "").with_context(|| {
            format!("creating step summary file {}", step_summary_file.display())
        })?;
        // Event file starts as empty JSON object — overwritten by write_event_file()
        std::fs::write(&event_file, "{}")
            .with_context(|| format!("creating event file {}", event_file.display()))?;

        Ok(Self {
            workspace_dir,
            runner_temp,
            tool_cache,
            env_file,
            path_file,
            output_file,
            state_file,
            step_summary_file,
            event_file,
        })
    }

    pub fn workspace_dir(&self) -> &Path {
        &self.workspace_dir
    }

    pub fn runner_temp(&self) -> &Path {
        &self.runner_temp
    }

    pub fn tool_cache(&self) -> &Path {
        &self.tool_cache
    }

    pub fn env_file(&self) -> &Path {
        &self.env_file
    }

    pub fn path_file(&self) -> &Path {
        &self.path_file
    }

    pub fn output_file(&self) -> &Path {
        &self.output_file
    }

    pub fn state_file(&self) -> &Path {
        &self.state_file
    }

    pub fn step_summary_file(&self) -> &Path {
        &self.step_summary_file
    }

    pub fn event_file(&self) -> &Path {
        &self.event_file
    }

    /// Write the GitHub event payload JSON to the event file.
    /// Actions read this via GITHUB_EVENT_PATH.
    pub fn write_event_file(&self, event: &serde_json::Value) -> Result<()> {
        let json = serde_json::to_string_pretty(event).context("serializing event payload")?;
        std::fs::write(&self.event_file, json)
            .with_context(|| format!("writing event file {}", self.event_file.display()))?;
        Ok(())
    }

    /// Read GITHUB_ENV file. Supports `KEY=VALUE` and heredoc format:
    /// ```text
    /// KEY<<EOF
    /// multiline
    /// value
    /// EOF
    /// ```
    pub fn read_env_file(&self) -> Result<HashMap<String, String>> {
        let content = std::fs::read_to_string(&self.env_file)
            .with_context(|| format!("reading env file {}", self.env_file.display()))?;
        parse_env_file(&content)
    }

    /// Read GITHUB_PATH file — one path per line.
    pub fn read_path_file(&self) -> Result<Vec<String>> {
        let content = std::fs::read_to_string(&self.path_file)
            .with_context(|| format!("reading path file {}", self.path_file.display()))?;
        Ok(content
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect())
    }

    /// Read GITHUB_OUTPUT file (same format as GITHUB_ENV).
    pub fn read_output_file(&self) -> Result<HashMap<String, String>> {
        let content = std::fs::read_to_string(&self.output_file)
            .with_context(|| format!("reading output file {}", self.output_file.display()))?;
        parse_env_file(&content)
    }

    /// Read GITHUB_STATE file (same format as GITHUB_ENV).
    /// Used by @actions/core saveState() to persist state for post steps.
    pub fn read_state_file(&self) -> Result<HashMap<String, String>> {
        let content = std::fs::read_to_string(&self.state_file)
            .with_context(|| format!("reading state file {}", self.state_file.display()))?;
        parse_env_file(&content)
    }

    /// Clear per-step files between steps so each step starts with empty files.
    pub fn clear_step_files(&self) {
        let _ = std::fs::write(&self.output_file, "");
        let _ = std::fs::write(&self.env_file, "");
        let _ = std::fs::write(&self.path_file, "");
        let _ = std::fs::write(&self.state_file, "");
        let _ = std::fs::write(&self.step_summary_file, "");
    }

    pub fn cleanup(&self) -> Result<()> {
        if self.workspace_dir.exists() {
            // Remove the runner's work directory (parent of parent of workspace)
            let runner_work = self
                .workspace_dir
                .parent()
                .and_then(|p| p.parent())
                .context("workspace has no grandparent")?;
            std::fs::remove_dir_all(runner_work)
                .with_context(|| format!("removing workspace {}", runner_work.display()))?;
        }

        if self.runner_temp.exists()
            && let Err(e) = std::fs::remove_dir_all(&self.runner_temp)
        {
            warn!(
                path = %self.runner_temp.display(),
                error = %e,
                "failed to clean up runner temp directory"
            );
        }

        Ok(())
    }
}

fn parse_env_file(content: &str) -> Result<HashMap<String, String>> {
    let mut result = HashMap::new();
    let mut lines = content.lines().peekable();

    while let Some(line) = lines.next() {
        if line.is_empty() {
            continue;
        }

        // Check for heredoc format: KEY<<DELIMITER
        if let Some(heredoc_pos) = line.find("<<") {
            let key = &line[..heredoc_pos];
            let delimiter = &line[heredoc_pos + 2..];
            let mut value_lines = Vec::new();
            for heredoc_line in lines.by_ref() {
                if heredoc_line == delimiter {
                    break;
                }
                value_lines.push(heredoc_line);
            }
            result.insert(key.to_string(), value_lines.join("\n"));
        } else if let Some(eq_pos) = line.find('=') {
            let key = &line[..eq_pos];
            let value = &line[eq_pos + 1..];
            result.insert(key.to_string(), value.to_string());
        }
    }

    Ok(result)
}

#[cfg(test)]
#[path = "workspace_test.rs"]
mod workspace_test;
