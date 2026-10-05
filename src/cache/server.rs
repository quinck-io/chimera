use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use axum::Router;
use axum::body::Body;
use axum::extract::{
    DefaultBodyLimit, FromRef, FromRequestParts, Path, Query, RawPathParams, State,
};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tracing::{debug, info, warn};

use super::manager::CacheManager;
use super::scope::{CacheScope, ScopeRegistry};
use super::upload::parse_content_range;

pub type SharedManager = Arc<CacheManager>;

#[derive(Clone)]
pub struct CacheState {
    manager: SharedManager,
    scopes: Arc<ScopeRegistry>,
}

impl FromRef<CacheState> for SharedManager {
    fn from_ref(state: &CacheState) -> Self {
        state.manager.clone()
    }
}

/// Every cache route is prefixed by `/cache/{token}`, where the token is issued to a single
/// job by the runner. The scope comes from the token, never from the request, and requests
/// with an unknown or revoked token are rejected.
struct Authorized {
    token: String,
    scope: CacheScope,
}

impl FromRequestParts<CacheState> for Authorized {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &CacheState,
    ) -> Result<Self, Self::Rejection> {
        let params = RawPathParams::from_request_parts(parts, state)
            .await
            .map_err(|_| StatusCode::UNAUTHORIZED)?;
        let token = params
            .iter()
            .find_map(|(name, value)| (name == "token").then_some(value))
            .ok_or(StatusCode::UNAUTHORIZED)?;
        let scope = state
            .scopes
            .resolve(token)
            .ok_or(StatusCode::UNAUTHORIZED)?;
        Ok(Self {
            token: token.to_string(),
            scope,
        })
    }
}

/// Base URL handed to a job as `ACTIONS_CACHE_URL`. The toolkit appends `_apis/artifactcache/...`.
pub fn job_cache_url(host: &str, port: u16, token: &str) -> String {
    format!("http://{host}:{port}/cache/{token}/")
}

pub fn router(manager: SharedManager, scopes: Arc<ScopeRegistry>) -> Router {
    // @actions/cache uploads chunks up to 128MB (default 32MB).
    // Axum's default body limit is 2MB, which silently rejects uploads.
    const UPLOAD_BODY_LIMIT: usize = 256 * 1024 * 1024;

    Router::new()
        .route(
            "/cache/{token}/_apis/artifactcache/cache",
            get(handle_lookup),
        )
        .route(
            "/cache/{token}/_apis/artifactcache/caches",
            post(handle_reserve),
        )
        .route(
            "/cache/{token}/_apis/artifactcache/caches/{id}",
            patch(handle_upload_chunk).layer(DefaultBodyLimit::max(UPLOAD_BODY_LIMIT)),
        )
        .route(
            "/cache/{token}/_apis/artifactcache/caches/{id}",
            post(handle_commit),
        )
        .route("/cache/{token}/download/{hash}", get(handle_download))
        .fallback(handle_unknown)
        .with_state(CacheState { manager, scopes })
}

/// Start the cache server, binding to the given port.
/// Returns the actual bound address (useful when port=0 for tests).
///
/// Binds all interfaces because job containers reach it through their bridge gateway;
/// access control is enforced per request by the job token (see [`Authorized`]).
pub async fn start(
    manager: SharedManager,
    scopes: Arc<ScopeRegistry>,
    port: u16,
) -> Result<SocketAddr> {
    tokio::spawn(discard_abandoned_uploads(manager.clone(), scopes.clone()));

    let app = router(manager, scopes);
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

/// A job that fails or is cancelled mid-upload never commits, and its revoked token means
/// nobody else can, so its partial upload would otherwise stay on disk until restart.
async fn discard_abandoned_uploads(manager: SharedManager, scopes: Arc<ScopeRegistry>) {
    const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

    let mut interval = tokio::time::interval(SWEEP_INTERVAL);
    loop {
        interval.tick().await;
        let discarded = manager
            .discard_abandoned_uploads(|token| scopes.resolve(token).is_some())
            .await;
        if discarded > 0 {
            info!(discarded, "discarded abandoned cache uploads");
        }
    }
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

// --- Handlers ---

async fn handle_lookup(
    State(manager): State<SharedManager>,
    Authorized { token, scope }: Authorized,
    headers: HeaderMap,
    Query(query): Query<LookupQuery>,
) -> Response {
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
        .lookup(&keys, &query.version, &scope.repo, &scope.readable_refs())
        .await;
    match entry {
        Some(entry) => {
            let host = headers
                .get("host")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("localhost:9999");

            let location = format!("http://{host}/cache/{token}/download/{}", entry.blob_hash);

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
    Authorized { token, scope }: Authorized,
    axum::Json(body): axum::Json<ReserveBody>,
) -> Response {
    info!(
        key = %body.key,
        version = %body.version,
        scope_repo = %scope.repo,
        scope_ref = %scope.git_ref,
        "cache reserve"
    );

    match manager
        .reserve_upload(body.key, body.version, scope.repo, scope.git_ref, token)
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
    Authorized { scope, .. }: Authorized,
    Path((_token, id)): Path<(String, u64)>,
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

    match manager.write_chunk(id, &scope, start, &body).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            debug!(error = %e, id, "upload chunk failed");
            StatusCode::NOT_FOUND.into_response()
        }
    }
}

async fn handle_commit(
    State(manager): State<SharedManager>,
    Authorized { scope, .. }: Authorized,
    Path((_token, id)): Path<(String, u64)>,
    axum::Json(body): axum::Json<CommitBody>,
) -> Response {
    info!(id, size = body.size, "cache commit");

    match manager.commit_upload(id, &scope, body.size).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            debug!(error = %e, id, "commit failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn handle_download(
    State(manager): State<SharedManager>,
    Authorized { scope, .. }: Authorized,
    Path((_token, hash)): Path<(String, String)>,
) -> Response {
    // Validate hash to prevent path traversal attacks (e.g. "../../etc/passwd")
    if !super::store::is_valid_blob_hash(&hash) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    if !manager
        .can_read_blob(&hash, &scope.repo, &scope.readable_refs())
        .await
    {
        return StatusCode::NOT_FOUND.into_response();
    }

    let blob_path = match manager.blob_path(&hash) {
        Ok(p) => p,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };

    let metadata = match tokio::fs::metadata(&blob_path).await {
        Ok(m) => m,
        Err(_) => return StatusCode::NOT_FOUND.into_response(),
    };
    let content_length = metadata.len().to_string();

    match tokio::fs::File::open(&blob_path).await {
        Ok(file) => {
            let stream = tokio_util::io::ReaderStream::new(file);
            let body = Body::from_stream(stream);
            (
                StatusCode::OK,
                [
                    (axum::http::header::CONTENT_TYPE, "application/octet-stream"),
                    (axum::http::header::CONTENT_LENGTH, content_length.as_str()),
                ],
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
