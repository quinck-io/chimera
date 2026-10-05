use std::collections::{BTreeSet, HashMap};
use std::ops::Bound;
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

fn exact_key(
    repo: &str,
    git_ref: &str,
    key: &str,
    version: &str,
) -> (String, String, String, String) {
    (
        repo.to_string(),
        git_ref.to_string(),
        key.to_string(),
        version.to_string(),
    )
}

fn scope_key(repo: &str, git_ref: &str, version: &str) -> (String, String, String) {
    (repo.to_string(), git_ref.to_string(), version.to_string())
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
    /// Keyed by (repo, scope_ref, version) -> sorted set of keys, for prefix lookups
    keys_by_scope: HashMap<(String, String, String), BTreeSet<String>>,
}

impl EntryIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, entry: CacheEntry) {
        self.keys_by_scope
            .entry(scope_key(
                &entry.scope_repo,
                &entry.scope_ref,
                &entry.version,
            ))
            .or_default()
            .insert(entry.key.clone());
        self.exact.insert(
            exact_key(
                &entry.scope_repo,
                &entry.scope_ref,
                &entry.key,
                &entry.version,
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
        let entry = self.exact.remove(&exact_key(repo, git_ref, key, version))?;

        let scope = scope_key(repo, git_ref, version);
        if let Some(keys) = self.keys_by_scope.get_mut(&scope) {
            keys.remove(key);
            if keys.is_empty() {
                self.keys_by_scope.remove(&scope);
            }
        }
        Some(entry)
    }

    /// Look up a cache entry using GitHub's lookup semantics with scope isolation:
    /// for each ref in order (the job's own ref first, then the refs it may restore from),
    /// try every search key in order as an exact match, then as a prefix of stored keys.
    ///
    /// Returns a clone of the entry (so callers only need a read lock in the future).
    pub fn lookup(
        &mut self,
        keys: &[String],
        version: &str,
        scope_repo: &str,
        refs: &[&str],
    ) -> Option<CacheEntry> {
        refs.iter()
            .find_map(|git_ref| self.lookup_for_ref(keys, version, scope_repo, git_ref))
    }

    /// Internal lookup for a specific repo + ref combination.
    fn lookup_for_ref(
        &mut self,
        keys: &[String],
        version: &str,
        repo: &str,
        git_ref: &str,
    ) -> Option<CacheEntry> {
        let matched_key = keys
            .iter()
            .find_map(|search_key| self.matching_key(search_key, version, repo, git_ref))?;

        let entry = self
            .exact
            .get_mut(&exact_key(repo, git_ref, &matched_key, version))?;
        entry.last_accessed_at = Utc::now();
        Some(entry.clone())
    }

    /// An exact hit wins; otherwise, like GitHub, the most recently created entry
    /// whose key starts with `search_key`.
    fn matching_key(
        &self,
        search_key: &str,
        version: &str,
        repo: &str,
        git_ref: &str,
    ) -> Option<String> {
        // An empty restore key would otherwise match every entry in the scope.
        if search_key.is_empty() {
            return None;
        }
        if self
            .exact
            .contains_key(&exact_key(repo, git_ref, search_key, version))
        {
            return Some(search_key.to_string());
        }

        self.keys_by_scope
            .get(&scope_key(repo, git_ref, version))?
            .range::<str, _>((Bound::Included(search_key), Bound::Unbounded))
            .take_while(|key| key.starts_with(search_key))
            .filter_map(|key| self.exact.get(&exact_key(repo, git_ref, key, version)))
            .max_by_key(|entry| entry.created_at)
            .map(|entry| entry.key.clone())
    }

    /// Get all entries sorted by last_accessed_at (oldest first) for LRU eviction.
    pub fn lru_candidates(&self) -> Vec<&CacheEntry> {
        let mut entries: Vec<&CacheEntry> = self.exact.values().collect();
        entries.sort_by_key(|e| e.last_accessed_at);
        entries
    }

    /// Whether an entry readable from `refs` in `repo` stores the blob `hash`.
    pub fn references_blob(&self, hash: &str, repo: &str, refs: &[&str]) -> bool {
        self.exact.values().any(|entry| {
            entry.blob_hash == hash
                && entry.scope_repo == repo
                && refs.contains(&entry.scope_ref.as_str())
        })
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
