use std::sync::Arc;

use axum::body::Bytes;
use axum::http::{Request, StatusCode};
use tempfile::TempDir;
use tower::ServiceExt;

use super::*;
use crate::cache::manager::CacheManager;
use crate::cache::scope::CacheGrant;

const SCOPE_REPO: &str = "owner/repo";
const SCOPE_REF: &str = "refs/heads/main";
const DEFAULT_REF: &str = "refs/heads/main";

fn scope(repo: &str, git_ref: &str) -> CacheScope {
    CacheScope {
        repo: repo.into(),
        git_ref: git_ref.into(),
        fallback_refs: vec![DEFAULT_REF.into()],
    }
}

fn main_scope() -> CacheScope {
    scope(SCOPE_REPO, SCOPE_REF)
}

fn prefix_for(grant: &CacheGrant) -> String {
    format!("/cache/{}", grant.token())
}

async fn make_test_app(tmp: &TempDir) -> (Router, SharedManager, Arc<ScopeRegistry>) {
    let entries_dir = tmp.path().join("entries");
    let data_dir = tmp.path().join("data");
    let tmp_dir = tmp.path().join("tmp");

    let manager = Arc::new(
        CacheManager::new(entries_dir, data_dir, tmp_dir, 1024 * 1024)
            .await
            .unwrap(),
    );

    let scopes = Arc::new(ScopeRegistry::default());
    (router(manager.clone(), scopes.clone()), manager, scopes)
}

/// Reserve, upload and commit `data` under `key` through the HTTP API.
async fn upload_via_http(app: &Router, prefix: &str, key: &str, data: &'static [u8]) {
    let cache_id = reserve_via_http(app, prefix, key).await;

    let req = Request::builder()
        .method("PATCH")
        .uri(format!("{prefix}/_apis/artifactcache/caches/{cache_id}"))
        .header("content-range", format!("bytes 0-{}/*", data.len() - 1))
        .body(Body::from(Bytes::from_static(data)))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = commit_via_http(app, prefix, cache_id, data.len()).await;
    assert_eq!(resp, StatusCode::NO_CONTENT);
}

async fn reserve_via_http(app: &Router, prefix: &str, key: &str) -> u64 {
    let req = Request::builder()
        .method("POST")
        .uri(format!("{prefix}/_apis/artifactcache/caches"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "key": key, "version": "v1" }).to_string(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
    let reserve_resp: serde_json::Value = serde_json::from_slice(&body).unwrap();
    reserve_resp["cacheId"].as_u64().unwrap()
}

async fn commit_via_http(app: &Router, prefix: &str, cache_id: u64, size: usize) -> StatusCode {
    let req = Request::builder()
        .method("POST")
        .uri(format!("{prefix}/_apis/artifactcache/caches/{cache_id}"))
        .header("content-type", "application/json")
        .body(Body::from(serde_json::json!({ "size": size }).to_string()))
        .unwrap();
    app.clone().oneshot(req).await.unwrap().status()
}

async fn lookup_status(app: &Router, prefix: &str, key: &str) -> StatusCode {
    let req = Request::builder()
        .uri(format!(
            "{prefix}/_apis/artifactcache/cache?keys={key}&version=v1"
        ))
        .header("host", "localhost:9999")
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn lookup_miss() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr, scopes) = make_test_app(&tmp).await;

    let grant = scopes.grant(main_scope());
    let prefix = prefix_for(&grant);
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
    let (app, _mgr, scopes) = make_test_app(&tmp).await;

    let grant = scopes.grant(main_scope());
    let prefix = prefix_for(&grant);
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
    assert!(archive_location.starts_with(&format!("http://localhost:9999{prefix}/download/")));

    // 5. Download (requires the job token too)
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
    let (app, _mgr, scopes) = make_test_app(&tmp).await;
    let grant = scopes.grant(main_scope());
    let prefix = prefix_for(&grant);

    // Non-hex characters -- rejected as bad request (prevents path traversal)
    let req = Request::builder()
        .uri(format!("{prefix}/download/nonexistent"))
        .body(Body::empty())
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Valid hex but doesn't exist -- 404
    let fake_hash = "a".repeat(64);
    let req = Request::builder()
        .uri(format!("{prefix}/download/{fake_hash}"))
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn upload_chunk_missing_content_range() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr, scopes) = make_test_app(&tmp).await;

    let grant = scopes.grant(main_scope());
    let prefix = prefix_for(&grant);
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
    let (app, _mgr, _scopes) = make_test_app(&tmp).await;

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
    let (app, _mgr, _scopes) = make_test_app(&tmp).await;

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
    let (_app, mgr, scopes) = make_test_app(&tmp).await;

    // Start a real TCP server on port 0
    let addr = start(mgr, scopes.clone(), 0).await.unwrap();
    let base_url = format!("http://{addr}");
    let grant = scopes.grant(main_scope());
    let prefix = prefix_for(&grant);
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
    let (app, _mgr, scopes) = make_test_app(&tmp).await;
    let repo_a = scopes.grant(scope("org/repo-a", SCOPE_REF));
    let repo_b = scopes.grant(scope("org/repo-b", SCOPE_REF));

    upload_via_http(&app, &prefix_for(&repo_a), "shared-key", b"scoped data").await;

    assert_eq!(
        lookup_status(&app, &prefix_for(&repo_a), "shared-key").await,
        StatusCode::OK
    );
    assert_eq!(
        lookup_status(&app, &prefix_for(&repo_b), "shared-key").await,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn unknown_token_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr, scopes) = make_test_app(&tmp).await;
    let grant = scopes.grant(main_scope());
    upload_via_http(&app, &prefix_for(&grant), "key", b"data").await;

    let status = lookup_status(&app, "/cache/guessed-token", "key").await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoked_token_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr, scopes) = make_test_app(&tmp).await;
    let grant = scopes.grant(main_scope());
    let prefix = prefix_for(&grant);
    upload_via_http(&app, &prefix, "key", b"data").await;

    drop(grant);

    assert_eq!(
        lookup_status(&app, &prefix, "key").await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn download_requires_valid_token() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr, _scopes) = make_test_app(&tmp).await;
    let hash = "a".repeat(64);

    let req = Request::builder()
        .uri(format!("/cache/guessed-token/download/{hash}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pull_request_cannot_write_default_branch_cache() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr, scopes) = make_test_app(&tmp).await;
    let main = scopes.grant(main_scope());
    let pull_request = scopes.grant(scope(SCOPE_REPO, "refs/pull/7/merge"));

    upload_via_http(&app, &prefix_for(&pull_request), "poisoned", b"evil").await;

    assert_eq!(
        lookup_status(&app, &prefix_for(&pull_request), "poisoned").await,
        StatusCode::OK
    );
    assert_eq!(
        lookup_status(&app, &prefix_for(&main), "poisoned").await,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn pull_request_can_restore_default_branch_cache() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr, scopes) = make_test_app(&tmp).await;
    let main = scopes.grant(main_scope());
    let pull_request = scopes.grant(scope(SCOPE_REPO, "refs/pull/7/merge"));

    upload_via_http(&app, &prefix_for(&main), "deps", b"cached deps").await;

    assert_eq!(
        lookup_status(&app, &prefix_for(&pull_request), "deps").await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn cannot_upload_chunk_to_session_of_another_scope() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr, scopes) = make_test_app(&tmp).await;
    let main = scopes.grant(main_scope());
    let pull_request = scopes.grant(scope(SCOPE_REPO, "refs/pull/7/merge"));
    let cache_id = reserve_via_http(&app, &prefix_for(&main), "deps").await;

    let req = Request::builder()
        .method("PATCH")
        .uri(format!(
            "{}/_apis/artifactcache/caches/{cache_id}",
            prefix_for(&pull_request)
        ))
        .header("content-range", "bytes 0-3/*")
        .body(Body::from(Bytes::from_static(b"evil")))
        .unwrap();
    let status = app.oneshot(req).await.unwrap().status();

    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn cannot_commit_upload_reserved_by_another_scope() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr, scopes) = make_test_app(&tmp).await;
    let main = scopes.grant(main_scope());
    let pull_request = scopes.grant(scope(SCOPE_REPO, "refs/pull/7/merge"));
    let cache_id = reserve_via_http(&app, &prefix_for(&main), "deps").await;

    let commit_status = commit_via_http(&app, &prefix_for(&pull_request), cache_id, 0).await;

    assert_ne!(commit_status, StatusCode::NO_CONTENT);
    assert_eq!(
        lookup_status(&app, &prefix_for(&main), "deps").await,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn cannot_download_blob_of_another_repo() {
    let tmp = TempDir::new().unwrap();
    let (app, _mgr, scopes) = make_test_app(&tmp).await;
    let repo_a = scopes.grant(scope("org/repo-a", SCOPE_REF));
    let repo_b = scopes.grant(scope("org/repo-b", SCOPE_REF));
    let data: &[u8] = b"private to repo a";
    upload_via_http(&app, &prefix_for(&repo_a), "key", data).await;
    let hash = blake3::hash(data).to_hex();

    let own = download_status(&app, &prefix_for(&repo_a), &hash).await;
    let foreign = download_status(&app, &prefix_for(&repo_b), &hash).await;

    assert_eq!(own, StatusCode::OK);
    assert_eq!(foreign, StatusCode::NOT_FOUND);
}

async fn download_status(app: &Router, prefix: &str, hash: &str) -> StatusCode {
    let req = Request::builder()
        .uri(format!("{prefix}/download/{hash}"))
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(req).await.unwrap().status()
}

#[test]
fn redact_token_hides_the_job_token_segment() {
    assert_eq!(
        redact_token("/cache/abc123/_apis/unknown"),
        "/cache/***/_apis/unknown"
    );
    assert_eq!(redact_token("/cache/abc123"), "/cache/***");
    assert_eq!(redact_token("/other/path"), "/other/path");
}
