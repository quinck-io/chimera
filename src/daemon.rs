use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{RwLock, watch};
use tokio::task::JoinSet;
use tracing::{Instrument, error, info, warn};

use crate::cache::manager::CacheManager;
use crate::cache::server as cache_server;
use crate::config::{ChimeraConfig, ChimeraPaths, load_runner_credentials};
use crate::runner::Runner;

// --- PID Lock ---

#[derive(Debug)]
pub struct PidLock {
    path: PathBuf,
}

impl PidLock {
    pub fn acquire(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("reading PID file {}", path.display()))?;
            let pid: u32 = content
                .trim()
                .parse()
                .with_context(|| format!("parsing PID from {}", path.display()))?;

            if is_process_alive(pid) {
                bail!("chimera daemon already running (pid {pid}). Use 'chimera status' to check.");
            }

            std::fs::remove_file(path)
                .with_context(|| format!("removing stale PID file {}", path.display()))?;
        }

        std::fs::write(path, std::process::id().to_string())
            .with_context(|| format!("writing PID file {}", path.display()))?;

        Ok(Self {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for PidLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// --- Process liveness check ---

pub fn is_process_alive(pid: u32) -> bool {
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        return true;
    }
    // EPERM means the process exists but we lack permission to signal it
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

// --- Runner state ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunnerPhase {
    Starting,
    Idle,
    Running,
    Stopping,
    Stopped,
}

impl std::fmt::Display for RunnerPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Starting => write!(f, "Starting"),
            Self::Idle => write!(f, "Idle"),
            Self::Running => write!(f, "Running"),
            Self::Stopping => write!(f, "Stopping"),
            Self::Stopped => write!(f, "Stopped"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobInfo {
    pub repo: String,
    pub job_id: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerStatus {
    pub phase: RunnerPhase,
    pub current_job: Option<JobInfo>,
    pub last_error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub phase_changed_at: DateTime<Utc>,
}

pub struct DaemonState {
    runners: RwLock<HashMap<String, RunnerStatus>>,
    pid: u32,
    started_at: DateTime<Utc>,
}

impl DaemonState {
    pub fn new(runner_names: &[String]) -> Self {
        let now = Utc::now();
        let mut runners = HashMap::new();
        for name in runner_names {
            runners.insert(
                name.clone(),
                RunnerStatus {
                    phase: RunnerPhase::Starting,
                    current_job: None,
                    last_error: None,
                    started_at: now,
                    phase_changed_at: now,
                },
            );
        }
        Self {
            runners: RwLock::new(runners),
            pid: std::process::id(),
            started_at: now,
        }
    }

    pub async fn set_phase(&self, name: &str, phase: RunnerPhase) {
        let mut runners = self.runners.write().await;
        if let Some(status) = runners.get_mut(name) {
            status.phase = phase;
            status.phase_changed_at = Utc::now();
            if !matches!(status.phase, RunnerPhase::Running) {
                status.current_job = None;
            }
        }
    }

    pub async fn set_running(&self, name: &str, job: JobInfo) {
        let mut runners = self.runners.write().await;
        if let Some(status) = runners.get_mut(name) {
            status.phase = RunnerPhase::Running;
            status.current_job = Some(job);
            status.phase_changed_at = Utc::now();
        }
    }

    pub async fn set_error(&self, name: &str, error: String) {
        let mut runners = self.runners.write().await;
        if let Some(status) = runners.get_mut(name) {
            status.phase = RunnerPhase::Stopped;
            status.last_error = Some(error);
            status.current_job = None;
            status.phase_changed_at = Utc::now();
        }
    }

    pub async fn snapshot(&self) -> StateSnapshot {
        let runners = self.runners.read().await;
        StateSnapshot {
            pid: self.pid,
            started_at: self.started_at,
            runners: runners.clone(),
        }
    }
}

// --- State file ---

#[derive(Debug, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub pid: u32,
    pub started_at: DateTime<Utc>,
    pub runners: HashMap<String, RunnerStatus>,
}

pub fn write_state_file(path: &Path, snapshot: &StateSnapshot) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(snapshot).context("serializing state")?;
    std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))?;
    Ok(())
}

pub fn read_state_file(path: &Path) -> Result<StateSnapshot> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading state file {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing state file {}", path.display()))
}

// --- Daemon ---

pub struct Daemon {
    paths: ChimeraPaths,
    config: ChimeraConfig,
}

impl Daemon {
    pub fn new(paths: ChimeraPaths, config: ChimeraConfig) -> Self {
        Self { paths, config }
    }

    pub async fn run(self, mut shutdown_rx: watch::Receiver<bool>) -> Result<()> {
        let _pid_lock = PidLock::acquire(&self.paths.pid_file()).context("acquiring PID lock")?;

        // Start cache server if configured
        let cache_config = self.config.cache.clone().unwrap_or_default();
        let cache_manager = Arc::new(
            CacheManager::new(
                self.paths.cache_entries_dir(),
                self.paths.cache_data_dir(),
                self.paths.cache_tmp_dir(),
                cache_config.max_gb * 1024 * 1024 * 1024,
            )
            .await
            .context("initializing cache manager")?,
        );

        let cache_addr = cache_server::start(cache_manager, cache_config.cache_port)
            .await
            .context("starting cache server")?;
        let cache_port = cache_addr.port();

        let state = Arc::new(DaemonState::new(&self.config.runners));

        let mut join_set = JoinSet::new();
        let mut started = 0usize;

        for name in &self.config.runners {
            let creds = match load_runner_credentials(&self.paths.runners_dir(), name) {
                Ok(c) => c,
                Err(e) => {
                    error!(runner = %name, error = %e, "failed to load credentials, skipping");
                    state.set_error(name, format!("{e:#}")).await;
                    continue;
                }
            };

            let runner = Runner::with_state(
                name.clone(),
                creds,
                self.paths.clone(),
                Arc::clone(&state),
                cache_port,
            );

            let rx = shutdown_rx.clone();
            let runner_name = name.clone();
            let state_ref = Arc::clone(&state);

            join_set.spawn(
                async move {
                    let result = runner.start(rx).await;
                    if let Err(ref e) = result {
                        state_ref.set_error(&runner_name, format!("{e:#}")).await;
                    }
                    (runner_name, result)
                }
                .instrument(tracing::info_span!("runner", name = %name)),
            );

            started += 1;
        }

        if started == 0 {
            bail!("no runners could be started");
        }

        // Spawn state file writer
        let state_writer = Arc::clone(&state);
        let state_path = self.paths.state_file();
        let mut writer_shutdown_rx = shutdown_rx.clone();
        let writer_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {
                        let snapshot = state_writer.snapshot().await;
                        if let Err(e) = write_state_file(&state_path, &snapshot) {
                            warn!(error = %e, "failed to write state file");
                        }
                    }
                    _ = writer_shutdown_rx.changed() => break,
                }
            }
        });

        // Write initial state immediately
        let snapshot = state.snapshot().await;
        if let Err(e) = write_state_file(&self.paths.state_file(), &snapshot) {
            warn!(error = %e, "failed to write initial state file");
        }

        let shutdown_timeout = self
            .config
            .daemon
            .as_ref()
            .map(|d| d.shutdown_timeout_secs)
            .unwrap_or(300);

        info!(runners = started, "daemon started");

        // Wait for either: all runners exit or shutdown signal
        loop {
            tokio::select! {
                result = join_set.join_next() => {
                    match result {
                        Some(Ok((name, Ok(())))) => {
                            info!(runner = %name, "runner exited cleanly");
                        }
                        Some(Ok((name, Err(e)))) => {
                            error!(runner = %name, error = %e, "runner exited with error");
                        }
                        Some(Err(e)) => {
                            error!(error = %e, "runner task panicked");
                        }
                        None => {
                            info!("all runners exited");
                            break;
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    info!("shutdown signal received, waiting for runners to finish");
                    break;
                }
            }
        }

        // Drain remaining runners with timeout
        if !join_set.is_empty() {
            let timeout = Duration::from_secs(shutdown_timeout);
            let drain = async {
                while let Some(result) = join_set.join_next().await {
                    match result {
                        Ok((name, Ok(()))) => info!(runner = %name, "runner exited cleanly"),
                        Ok((name, Err(e))) => {
                            error!(runner = %name, error = %e, "runner exited with error")
                        }
                        Err(e) => error!(error = %e, "runner task panicked"),
                    }
                }
            };
            if tokio::time::timeout(timeout, drain).await.is_err() {
                warn!(
                    timeout_secs = shutdown_timeout,
                    "shutdown timeout exceeded, forcing exit"
                );
                join_set.abort_all();
            }
        }

        // Stop state writer
        writer_handle.abort();
        let _ = writer_handle.await;

        // Clean up state file
        let _ = std::fs::remove_file(self.paths.state_file());

        info!("daemon shut down");
        Ok(())
    }
}

// --- Status display ---

pub fn format_status_display(snapshot: &StateSnapshot) -> String {
    let mut out = String::new();

    let uptime = Utc::now() - snapshot.started_at;
    out.push_str(&format!(
        "Daemon: running (pid {}, uptime {})\n",
        snapshot.pid,
        format_duration(uptime),
    ));
    out.push('\n');
    out.push_str("Runners:\n");

    let mut names: Vec<&String> = snapshot.runners.keys().collect();
    names.sort();

    for name in names {
        let status = &snapshot.runners[name];
        out.push_str(&format!("  {name}: {}\n", format_runner_line(status)));
    }

    out
}

pub fn format_runner_line(status: &RunnerStatus) -> String {
    match &status.phase {
        RunnerPhase::Running => {
            if let Some(ref job) = status.current_job {
                let elapsed = Utc::now() - job.started_at;
                format!("Running job {} ({})", job.repo, format_duration(elapsed))
            } else {
                "Running".to_string()
            }
        }
        RunnerPhase::Idle => {
            let idle_dur = Utc::now() - status.phase_changed_at;
            format!("Idle ({})", format_duration(idle_dur))
        }
        RunnerPhase::Stopped => {
            if let Some(ref err) = status.last_error {
                format!("Stopped — error: {err}")
            } else {
                "Stopped".to_string()
            }
        }
        phase => phase.to_string(),
    }
}

pub fn format_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

#[cfg(test)]
#[path = "daemon_test.rs"]
mod daemon_test;
