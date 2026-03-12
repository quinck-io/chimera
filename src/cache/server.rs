use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tracing::{debug, info, warn};

use super::manager::CacheManager;
use super::upload::parse_content_range;

pub type SharedManager = Arc<CacheManager>;

/// Base64url-encode a scope string (repo or ref) for embedding in URL paths.
pub fn encode_scope(s: &str) -> String {
    URL_SAFE_NO_PAD.encode(s.as_bytes())
}

/// Decode a base64url-encoded scope string from a URL path segment.
pub fn decode_scope(s: &str) -> Result<String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(s)
        .context("invalid base64url scope")?;
    String::from_utf8(bytes).context("scope is not valid UTF-8")
}

/// Scope extracted from the URL path prefix: repo, ref, and default ref.
struct CacheScope {
    repo: String,
    git_ref: String,
    default_ref: String,
}

pub fn router(manager: SharedManager) -> Router {
    // @actions/cache uploads chunks up to 128MB (default 32MB).
    // Axum's default body limit is 2MB, which silently rejects uploads.
    const UPLOAD_BODY_LIMIT: usize = 256 * 1024 * 1024;

    Router::new()
        .route(
            "/cache/{scope_repo}/{scope_ref}/{default_ref}/_apis/artifactcache/cache",
            get(handle_lookup),
        )
        .route(
            "/cache/{scope_repo}/{scope_ref}/{default_ref}/_apis/artifactcache/caches",
            post(handle_reserve),
        )
        .route(
            "/cache/{scope_repo}/{scope_ref}/{default_ref}/_apis/artifactcache/caches/{id}",
            patch(handle_upload_chunk).layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)),
        )
        .route(
            "/cache/{scope_repo}/{scope_ref}/{default_ref}/_apis/artifactcache/caches/{id}",
            post(handle_commit),
        )
        .route("/download/{hash}", get(handle_download))
        .fallback(handle_unknown)
        .with_state(manager)
}

/// Start the cache server, binding to the given port.
/// Returns the actual bound address (useful when port=0 for tests).
pub async fn start(manager: SharedManager, port: u16) -> Result<SocketAddr> {
    let app = router(manager);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(addr).await?;
    let local_addr = listener.local_addr()?;

    info!(addr = %local_addr, "cache server listening");

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            tracing::error!(error = %e, "cache server error");
        }
    });

    Ok(local_addr)
}

// --- Query / body types ---

#[derive(Deserialize)]
struct LookupQuery {
    keys: String,
    version: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LookupResponse {
    cache_key: String,
    archive_location: String,
    scope: String,
}

#[derive(Deserialize)]
struct ReserveBody {
    key: String,
    version: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReserveResponse {
    cache_id: u64,
}

#[derive(Deserialize)]
struct CommitBody {
    size: u64,
}

// --- Scope extraction ---

fn extract_scope(
    scope_repo: &str,
    scope_ref: &str,
    default_ref: &str,
) -> Result<CacheScope, StatusCode> {
    let repo = decode_scope(scope_repo).map_err(|_| StatusCode::BAD_REQUEST)?;
    let git_ref = decode_scope(scope_ref).map_err(|_| StatusCode::BAD_REQUEST)?;
    let default = decode_scope(default_ref).map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(CacheScope {
        repo,
        git_ref,
        default_ref: default,
    })
}

// --- Handlers ---

async fn handle_lookup(
    State(manager): State<SharedManager>,
    Path((scope_repo, scope_ref, default_ref)): Path<(String, String, String)>,
    headers: HeaderMap,
    Query(query): Query<LookupQuery>,
) -> Response {
    let scope = match extract_scope(&scope_repo, &scope_ref, &default_ref) {
        Ok(s) => s,
        Err(status) => return status.into_response(),
    };

    // @actions/cache encodes commas in keys with encodeURIComponent (%2C).
    // The HTTP client may re-encode the percent sign, producing %252C on the
    // wire. Axum's Query extractor decodes one layer, leaving literal "%2C".
    // Decode that remaining layer so we can split on actual commas.
    let decoded_keys = percent_decode_commas(&query.keys);
    let keys: Vec<String> = decoded_keys
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();

    info!(
        raw_keys = %query.keys,
        keys = ?keys,
        version = %query.version,
        scope_repo = %scope.repo,
        scope_ref = %scope.git_ref,
        "cache lookup"
    );

    let entry = manager
        .lookup(
            &keys,
            &query.version,
            &scope.repo,
            &scope.git_ref,
            &scope.default_ref,
        )
        .await;
    match entry {
        Some(entry) => {
            let host = headers
                .get("host")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("localhost:9999");

            let location = format!("http://{host}/download/{}", entry.blob_hash);

            debug!(cache_key = %entry.key, location = %location, "cache hit");

            let body = LookupResponse {
                cache_key: entry.key,
                archive_location: location,
                scope: entry.scope_ref,
            };
            (StatusCode::OK, axum::Json(body)).into_response()
        }
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

async fn handle_reserve(
    State(manager): State<SharedManager>,
    Path((scope_repo, scope_ref, _default_ref)): Path<(String, String, String)>,
    axum::Json(body): axum::Json<ReserveBody>,
) -> Response {
    let scope = match extract_scope(&scope_repo, &scope_ref, &_default_ref) {
        Ok(s) => s,
        Err(status) => return status.into_response(),
    };

    info!(
        key = %body.key,
        version = %body.version,
        scope_repo = %scope.repo,
        scope_ref = %scope.git_ref,
        "cache reserve"
    );

    match manager
        .reserve_upload(body.key, body.version, scope.repo, scope.git_ref)
        .await
    {
        Ok(id) => {
            let resp = ReserveResponse { cache_id: id };
            (StatusCode::OK, axum::Json(resp)).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "reserve failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn handle_upload_chunk(
    State(manager): State<SharedManager>,
    Path((_scope_repo, _scope_ref, _default_ref, id)): Path<(String, String, String, u64)>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let content_range = match headers.get("content-range").and_then(|v| v.to_str().ok()) {
        Some(cr) => cr,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };

    let (start, _end) = match parse_content_range(content_range) {
        Ok(range) => range,
        Err(e) => {
            debug!(error = %e, "invalid Content-Range");
            return StatusCode::BAD_REQUEST.into_response();
        }
    };

    info!(
        id,
        start,
        bytes = body.len(),
        content_range,
        "cache upload chunk"
    );

    match manager.write_chunk(id, start, &body).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            debug!(error = %e, id, "upload chunk failed");
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

async fn handle_commit(
    State(manager): State<SharedManager>,
    Path((_scope_repo, _scope_ref, _default_ref, id)): Path<(String, String, String, u64)>,
    axum::Json(body): axum::Json<CommitBody>,
) -> Response {
    info!(id, size = body.size, "cache commit");

    match manager.commit_upload(id, body.size).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            debug!(error = %e, id, "commit failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn handle_download(
    State(manager): State<SharedManager>,
    Path(hash): Path<String>,
) -> Response {
    // Validate hash to prevent path traversal attacks (e.g. "../../etc/passwd")
    if !super::store::is_valid_blob_hash(&hash) {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let blob_path = match manager.blob_path(&hash) {
        Ok(p) => p,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    match tokio::fs::File::open(&blob_path).await {
        Ok(file) => {
            let stream = tokio_util::io::ReaderStream::new(file);
            let body = Body::from_stream(stream);
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
                body,
            )
                .into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn handle_unknown(uri: Uri) -> Response {
    // Both actions/cache v3 and v4 use the legacy REST API (_apis/artifactcache/*)
    // when ACTIONS_CACHE_URL is set. The Twirp protocol is only used when
    // ACTIONS_CACHE_SERVICE_V2 is set (which chimera never does). If we see Twirp
    // requests, something has gone wrong with environment variable injection.
    if uri.path().contains("twirp") || uri.path().contains("CacheService") {
        warn!(
            path = %uri.path(),
            "received Twirp cache request — this means ACTIONS_CACHE_SERVICE_V2 is set \
             unexpectedly. Chimera's cache server uses the REST API which both actions/cache \
             v3 and v4 support when ACTIONS_CACHE_URL is set"
        );
    } else {
        warn!(path = %uri.path(), "unknown cache API request");
    }
    StatusCode::NOT_FOUND.into_response()
}

/// Decode `%2C` (and `%2c`) back to `,` in a query parameter value.
/// This handles the double-encoding that `@actions/cache` produces.
fn percent_decode_commas(s: &str) -> String {
    s.replace("%2C", ",").replace("%2c", ",")
}

#[cfg(test)]
#[path = "server_test.rs"]
mod server_test;
