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

#[tokio::test]
async fn docker_action_returns_error() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = ActionCache::new(tmp.path().join("actions"), reqwest::Client::new());
    let source = ActionSource::Docker {
        image: "node:18".into(),
    };

    let result = cache.get_action(&source, tmp.path(), "fake-token").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Phase 3"));
}
