use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;
use tracing::warn;

use super::error::CacheError;
use super::scope::CacheScope;

struct UploadSession {
    key: String,
    version: String,
    scope_repo: String,
    scope_ref: String,
    /// Token of the job that reserved the session. Once it is revoked, nobody can commit.
    token: String,
    tmp_path: PathBuf,
    bytes_written: u64,
}

impl UploadSession {
    /// Upload ids are sequential, so a session must only be usable from the scope that reserved it.
    fn is_owned_by(&self, scope: &CacheScope) -> bool {
        self.scope_repo == scope.repo && self.scope_ref == scope.git_ref
    }
}

/// Manages chunked upload sessions for the cache API.
///
/// Flow: reserve() -> write_chunk() -> commit()
pub struct UploadTracker {
    next_id: AtomicU64,
    sessions: RwLock<HashMap<u64, UploadSession>>,
    tmp_dir: PathBuf,
}

impl UploadTracker {
    pub fn new(tmp_dir: PathBuf) -> Self {
        Self {
            next_id: AtomicU64::new(1),
            sessions: RwLock::new(HashMap::new()),
            tmp_dir,
        }
    }

    /// Reserve a new upload session. Returns the cache ID.
    pub async fn reserve(
        &self,
        key: String,
        version: String,
        scope_repo: String,
        scope_ref: String,
        token: String,
    ) -> Result<u64> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let tmp_path = self.tmp_dir.join(format!("upload-{id}.tmp"));

        // Create empty file
        tokio::fs::File::create(&tmp_path)
            .await
            .with_context(|| format!("creating upload file {}", tmp_path.display()))?;

        let session = UploadSession {
            key,
            version,
            scope_repo,
            scope_ref,
            token,
            tmp_path,
            bytes_written: 0,
        };

        self.sessions.write().await.insert(id, session);
        Ok(id)
    }

    /// Write a chunk to an upload session at the given offset.
    pub async fn write_chunk(
        &self,
        id: u64,
        scope: &CacheScope,
        offset: u64,
        data: &[u8],
    ) -> Result<()> {
        // Hold write lock for the entire operation to prevent races with commit().
        // The lock scope covers both the file I/O and the bytes_written update,
        // ensuring the session can't be removed mid-write.
        let mut sessions = self.sessions.write().await;
        let session = sessions
            .get_mut(&id)
            .filter(|session| session.is_owned_by(scope))
            .ok_or(CacheError::UploadNotFound(id))?;

        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&session.tmp_path)
            .await
            .with_context(|| format!("opening upload file {}", session.tmp_path.display()))?;

        use tokio::io::AsyncSeekExt;
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        file.write_all(data).await?;
        file.flush().await?;

        let end = offset + data.len() as u64;
        if end > session.bytes_written {
            session.bytes_written = end;
        }

        Ok(())
    }

    /// Commit an upload session. Returns (key, version, scope_repo, scope_ref, tmp_path, bytes_written).
    /// The caller is responsible for storing the blob and cleaning up.
    pub async fn commit(
        &self,
        id: u64,
        scope: &CacheScope,
        expected_size: u64,
    ) -> Result<(String, String, String, String, PathBuf, u64)> {
        let session = {
            let mut sessions = self.sessions.write().await;
            if !sessions.get(&id).is_some_and(|s| s.is_owned_by(scope)) {
                return Err(CacheError::UploadNotFound(id).into());
            }
            sessions.remove(&id).ok_or(CacheError::UploadNotFound(id))?
        };

        if session.bytes_written != expected_size {
            // Clean up tmp file on mismatch
            let _ = tokio::fs::remove_file(&session.tmp_path).await;
            return Err(CacheError::SizeMismatch {
                committed: session.bytes_written,
                expected: expected_size,
            }
            .into());
        }

        Ok((
            session.key,
            session.version,
            session.scope_repo,
            session.scope_ref,
            session.tmp_path,
            session.bytes_written,
        ))
    }

    /// Drops sessions whose job token is no longer live, with their partial uploads.
    /// Returns how many were discarded.
    pub async fn discard_abandoned(&self, is_live: impl Fn(&str) -> bool) -> usize {
        let abandoned: Vec<UploadSession> = {
            let mut sessions = self.sessions.write().await;
            let ids: Vec<u64> = sessions
                .iter()
                .filter(|(_, session)| !is_live(&session.token))
                .map(|(id, _)| *id)
                .collect();
            ids.iter().filter_map(|id| sessions.remove(id)).collect()
        };

        for session in &abandoned {
            if let Err(e) = tokio::fs::remove_file(&session.tmp_path).await {
                warn!(path = %session.tmp_path.display(), error = %e, "removing abandoned upload");
            }
        }
        abandoned.len()
    }

    /// Clean up stale tmp files in the tmp directory (from previous crashes).
    pub fn cleanup_stale_files(tmp_dir: &Path) {
        if let Ok(entries) = std::fs::read_dir(tmp_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("upload-") && n.ends_with(".tmp"))
                {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

/// Parse a Content-Range header value like "bytes 0-99/*" or "bytes 0-99/200".
/// Returns (start, end) where both are inclusive.
pub fn parse_content_range(header: &str) -> Result<(u64, u64), CacheError> {
    let header = header.trim();
    let rest = header
        .strip_prefix("bytes ")
        .ok_or_else(|| CacheError::InvalidContentRange(header.to_string()))?;

    let range_part = rest
        .split('/')
        .next()
        .ok_or_else(|| CacheError::InvalidContentRange(header.to_string()))?;

    let parts: Vec<&str> = range_part.split('-').collect();
    if parts.len() != 2 {
        return Err(CacheError::InvalidContentRange(header.to_string()));
    }

    let start: u64 = parts[0]
        .parse()
        .map_err(|_| CacheError::InvalidContentRange(header.to_string()))?;
    let end: u64 = parts[1]
        .parse()
        .map_err(|_| CacheError::InvalidContentRange(header.to_string()))?;

    Ok((start, end))
}

#[cfg(test)]
#[path = "upload_test.rs"]
mod upload_test;
