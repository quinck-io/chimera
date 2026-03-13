use std::io::Write;
use std::path::PathBuf;

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

#[tokio::test]
async fn cache_hit_skips_download() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("actions");
    let action_dir = cache_dir.join("actions/checkout/v4");
    std::fs::create_dir_all(&action_dir).unwrap();
    std::fs::write(action_dir.join("action.yml"), "name: checkout").unwrap();

    let cache = ActionCache::new(cache_dir, reqwest::Client::new());
    let source = ActionSource::Remote {
        owner: "actions".into(),
        repo: "checkout".into(),
        git_ref: "v4".into(),
        path: None,
    };

    let result = cache
        .get_action(&source, tmp.path(), "fake-token")
        .await
        .unwrap();
    assert_eq!(result, action_dir);
    assert!(result.join("action.yml").exists());
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
    let cache_dir = tmp.path().join("actions");

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .unwrap();

    let cache = ActionCache {
        cache_dir: cache_dir.clone(),
        client,
    };

    // Override the download URL by manually calling download_tarball
    let dest = cache_dir.join("test-owner/test-action/v1");
    let url = format!(
        "{}/repos/test-owner/test-action/tarball/v1",
        mock_server.uri()
    );

    let response = cache
        .client
        .get(&url)
        .header("Authorization", "token fake-token")
        .header("User-Agent", "chimera")
        .send()
        .await
        .unwrap();

    let bytes = response.bytes().await.unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    extract_tarball(&bytes, &dest).unwrap();

    assert!(dest.join("action.yml").exists());
    assert!(dest.join("index.js").exists());

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
