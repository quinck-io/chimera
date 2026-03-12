use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tokio::sync::RwLock;
use tracing::debug;

use super::error::CacheError;

/// Content-addressed blob storage using blake3 hashes.
///
/// Blobs are stored at `data/{hash[0..2]}/{hash}` with two-char prefix
/// subdirectories to prevent directory bloat. Writes are atomic via
/// tmp-then-rename. Dedup is automatic — same content produces same hash.
pub struct BlobStore {
    data_dir: PathBuf,
    tmp_dir: PathBuf,
    ref_counts: RwLock<HashMap<String, u32>>,
}

impl BlobStore {
    pub fn new(data_dir: PathBuf, tmp_dir: PathBuf) -> Self {
        Self {
            data_dir,
            tmp_dir,
            ref_counts: RwLock::new(HashMap::new()),
        }
    }

    /// Store a blob from a file path. Returns the blake3 hex hash.
    /// If the blob already exists (dedup), the source file is removed.
    /// Uses streaming hash to avoid loading entire file into memory.
    pub async fn store_from_file(&self, source: &Path) -> Result<String> {
        let source = source.to_path_buf();
        let data_dir = self.data_dir.clone();

        tokio::task::spawn_blocking(move || {
            // Stream the file through blake3 in 64KB chunks to avoid OOM on large blobs
            let file = std::fs::File::open(&source)
                .with_context(|| format!("opening file {}", source.display()))?;
            let mut reader = std::io::BufReader::new(file);
            let mut hasher = blake3::Hasher::new();
            std::io::copy(&mut reader, &mut hasher)
                .with_context(|| format!("hashing file {}", source.display()))?;
            let hash = hasher.finalize().to_hex().to_string();

            let blob_path = blob_path(&data_dir, &hash);
            if blob_path.exists() {
                // Dedup: blob already exists, remove source
                std::fs::remove_file(&source).ok();
                return Ok(hash);
            }

            if let Some(parent) = blob_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating blob dir {}", parent.display()))?;
            }

            // rename() is atomic on POSIX. If another thread placed the same blob
            // concurrently, the rename either overwrites atomically (same content,
            // same hash — harmless) or fails. Either way the blob is intact.
            match std::fs::rename(&source, &blob_path) {
                Ok(()) => Ok(hash),
                Err(_) if blob_path.exists() => {
                    // Another thread won the race — blob is already there, clean up source
                    std::fs::remove_file(&source).ok();
                    Ok(hash)
                }
                Err(e) => Err(e).with_context(|| {
                    format!("renaming {} to {}", source.display(), blob_path.display())
                }),
            }
        })
        .await
        .context("blob store task")?
    }

    /// Get the filesystem path for a blob by hash.
    pub fn blob_path(&self, hash: &str) -> Result<PathBuf, CacheError> {
        let path = blob_path(&self.data_dir, hash);
        if path.exists() {
            Ok(path)
        } else {
            Err(CacheError::BlobNotFound(hash.to_string()))
        }
    }

    /// Increment reference count for a blob.
    pub async fn incref(&self, hash: &str) {
        let mut refs = self.ref_counts.write().await;
        *refs.entry(hash.to_string()).or_insert(0) += 1;
    }

    /// Decrement reference count. Returns true if the blob was deleted (refcount hit 0).
    pub async fn decref(&self, hash: &str) -> bool {
        let path_to_delete = {
            let mut refs = self.ref_counts.write().await;
            let Some(count) = refs.get_mut(hash) else {
                // No refcount entry — don't touch the blob
                return false;
            };
            *count = count.saturating_sub(1);
            if *count == 0 {
                refs.remove(hash);
                Some(blob_path(&self.data_dir, hash))
            } else {
                None
            }
        };
        // File deletion happens outside the lock to avoid blocking the async runtime
        if let Some(path) = path_to_delete {
            if path.exists()
                && let Err(e) = tokio::fs::remove_file(&path).await
            {
                debug!(hash, error = %e, "failed to remove blob");
                return false;
            }
            true
        } else {
            false
        }
    }

    /// Set reference count directly (used during startup recovery).
    pub async fn set_refcount(&self, hash: &str, count: u32) {
        let mut refs = self.ref_counts.write().await;
        if count > 0 {
            refs.insert(hash.to_string(), count);
        } else {
            refs.remove(hash);
        }
    }

    /// Check if a blob exists on disk.
    pub fn exists(&self, hash: &str) -> bool {
        blob_path(&self.data_dir, hash).exists()
    }

    /// Get all blob hashes present on disk.
    pub fn all_hashes(&self) -> Result<Vec<String>> {
        let mut hashes = Vec::new();
        if !self.data_dir.exists() {
            return Ok(hashes);
        }
        for prefix_entry in std::fs::read_dir(&self.data_dir)
            .with_context(|| format!("reading {}", self.data_dir.display()))?
        {
            let prefix_entry = prefix_entry?;
            if !prefix_entry.file_type()?.is_dir() {
                continue;
            }
            for blob_entry in std::fs::read_dir(prefix_entry.path())? {
                let blob_entry = blob_entry?;
                if let Some(name) = blob_entry.file_name().to_str() {
                    hashes.push(name.to_string());
                }
            }
        }
        Ok(hashes)
    }

    /// Total size of all blobs on disk.
    pub fn total_bytes(&self) -> Result<u64> {
        let mut total = 0u64;
        if !self.data_dir.exists() {
            return Ok(0);
        }
        for prefix_entry in std::fs::read_dir(&self.data_dir)? {
            let prefix_entry = prefix_entry?;
            if !prefix_entry.file_type()?.is_dir() {
                continue;
            }
            for blob_entry in std::fs::read_dir(prefix_entry.path())? {
                let blob_entry = blob_entry?;
                total += blob_entry.metadata()?.len();
            }
        }
        Ok(total)
    }

    pub fn tmp_dir(&self) -> &Path {
        &self.tmp_dir
    }
}

/// Validate that a hash string is a valid blake3 hex digest (64 lowercase hex chars).
pub fn is_valid_blob_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
}

fn blob_path(data_dir: &Path, hash: &str) -> PathBuf {
    let prefix = hash.get(..2).unwrap_or(hash);
    data_dir.join(prefix).join(hash)
}

#[cfg(test)]
#[path = "store_test.rs"]
mod store_test;
