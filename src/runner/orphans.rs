//! Background processes a host-mode job leaves behind (`./server &`, `nohup ...`)
//! would otherwise keep running as the daemon's user, through every later job.
//! Like the official runner, every step inherits a per-job `RUNNER_TRACKING_ID`,
//! and whatever still carries it when the job ends is killed. Overriding the
//! variable keeps a process alive, the same escape hatch the official runner offers.

use std::path::Path;

use tracing::{info, warn};

pub const TRACKING_ID_ENV: &str = "RUNNER_TRACKING_ID";

pub fn tracking_id(job_id: &str) -> String {
    format!("chimera_{job_id}")
}

/// Kills every process still tagged with the job's tracking ID.
/// A no-op where there is no `/proc`, such as macOS.
pub async fn kill_orphans(job_id: &str) {
    let tracking_id = tracking_id(job_id);
    let pids =
        tokio::task::spawn_blocking(move || tracked_pids(Path::new("/proc"), &tracking_id)).await;
    let pids = match pids {
        Ok(pids) => pids,
        Err(e) => {
            warn!(error = %e, "scanning for orphaned job processes panicked");
            return;
        }
    };

    for &pid in &pids {
        // SAFETY: kill(2) only sends a signal and has no memory-safety requirements.
        let ret = unsafe { libc::kill(pid, libc::SIGKILL) };
        if ret != 0 {
            warn!(pid, error = %std::io::Error::last_os_error(), "failed to kill orphaned job process");
        }
    }
    if !pids.is_empty() {
        info!(count = pids.len(), "killed orphaned job processes");
    }
}

/// PIDs under `proc_dir` whose environment carries `RUNNER_TRACKING_ID=<tracking_id>`.
/// Processes whose environment can't be read (gone, or another user's) are skipped.
fn tracked_pids(proc_dir: &Path, tracking_id: &str) -> Vec<libc::pid_t> {
    let Ok(entries) = std::fs::read_dir(proc_dir) else {
        return Vec::new();
    };
    let marker = format!("{TRACKING_ID_ENV}={tracking_id}");

    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse::<libc::pid_t>().ok()?;
            let environ = std::fs::read(entry.path().join("environ")).ok()?;
            environ_contains(&environ, &marker).then_some(pid)
        })
        .filter(|&pid| pid != std::process::id() as libc::pid_t)
        .collect()
}

/// `/proc/<pid>/environ` is a NUL-separated list of `NAME=value` entries.
fn environ_contains(environ: &[u8], entry: &str) -> bool {
    environ
        .split(|&byte| byte == 0)
        .any(|candidate| candidate == entry.as_bytes())
}

#[cfg(test)]
#[path = "orphans_test.rs"]
mod orphans_test;
