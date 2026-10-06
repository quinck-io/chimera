use std::io::Write;
use std::path::{Path, PathBuf};

use super::*;

fn make_test_tarball(files: &[(&str, &str)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());

    for (path, content) in files {
        // Add a prefix component to simulate GitHub's tarball format
        let full_path = format!("owner-repo-abc123/{path}");
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, &full_path, content.as_bytes())
            .unwrap();
    }

    let tar_data = builder.into_inner().unwrap();

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_data).unwrap();
    encoder.finish().unwrap()
}

const OLD_COMMIT: &str = "1111111111111111111111111111111111111111";
const NEW_COMMIT: &str = "2222222222222222222222222222222222222222";

fn remote(git_ref: &str) -> ActionSource {
    ActionSource::Remote {
        owner: "actions".into(),
        repo: "checkout".into(),
        git_ref: git_ref.into(),
        path: None,
    }
}

fn cache_for(server: &wiremock::MockServer, cache_dir: PathBuf) -> ActionCache {
    ActionCache::with_api_url(cache_dir, reqwest::Client::new(), server.uri())
}

fn cached_action(cache_dir: &Path, commit: &str, name: &str) -> PathBuf {
    let action_dir = cache_dir.join("actions/checkout").join(commit);
    std::fs::create_dir_all(&action_dir).unwrap();
    std::fs::write(action_dir.join("action.yml"), format!("name: {name}")).unwrap();
    action_dir
}

async fn mount_ref(server: &wiremock::MockServer, git_ref: &str, commit: &str) {
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(format!(
            "/repos/actions/checkout/commits/{git_ref}"
        )))
        .and(wiremock::matchers::header(
            "accept",
            "application/vnd.github.sha",
        ))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(commit))
        .mount(server)
        .await;
}

async fn mount_tarball(server: &wiremock::MockServer, commit: &str, name: &str) {
    let tarball = make_test_tarball(&[("action.yml", &format!("name: {name}"))]);
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(format!(
            "/repos/actions/checkout/tarball/{commit}"
        )))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_bytes(tarball))
        .mount(server)
        .await;
}

#[tokio::test]
async fn commit_sha_ref_is_served_from_cache_without_resolving() {
    let server = wiremock::MockServer::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("actions");
    let action_dir = cached_action(&cache_dir, OLD_COMMIT, "checkout");
    let cache = cache_for(&server, cache_dir);

    let result = cache
        .get_action(&remote(OLD_COMMIT), tmp.path(), "fake-token")
        .await
        .unwrap();

    assert_eq!(result, action_dir);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn moving_ref_that_still_points_to_the_cached_commit_is_a_cache_hit() {
    let server = wiremock::MockServer::start().await;
    mount_ref(&server, "v4", OLD_COMMIT).await;
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("actions");
    let action_dir = cached_action(&cache_dir, OLD_COMMIT, "checkout");
    let cache = cache_for(&server, cache_dir);

    let result = cache
        .get_action(&remote("v4"), tmp.path(), "fake-token")
        .await
        .unwrap();

    assert_eq!(result, action_dir);
}

#[tokio::test]
async fn moving_ref_that_advanced_downloads_the_new_commit() {
    let server = wiremock::MockServer::start().await;
    mount_ref(&server, "v4", NEW_COMMIT).await;
    mount_tarball(&server, NEW_COMMIT, "checkout-updated").await;
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("actions");
    cached_action(&cache_dir, OLD_COMMIT, "checkout");
    let cache = cache_for(&server, cache_dir.clone());

    let result = cache
        .get_action(&remote("v4"), tmp.path(), "fake-token")
        .await
        .unwrap();

    assert_eq!(result, cache_dir.join("actions/checkout").join(NEW_COMMIT));
    let metadata = std::fs::read_to_string(result.join("action.yml")).unwrap();
    assert_eq!(metadata, "name: checkout-updated");
}

#[tokio::test]
async fn a_ref_is_resolved_once_per_job() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(
            "/repos/actions/checkout/commits/v4",
        ))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(OLD_COMMIT))
        .expect(1)
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("actions");
    cached_action(&cache_dir, OLD_COMMIT, "checkout");
    let cache = cache_for(&server, cache_dir);

    for _ in 0..3 {
        cache
            .get_action(&remote("v4"), tmp.path(), "fake-token")
            .await
            .unwrap();
    }

    server.verify().await;
}

#[tokio::test]
async fn unresolvable_ref_fails() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(
            "/repos/actions/checkout/commits/missing",
        ))
        .respond_with(wiremock::ResponseTemplate::new(422))
        .mount(&server)
        .await;
    let tmp = tempfile::tempdir().unwrap();
    let cache = cache_for(&server, tmp.path().join("actions"));

    let result = cache
        .get_action(&remote("missing"), tmp.path(), "fake-token")
        .await;

    let error = result.unwrap_err().to_string();
    assert!(error.contains("actions/checkout@missing"), "{error}");
}

#[tokio::test]
async fn resolution_must_return_a_commit_sha() {
    let server = wiremock::MockServer::start().await;
    mount_ref(&server, "v4", "<html>not a sha</html>").await;
    let tmp = tempfile::tempdir().unwrap();
    let cache = cache_for(&server, tmp.path().join("actions"));

    let result = cache
        .get_action(&remote("v4"), tmp.path(), "fake-token")
        .await;

    assert!(result.is_err());
}

#[test]
fn commit_sha_detection() {
    assert!(is_commit_sha(OLD_COMMIT));
    assert!(is_commit_sha("ABCDEF0123456789abcdef0123456789abcdef01"));
    assert!(!is_commit_sha("v4"));
    assert!(!is_commit_sha("abc1234"));
    assert!(!is_commit_sha("g".repeat(40).as_str()));
}

#[tokio::test]
async fn tarball_extraction() {
    let mock_server = wiremock::MockServer::start().await;

    let tarball = make_test_tarball(&[
        (
            "action.yml",
            "name: test-action\nruns:\n  using: node20\n  main: index.js\n",
        ),
        ("index.js", "console.log('hello');\n"),
    ]);

    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(
            "/repos/test-owner/test-action/tarball/v1",
        ))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_bytes(tarball)
                .insert_header("content-type", "application/gzip"),
        )
        .mount(&mock_server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("actions/test-owner/test-action/v1");
    let cache = cache_for(&mock_server, tmp.path().join("actions"));

    cache
        .download_tarball("test-owner", "test-action", "v1", &dest, "fake-token")
        .await
        .unwrap();

    assert!(dest.join("action.yml").exists());
    let content = std::fs::read_to_string(dest.join("index.js")).unwrap();
    assert!(content.contains("console.log"));
}

#[tokio::test]
async fn local_path_returns_workspace_join() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let cache = ActionCache::new(tmp.path().join("actions"), reqwest::Client::new());
    let source = ActionSource::Local {
        path: PathBuf::from(".github/actions/my-action"),
    };

    let result = cache
        .get_action(&source, &workspace, "fake-token")
        .await
        .unwrap();
    assert_eq!(result, workspace.join(".github/actions/my-action"));
}

/// Build a tarball with unsafe path entries (bypassing tar crate's safety checks).
fn make_unsafe_tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar_bytes = Vec::new();
    for (path, content) in files {
        let path_bytes = path.as_bytes();
        // Build a 512-byte tar header manually
        let mut header = [0u8; 512];
        header[..path_bytes.len()].copy_from_slice(path_bytes);
        // Mode field (offset 100, 8 bytes): "0000644\0"
        header[100..108].copy_from_slice(b"0000644\0");
        // Size field (offset 124, 12 bytes): octal size
        let size_str = format!("{:011o}\0", content.len());
        header[124..136].copy_from_slice(size_str.as_bytes());
        // Type flag (offset 156): '0' = regular file
        header[156] = b'0';
        // Magic (offset 257): "ustar\0"
        header[257..263].copy_from_slice(b"ustar\0");
        // Version (offset 263): "00"
        header[263..265].copy_from_slice(b"00");
        // Compute checksum (offset 148, 8 bytes): treat checksum field as spaces
        header[148..156].copy_from_slice(b"        ");
        let cksum: u32 = header.iter().map(|&b| b as u32).sum();
        let cksum_str = format!("{:06o}\0 ", cksum);
        header[148..156].copy_from_slice(cksum_str.as_bytes());

        tar_bytes.extend_from_slice(&header);
        tar_bytes.extend_from_slice(content);
        // Pad to 512-byte boundary
        let padding = (512 - (content.len() % 512)) % 512;
        tar_bytes.extend(std::iter::repeat_n(0u8, padding));
    }
    // End-of-archive: two 512-byte blocks of zeros
    tar_bytes.extend(std::iter::repeat_n(0u8, 1024));

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&tar_bytes).unwrap();
    encoder.finish().unwrap()
}

#[test]
fn path_traversal_entries_are_skipped() {
    let tarball = make_unsafe_tarball(&[
        ("owner-repo-abc123/action.yml", b"name: legit\n"),
        ("owner-repo-abc123/../escape.txt", b"malicious\n"),
        (
            "owner-repo-abc123/sub/../../etc/passwd",
            b"also malicious\n",
        ),
    ]);

    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("extracted");
    std::fs::create_dir_all(&dest).unwrap();
    extract_tarball(&tarball, &dest).unwrap();

    // Legit file should be extracted
    assert!(dest.join("action.yml").exists());

    // Malicious entries should not escape or be created
    assert!(!tmp.path().join("escape.txt").exists());
    assert!(!tmp.path().join("etc").exists());
}

#[test]
fn extraction_keeps_the_executable_bit_and_symlinks() {
    use std::os::unix::fs::PermissionsExt;

    let mut builder = tar::Builder::new(Vec::new());
    let script = b"#!/bin/sh\necho hi\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(script.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    builder
        .append_data(&mut header, "owner-repo-abc123/entrypoint.sh", &script[..])
        .unwrap();
    let mut link = tar::Header::new_gnu();
    link.set_entry_type(tar::EntryType::Symlink);
    link.set_size(0);
    builder
        .append_link(&mut link, "owner-repo-abc123/run.sh", "entrypoint.sh")
        .unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&builder.into_inner().unwrap()).unwrap();
    let tarball = encoder.finish().unwrap();

    let tmp = tempfile::tempdir().unwrap();
    extract_tarball(&tarball, tmp.path()).unwrap();

    let mode = std::fs::metadata(tmp.path().join("entrypoint.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111);
    assert_eq!(
        std::fs::read_link(tmp.path().join("run.sh")).unwrap(),
        PathBuf::from("entrypoint.sh")
    );
}

#[test]
fn has_path_traversal_detection() {
    assert!(has_path_traversal(Path::new("../foo")));
    assert!(has_path_traversal(Path::new("foo/../../bar")));
    assert!(!has_path_traversal(Path::new("foo/bar")));
    assert!(!has_path_traversal(Path::new("foo")));
}

#[tokio::test]
async fn docker_action_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ActionCache::new(tmp.path().join("actions"), reqwest::Client::new());
    let source = ActionSource::Docker {
        image: "node:18".into(),
    };

    let result = cache.get_action(&source, tmp.path(), "fake-token").await;
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("should be handled before get_action")
    );
}
