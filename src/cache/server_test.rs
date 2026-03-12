use std::sync::Arc;

use axum::body::Bytes;
use axum::http::{Request, StatusCode};
use tempfile::TempDir;
use tower::ServiceExt;

use super::*;
use crate::cache::manager::CacheManager;

const SCOPE_REPO: &str = "owner/repo";
const SCOPE_REF: &str = "refs/heads/main";
const DEFAULT_REF: &str = "refs/heads/main";

fn scope_prefix() -> String {
    let repo = encode_scope(SCOPE_REPO);
    let git_ref = encode_scope(SCOPE_REF);
    let default = encode_scope(DEFAULT_REF);
    format!("/cache/{repo}/{git_ref}/{default}")
}

async fn make_test_app(tmp: &TempDir) -> (Router, SharedManager) {
    let entries_dir = tmp.path().join("entries");
    let data_dir = tmp.path().join("data");
    let tmp_dir = tmp.path().join("tmp");

    let manager = Arc::new(
        CacheManager::new(entries_dir, data_dir, tmp_dir, 1024 * 1024)
            .await
            .unwrap(),
    );

    (router(manager.clone()), manager)
}

#[tokio::test]
async fn lookup_miss() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr) = make_test_app(&tmp).await;

    let prefix = scope_prefix();
    let req = Request::builder()
        .uri(format!(
            "{prefix}/_apis/artifactcache/cache?keys=nonexistent&version=v1"
        ))
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn full_http_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr) = make_test_app(&tmp).await;

    let prefix = scope_prefix();
    let data = b"test cache data for http roundtrip";

    // 1. Reserve
    let req = Request::builder()
        .method("POST")
        .uri(format!("{prefix}/_apis/artifactcache/caches"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "key": "http-key",
                "version": "v1"
            }))
            .unwrap(),
        ))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let reserve_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let cache_id = reserve_resp["cacheId"].as_u64().unwrap();

    // 2. Upload chunk
    let req = Request::builder()
        .method("PATCH")
        .uri(format!("{prefix}/_apis/artifactcache/caches/{cache_id}"))
        .header("content-range", format!("bytes 0-{}/*", data.len() - 1))
        .body(Body::from(Bytes::from_static(data)))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // 3. Commit
    let req = Request::builder()
        .method("POST")
        .uri(format!("{prefix}/_apis/artifactcache/caches/{cache_id}"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "size": data.len()
            }))
            .unwrap(),
        ))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // 4. Lookup
    let req = Request::builder()
        .uri(format!(
            "{prefix}/_apis/artifactcache/cache?keys=http-key&version=v1"
        ))
        .header("host", "localhost:9999")
        .body(Body::empty())
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let lookup_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(lookup_resp["cacheKey"], "http-key");
    assert_eq!(lookup_resp["scope"], SCOPE_REF);

    let archive_location = lookup_resp["archiveLocation"].as_str().unwrap();
    assert!(archive_location.starts_with("http://localhost:9999/download/"));

    // 5. Download (global, no scope prefix)
    let download_path = archive_location
        .strip_prefix("http://localhost:9999")
        .unwrap();
    let req = Request::builder()
        .uri(download_path)
        .body(Body::empty())
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    assert_eq!(&body[..], data);
}

#[tokio::test]
async fn download_invalid_hash_rejected() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr) = make_test_app(&tmp).await;

    // Non-hex characters -- rejected as bad request (prevents path traversal)
    let req = Request::builder()
        .uri("/download/nonexistent")
        .body(Body::empty())
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Valid hex but doesn't exist -- 404
    let fake_hash = "a".repeat(64);
    let req = Request::builder()
        .uri(format!("/download/{fake_hash}"))
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn upload_chunk_missing_content_range() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr) = make_test_app(&tmp).await;

    let prefix = scope_prefix();
    let req = Request::builder()
        .method("PATCH")
        .uri(format!("{prefix}/_apis/artifactcache/caches/1"))
        .body(Body::from(Bytes::from_static(b"data")))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn v4_twirp_request_returns_404() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr) = make_test_app(&tmp).await;

    let req = Request::builder()
        .method("POST")
        .uri("/twirp/github.actions.results.api.v1.CacheService/CreateCacheEntry")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unknown_path_returns_404() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr) = make_test_app(&tmp).await;

    let req = Request::builder()
        .uri("/some/random/path")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn concurrent_http_clients() {
    let tmp = TempDir::new().unwrap();
    let (_app, mgr) = make_test_app(&tmp).await;

    // Start a real TCP server on port 0
    let addr = start(mgr, 0).await.unwrap();
    let base_url = format!("http://{addr}");
    let prefix = scope_prefix();
    let client = reqwest::Client::new();

    let mut handles = Vec::new();
    for i in 0..5 {
        let c = client.clone();
        let url = base_url.clone();
        let pfx = prefix.clone();
        handles.push(tokio::spawn(async move {
            let key = format!("concurrent-http-{i}");
            let data = format!("concurrent data {i}");

            // Reserve
            let resp = c
                .post(format!("{url}{pfx}/_apis/artifactcache/caches"))
                .json(&serde_json::json!({ "key": key, "version": "v1" }))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200);
            let reserve_resp: serde_json::Value = resp.json().await.unwrap();
            let cache_id = reserve_resp["cacheId"].as_u64().unwrap();

            // Upload
            let resp = c
                .patch(format!("{url}{pfx}/_apis/artifactcache/caches/{cache_id}"))
                .header("content-range", format!("bytes 0-{}/*", data.len() - 1))
                .body(data.clone())
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 204);

            // Commit
            let resp = c
                .post(format!("{url}{pfx}/_apis/artifactcache/caches/{cache_id}"))
                .json(&serde_json::json!({ "size": data.len() }))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 204);

            // Lookup
            let resp = c
                .get(format!(
                    "{url}{pfx}/_apis/artifactcache/cache?keys={key}&version=v1"
                ))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200);
        }));
    }

    for h in handles {
        h.await.unwrap();
    }
}

#[tokio::test]
async fn scope_isolation_between_repos() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr) = make_test_app(&tmp).await;

    let data = b"scoped data";
    let repo_a_prefix = format!(
        "/cache/{}/{}/{}",
        encode_scope("org/repo-a"),
        encode_scope(SCOPE_REF),
        encode_scope(DEFAULT_REF),
    );
    let repo_b_prefix = format!(
        "/cache/{}/{}/{}",
        encode_scope("org/repo-b"),
        encode_scope(SCOPE_REF),
        encode_scope(DEFAULT_REF),
    );

    // Upload cache under repo-a
    let req = Request::builder()
        .method("POST")
        .uri(format!("{repo_a_prefix}/_apis/artifactcache/caches"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({
                "key": "shared-key",
                "version": "v1"
            }))
            .unwrap(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let reserve_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let cache_id = reserve_resp["cacheId"].as_u64().unwrap();

    let req = Request::builder()
        .method("PATCH")
        .uri(format!(
            "{repo_a_prefix}/_apis/artifactcache/caches/{cache_id}"
        ))
        .header("content-range", format!("bytes 0-{}/*", data.len() - 1))
        .body(Body::from(Bytes::from_static(data)))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "{repo_a_prefix}/_apis/artifactcache/caches/{cache_id}"
        ))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_string(&serde_json::json!({ "size": data.len() })).unwrap(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Lookup from repo-a should succeed
    let req = Request::builder()
        .uri(format!(
            "{repo_a_prefix}/_apis/artifactcache/cache?keys=shared-key&version=v1"
        ))
        .header("host", "localhost:9999")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Lookup from repo-b should miss
    let req = Request::builder()
        .uri(format!(
            "{repo_b_prefix}/_apis/artifactcache/cache?keys=shared-key&version=v1"
        ))
        .header("host", "localhost:9999")
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[test]
fn scope_encode_decode_roundtrip() {
    let values = [
        "owner/repo",
        "refs/heads/main",
        "refs/heads/feature/my-branch",
        "refs/tags/v1.0.0",
        "",
    ];
    for val in values {
        let encoded = encode_scope(val);
        let decoded = decode_scope(&encoded).unwrap();
        assert_eq!(decoded, val);
    }
}

#[test]
fn decode_scope_invalid_base64() {
    let result = decode_scope("!!!invalid!!!");
    assert!(result.is_err());
}
