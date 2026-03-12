use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use chrono::Utc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use super::entry::{CacheEntry, EntryIndex, load_entries_from_disk};
use super::error::CacheError;
use super::store::BlobStore;
use super::upload::UploadTracker;

pub struct CacheStats {
    pub hits: AtomicU64,
    pub misses: AtomicU64,
}

impl CacheStats {
    fn new() -> Self {
        Self {
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }
}

/// Orchestrates blob store, entry index, upload tracker, and LRU eviction.
pub struct CacheManager {
    store: BlobStore,
    entries: RwLock<EntryIndex>,
    uploads: UploadTracker,
    max_bytes: u64,
    pub stats: CacheStats,
    entries_dir: PathBuf,
}

impl CacheManager {
    /// Create a new CacheManager, recovering state from disk.
    pub async fn new(
        entries_dir: PathBuf,
        data_dir: PathBuf,
        tmp_dir: PathBuf,
        max_bytes: u64,
    ) -> Result<Self> {
        // Ensure directories exist
        std::fs::create_dir_all(&entries_dir)
            .with_context(|| format!("creating entries dir {}", entries_dir.display()))?;
        std::fs::create_dir_all(&data_dir)
            .with_context(|| format!("creating data dir {}", data_dir.display()))?;
        std::fs::create_dir_all(&tmp_dir)
            .with_context(|| format!("creating tmp dir {}", tmp_dir.display()))?;

        let store = BlobStore::new(data_dir, tmp_dir.clone());
        let uploads = UploadTracker::new(tmp_dir);

        // Clean stale uploads from previous crash
        UploadTracker::cleanup_stale_files(store.tmp_dir());

        // Load entries from disk
        let disk_entries = load_entries_from_disk(&entries_dir)?;
        let mut index = EntryIndex::new();

        // Rebuild blob ref counts and verify blobs exist
        for entry in disk_entries {
            if store.exists(&entry.blob_hash) {
                store.incref(&entry.blob_hash).await;
                index.insert(entry);
            } else {
                warn!(key = %entry.key, hash = %entry.blob_hash, "removing orphaned entry (blob missing)");
                entry.remove_file(&entries_dir);
            }
        }

        // Delete orphaned blobs (blobs with no entry pointing to them)
        if let Ok(all_hashes) = store.all_hashes() {
            let referenced: std::collections::HashSet<String> = index
                .all_entries()
                .iter()
                .map(|e| e.blob_hash.clone())
                .collect();
            for hash in all_hashes {
                if !referenced.contains(&hash) {
                    debug!(hash, "removing orphaned blob");
                    // Set refcount to 1 then decref to 0, which triggers deletion
                    store.set_refcount(&hash, 1).await;
                    store.decref(&hash).await;
                }
            }
        }

        let manager = Self {
            store,
            entries: RwLock::new(index),
            uploads,
            max_bytes,
            stats: CacheStats::new(),
            entries_dir,
        };

        // Run initial eviction in case max_gb was lowered
        manager.evict().await;

        let entry_count = manager.entries.read().await.entry_count();
        let total = manager.store.total_bytes().unwrap_or(0);
        info!(
            entries = entry_count,
            total_bytes = total,
            max_bytes,
            "cache manager initialized"
        );

        Ok(manager)
    }

    /// Look up a cache entry with scope isolation.
    /// Falls back to default_ref if no match on scope_ref (feature branch reads from default branch).
    pub async fn lookup(
        &self,
        keys: &[String],
        version: &str,
        scope_repo: &str,
        scope_ref: &str,
        default_ref: &str,
    ) -> Option<CacheEntry> {
        let mut entries = self.entries.write().await;
        match entries.lookup(keys, version, scope_repo, scope_ref, default_ref) {
            Some(entry) => {
                self.stats.hits.fetch_add(1, Ordering::Relaxed);
                // Persist updated last_accessed_at
                let _ = entry.persist(&self.entries_dir);
                Some(entry)
            }
            None => {
                self.stats.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Reserve a new upload session with scope.
    pub async fn reserve_upload(
        &self,
        key: String,
        version: String,
        scope_repo: String,
        scope_ref: String,
    ) -> Result<u64> {
        self.uploads
            .reserve(key, version, scope_repo, scope_ref)
            .await
    }

    /// Write a chunk to an upload session.
    pub async fn write_chunk(&self, id: u64, offset: u64, data: &[u8]) -> Result<()> {
        self.uploads.write_chunk(id, offset, data).await
    }

    /// Commit an upload: finalize the blob and create a cache entry.
    pub async fn commit_upload(&self, id: u64, expected_size: u64) -> Result<()> {
        let (key, version, scope_repo, scope_ref, tmp_path, size) =
            self.uploads.commit(id, expected_size).await?;

        let hash = self
            .store
            .store_from_file(&tmp_path)
            .await
            .context("storing blob")?;

        // Incref before persist: if we crash after persist but before incref, the
        // on-disk entry would reference a blob with refcount 0 (orphan cleanup
        // would delete it). By incrementing first, the blob is protected.
        self.store.incref(&hash).await;

        let entry = CacheEntry {
            key: key.clone(),
            version: version.clone(),
            scope_repo: scope_repo.clone(),
            scope_ref: scope_ref.clone(),
            blob_hash: hash,
            size_bytes: size,
            created_at: Utc::now(),
            last_accessed_at: Utc::now(),
        };

        entry
            .persist(&self.entries_dir)
            .context("persisting entry")?;

        // If an entry with the same scope+key+version already exists (duplicate commit),
        // decref the old blob to avoid leaking refcounts.
        {
            let mut entries = self.entries.write().await;
            if let Some(old) = entries.remove(&scope_repo, &scope_ref, &key, &version) {
                old.remove_file(&self.entries_dir);
                self.store.decref(&old.blob_hash).await;
            }
            entries.insert(entry);
        }

        // Run eviction if we're over limit
        self.evict().await;

        Ok(())
    }

    /// Get the filesystem path for a blob.
    pub fn blob_path(&self, hash: &str) -> Result<PathBuf, CacheError> {
        self.store.blob_path(hash)
    }

    /// Evict oldest entries until total size is under max_bytes.
    /// Collects victims under the lock, then performs I/O (file delete, blob decref)
    /// after releasing it to avoid blocking concurrent lookups and uploads.
    async fn evict(&self) {
        let victims = {
            let mut entries = self.entries.write().await;
            let now = Utc::now();
            let protection_window = chrono::Duration::seconds(60);
            let mut to_evict = Vec::new();

            loop {
                let total = entries.total_size_bytes();
                if total <= self.max_bytes {
                    break;
                }

                let candidates = entries.lru_candidates();
                let victim = candidates
                    .into_iter()
                    .find(|e| now.signed_duration_since(e.last_accessed_at) > protection_window);

                let Some(victim) = victim else {
                    debug!("no evictable entries (all within protection window)");
                    break;
                };

                let repo = victim.scope_repo.clone();
                let git_ref = victim.scope_ref.clone();
                let key = victim.key.clone();
                let version = victim.version.clone();

                if let Some(removed) = entries.remove(&repo, &git_ref, &key, &version) {
                    info!(
                        key = removed.key,
                        version = removed.version,
                        scope_repo = removed.scope_repo,
                        scope_ref = removed.scope_ref,
                        size_bytes = removed.size_bytes,
                        "evicting cache entry"
                    );
                    to_evict.push(removed);
                }
            }

            to_evict
        };
        // entries lock released — perform I/O without blocking other operations
        for victim in victims {
            victim.remove_file(&self.entries_dir);
            self.store.decref(&victim.blob_hash).await;
        }
    }
}

#[cfg(test)]
#[path = "manager_test.rs"]
mod manager_test;
