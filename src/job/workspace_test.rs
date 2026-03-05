use super::*;

fn make_workspace() -> (tempfile::TempDir, Workspace) {
    let tmp = tempfile::tempdir().unwrap();
    let work_dir = tmp.path().join("work");
    let tmp_dir = tmp.path().join("tmp");
    let tool_cache = tmp.path().join("tool-cache");

    let ws = Workspace::create(&work_dir, &tmp_dir, &tool_cache, "runner-0", "owner/repo").unwrap();
    (tmp, ws)
}

#[test]
fn creates_all_directories() {
    let (_tmp, ws) = make_workspace();

    assert!(ws.workspace_dir().exists());
    assert!(ws.runner_temp().exists());
    assert!(ws.tool_cache().exists());
    assert!(ws.env_file().exists());
    assert!(ws.path_file().exists());
    assert!(ws.output_file().exists());

    // Verify workspace path structure
    let ws_str = ws.workspace_dir().to_string_lossy();
    assert!(ws_str.contains("runner-0/repo/repo"));
}

#[test]
fn cleanup_removes_dirs() {
    let (_tmp, ws) = make_workspace();
    assert!(ws.workspace_dir().exists());

    ws.cleanup().unwrap();
    assert!(!ws.workspace_dir().exists());
}

#[test]
fn read_env_file_key_value() {
    let (_tmp, ws) = make_workspace();
    std::fs::write(ws.env_file(), "FOO=bar\nBAZ=qux\n").unwrap();

    let env = ws.read_env_file().unwrap();
    assert_eq!(env["FOO"], "bar");
    assert_eq!(env["BAZ"], "qux");
}

#[test]
fn read_env_file_heredoc() {
    let (_tmp, ws) = make_workspace();
    std::fs::write(ws.env_file(), "MULTI<<EOF\nline1\nline2\nEOF\nSIMPLE=val\n").unwrap();

    let env = ws.read_env_file().unwrap();
    assert_eq!(env["MULTI"], "line1\nline2");
    assert_eq!(env["SIMPLE"], "val");
}

#[test]
fn read_path_file_one_per_line() {
    let (_tmp, ws) = make_workspace();
    std::fs::write(ws.path_file(), "/usr/local/bin\n/opt/bin\n").unwrap();

    let paths = ws.read_path_file().unwrap();
    assert_eq!(paths, vec!["/usr/local/bin", "/opt/bin"]);
}

#[test]
fn empty_files_return_empty_results() {
    let (_tmp, ws) = make_workspace();

    let env = ws.read_env_file().unwrap();
    assert!(env.is_empty());

    let paths = ws.read_path_file().unwrap();
    assert!(paths.is_empty());

    let outputs = ws.read_output_file().unwrap();
    assert!(outputs.is_empty());
}
