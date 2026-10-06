use std::path::Path;

use tempfile::TempDir;

use super::*;

fn fake_process(proc_dir: &Path, pid: &str, environ: &[&str]) {
    let dir = proc_dir.join(pid);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("environ"), environ.join("\0")).unwrap();
}

#[test]
fn finds_only_processes_tagged_with_the_job() {
    let proc_dir = TempDir::new().unwrap();
    fake_process(
        proc_dir.path(),
        "101",
        &["PATH=/bin", "RUNNER_TRACKING_ID=chimera_job-1"],
    );
    fake_process(
        proc_dir.path(),
        "102",
        &["RUNNER_TRACKING_ID=chimera_job-2"],
    );
    fake_process(proc_dir.path(), "103", &["RUNNER_TRACKING_ID="]);
    fake_process(
        proc_dir.path(),
        "self",
        &["RUNNER_TRACKING_ID=chimera_job-1"],
    );

    let pids = tracked_pids(proc_dir.path(), &tracking_id("job-1"));

    assert_eq!(pids, vec![101]);
}

#[test]
fn tag_must_match_a_whole_entry() {
    let environ = b"RUNNER_TRACKING_ID=chimera_job-10\0HOME=/root";

    assert!(!environ_contains(
        environ,
        "RUNNER_TRACKING_ID=chimera_job-1"
    ));
    assert!(environ_contains(
        environ,
        "RUNNER_TRACKING_ID=chimera_job-10"
    ));
}

#[test]
fn missing_proc_dir_yields_nothing() {
    let pids = tracked_pids(Path::new("/nonexistent/proc"), "chimera_job");

    assert!(pids.is_empty());
}
