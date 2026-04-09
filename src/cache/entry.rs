use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry {
    pub key: String,
    pub version: String,
    #[serde(default)]
    pub scope_repo: String,
    #[serde(default)]
    pub scope_ref: String,
    pub blob_hash: String,
    pub size_bytes: u64,
    pub created_at: DateTime<Utc>,
    pub last_accessed_at: DateTime<Utc>,
}

impl CacheEntry {
    /// Filename for persisting this entry: blake3(repo + "\0" + ref + "\0" + key + "\0" + version), truncated to 16 chars.
    pub fn filename(&self) -> String {
        entry_filename(&self.scope_repo, &self.scope_ref, &self.key, &self.version)
    }

    pub fn persist(&self, entries_dir: &Path) -> Result<()> {
        let path = entries_dir.join(format!("{}.json", self.filename()));
        let json = serde_json::to_string_pretty(self).context("serializing cache entry")?;
        std::fs::write(&path, json)
            .with_context(|| format!("writing cache entry {}", path.display()))?;
        Ok(())
    }

    pub fn remove_file(&self, entries_dir: &Path) {
        let path = entries_dir.join(format!("{}.json", self.filename()));
        let _ = std::fs::remove_file(path);
    }
}

fn entry_filename(repo: &str, git_ref: &str, key: &str, version: &str) -> String {
    let input = format!("{repo}\0{git_ref}\0{key}\0{version}");
    let hash = blake3::hash(input.as_bytes()).to_hex();
    hash[..16].to_string()
}

/// In-memory index for fast cache entry lookup, scoped by repository and ref.
#[derive(Default)]
pub struct EntryIndex {
    /// Keyed by (repo, scope_ref, key, version)
    exact: HashMap<(String, String, String, String), CacheEntry>,
    /// Keyed by (repo, version) -> sorted set of keys
    by_repo_version: HashMap<(String, String), BTreeSet<String>>,
}

impl EntryIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, entry: CacheEntry) {
        self.by_repo_version
            .entry((entry.scope_repo.clone(), entry.version.clone()))
            .or_default()
            .insert(entry.key.clone());
        self.exact.insert(
            (
                entry.scope_repo.clone(),
                entry.scope_ref.clone(),
                entry.key.clone(),
                entry.version.clone(),
            ),
            entry,
        );
    }

    pub fn remove(
        &mut self,
        repo: &str,
        git_ref: &str,
        key: &str,
        version: &str,
    ) -> Option<CacheEntry> {
        let entry = self.exact.remove(&(
            repo.to_string(),
            git_ref.to_string(),
            key.to_string(),
            version.to_string(),
        ))?;

        if let Some(keys) = self
            .by_repo_version
            .get_mut(&(repo.to_string(), version.to_string()))
        {
            keys.remove(key);
            if keys.is_empty() {
                self.by_repo_version
                    .remove(&(repo.to_string(), version.to_string()));
            }
        }
        Some(entry)
    }

    /// Look up a cache entry using GitHub's lookup semantics with scope isolation:
    /// 1. For each search key, try exact then longest prefix match where repo and ref match scope_ref.
    /// 2. If no match found and scope_ref != default_ref, retry with default_ref
    ///    (feature branches can read from the default branch).
    ///
    /// Returns a clone of the entry (so callers only need a read lock in the future).
    pub fn lookup(
        &mut self,
        keys: &[String],
        version: &str,
        scope_repo: &str,
        scope_ref: &str,
        default_ref: &str,
    ) -> Option<CacheEntry> {
        // Try with the current ref first
        if let Some(entry) = self.lookup_for_ref(keys, version, scope_repo, scope_ref) {
            return Some(entry);
        }

        // Fall back to default ref if different
        if scope_ref != default_ref {
            return self.lookup_for_ref(keys, version, scope_repo, default_ref);
        }

        None
    }

    /// Internal lookup for a specific repo + ref combination.
    fn lookup_for_ref(
        &mut self,
        keys: &[String],
        version: &str,
        repo: &str,
        git_ref: &str,
    ) -> Option<CacheEntry> {
        for search_key in keys {
            // Exact match
            let exact_key = (
                repo.to_string(),
                git_ref.to_string(),
                search_key.clone(),
                version.to_string(),
            );
            if let Some(entry) = self.exact.get_mut(&exact_key) {
                entry.last_accessed_at = Utc::now();
                return Some(entry.clone());
            }

            // Prefix match: walk backward from the search key to find longest prefix.
            // We use by_repo_version keyed by (repo, version) to find candidate keys,
            // then filter to entries whose ref matches.
            if let Some(version_keys) = self
                .by_repo_version
                .get(&(repo.to_string(), version.to_string()))
            {
                let mut best_match: Option<String> = None;
                for candidate in version_keys.range(..=search_key.clone()).rev() {
                    if search_key.starts_with(candidate.as_str()) {
                        // Verify this candidate exists for the correct ref
                        let check_key = (
                            repo.to_string(),
                            git_ref.to_string(),
                            candidate.clone(),
                            version.to_string(),
                        );
                        if self.exact.contains_key(&check_key) {
                            best_match = Some(candidate.clone());
                            break;
                        }
                    }
                }
                if let Some(matched_key) = best_match {
                    let key_tuple = (
                        repo.to_string(),
                        git_ref.to_string(),
                        matched_key,
                        version.to_string(),
                    );
                    if let Some(entry) = self.exact.get_mut(&key_tuple) {
                        entry.last_accessed_at = Utc::now();
                        return Some(entry.clone());
                    }
                }
            }
        }
        None
    }

    /// Get all entries sorted by last_accessed_at (oldest first) for LRU eviction.
    pub fn lru_candidates(&self) -> Vec<&CacheEntry> {
        let mut entries: Vec<&CacheEntry> = self.exact.values().collect();
        entries.sort_by_key(|e| e.last_accessed_at);
        entries
    }

    pub fn all_entries(&self) -> Vec<&CacheEntry> {
        self.exact.values().collect()
    }

    pub fn total_size_bytes(&self) -> u64 {
        self.exact.values().map(|e| e.size_bytes).sum()
    }

    pub fn entry_count(&self) -> usize {
        self.exact.len()
    }
}

/// Load all cache entries from the entries directory.
pub fn load_entries_from_disk(entries_dir: &Path) -> Result<Vec<CacheEntry>> {
    let mut entries = Vec::new();
    if !entries_dir.exists() {
        return Ok(entries);
    }
    for dir_entry in std::fs::read_dir(entries_dir)
        .with_context(|| format!("reading entries dir {}", entries_dir.display()))?
    {
        let dir_entry = dir_entry?;
        let path = dir_entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            match load_entry_file(&path) {
                Ok(entry) => entries.push(entry),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping corrupt entry file");
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
    Ok(entries)
}

fn load_entry_file(path: &PathBuf) -> Result<CacheEntry> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading entry {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("parsing entry {}", path.display()))
}

#[cfg(test)]
#[path = "entry_test.rs"]
mod entry_test;
