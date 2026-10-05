use chrono::Utc;
use tempfile::TempDir;

use super::*;

fn make_entry(key: &str, version: &str, hash: &str) -> CacheEntry {
    CacheEntry {
        key: key.to_string(),
        version: version.to_string(),
        scope_repo: "owner/repo".to_string(),
        scope_ref: "refs/heads/main".to_string(),
        blob_hash: hash.to_string(),
        size_bytes: 100,
        created_at: Utc::now(),
        last_accessed_at: Utc::now(),
    }
}

fn make_scoped_entry(
    key: &str,
    version: &str,
    hash: &str,
    repo: &str,
    git_ref: &str,
) -> CacheEntry {
    CacheEntry {
        key: key.to_string(),
        version: version.to_string(),
        scope_repo: repo.to_string(),
        scope_ref: git_ref.to_string(),
        blob_hash: hash.to_string(),
        size_bytes: 100,
        created_at: Utc::now(),
        last_accessed_at: Utc::now(),
    }
}

const REPO: &str = "owner/repo";
const MAIN_REF: &str = "refs/heads/main";
const FEATURE_REF: &str = "refs/heads/feature";

#[test]
fn exact_lookup() {
    let mut index = EntryIndex::new();
    index.insert(make_entry("rust-cargo-abc123", "v1", "hash1"));

    let result = index.lookup(&["rust-cargo-abc123".to_string()], "v1", REPO, &[MAIN_REF]);
    assert!(result.is_some());
    assert_eq!(result.unwrap().blob_hash, "hash1");
}

#[test]
fn prefix_lookup_matches_keys_starting_with_search_key() {
    let mut index = EntryIndex::new();
    index.insert(make_entry("rust-cargo-abc123", "v1", "hash1"));

    let result = index.lookup(&["rust-cargo-".to_string()], "v1", REPO, &[MAIN_REF]);

    assert_eq!(result.unwrap().blob_hash, "hash1");
}

#[test]
fn prefix_lookup_ignores_keys_shorter_than_search_key() {
    let mut index = EntryIndex::new();
    index.insert(make_entry("rust-", "v1", "short"));
    index.insert(make_entry("rust-cargo-", "v1", "long"));

    let result = index.lookup(&["rust-cargo-abc123".to_string()], "v1", REPO, &[MAIN_REF]);

    assert!(result.is_none());
}

#[test]
fn prefix_lookup_returns_most_recently_created_match() {
    let mut index = EntryIndex::new();
    let mut older = make_entry("rust-cargo-zzz", "v1", "older");
    older.created_at = Utc::now() - chrono::Duration::hours(1);
    index.insert(older);
    index.insert(make_entry("rust-cargo-aaa", "v1", "newer"));

    let result = index.lookup(&["rust-cargo-".to_string()], "v1", REPO, &[MAIN_REF]);

    assert_eq!(result.unwrap().blob_hash, "newer");
}

#[test]
fn exact_match_wins_over_newer_prefix_match() {
    let mut index = EntryIndex::new();
    let mut exact = make_entry("rust-cargo", "v1", "exact");
    exact.created_at = Utc::now() - chrono::Duration::hours(1);
    index.insert(exact);
    index.insert(make_entry("rust-cargo-abc", "v1", "prefix"));

    let result = index.lookup(&["rust-cargo".to_string()], "v1", REPO, &[MAIN_REF]);

    assert_eq!(result.unwrap().blob_hash, "exact");
}

#[test]
fn version_scoping() {
    let mut index = EntryIndex::new();
    index.insert(make_entry("key1", "v1", "hash_v1"));
    index.insert(make_entry("key1", "v2", "hash_v2"));

    let result = index.lookup(&["key1".to_string()], "v1", REPO, &[MAIN_REF]);
    assert_eq!(result.unwrap().blob_hash, "hash_v1");

    let result = index.lookup(&["key1".to_string()], "v2", REPO, &[MAIN_REF]);
    assert_eq!(result.unwrap().blob_hash, "hash_v2");
}

#[test]
fn miss_returns_none() {
    let mut index = EntryIndex::new();
    let result = index.lookup(&["nonexistent".to_string()], "v1", REPO, &[MAIN_REF]);
    assert!(result.is_none());
}

#[test]
fn miss_wrong_version() {
    let mut index = EntryIndex::new();
    index.insert(make_entry("key1", "v1", "hash1"));

    let result = index.lookup(&["key1".to_string()], "v2", REPO, &[MAIN_REF]);
    assert!(result.is_none());
}

#[test]
fn restore_key_matches_entry_saved_under_a_different_primary_key() {
    let mut index = EntryIndex::new();
    index.insert(make_entry("rust-cargo-old", "v1", "old_hash"));

    let result = index.lookup(
        &["rust-cargo-abc123".to_string(), "rust-cargo-".to_string()],
        "v1",
        REPO,
        &[MAIN_REF],
    );

    assert_eq!(result.unwrap().blob_hash, "old_hash");
}

#[test]
fn restore_keys_are_tried_in_order() {
    let mut index = EntryIndex::new();
    index.insert(make_entry("linux-old", "v1", "linux_hash"));
    index.insert(make_entry("any-old", "v1", "any_hash"));

    let result = index.lookup(
        &[
            "linux-abc123".to_string(),
            "linux-".to_string(),
            "any-".to_string(),
        ],
        "v1",
        REPO,
        &[MAIN_REF],
    );

    assert_eq!(result.unwrap().blob_hash, "linux_hash");
}

#[test]
fn empty_restore_key_matches_nothing() {
    let mut index = EntryIndex::new();
    index.insert(make_entry("rust-cargo-old", "v1", "old_hash"));

    let result = index.lookup(&["".to_string()], "v1", REPO, &[MAIN_REF]);

    assert!(result.is_none());
}

#[test]
fn prefix_lookup_skips_keys_sharing_only_leading_characters() {
    let mut index = EntryIndex::new();
    index.insert(make_entry("rust-build-xyz", "v1", "wrong_prefix"));
    index.insert(make_entry("rust-cargo-xyz", "v1", "right_prefix"));
    index.insert(make_entry("rust-dist-xyz", "v1", "wrong_prefix"));

    let result = index.lookup(&["rust-cargo-".to_string()], "v1", REPO, &[MAIN_REF]);

    assert_eq!(result.unwrap().blob_hash, "right_prefix");
}

#[test]
fn lru_candidates_ordering() {
    let mut index = EntryIndex::new();

    let mut e1 = make_entry("key1", "v1", "h1");
    e1.last_accessed_at = Utc::now() - chrono::Duration::hours(2);

    let mut e2 = make_entry("key2", "v1", "h2");
    e2.last_accessed_at = Utc::now() - chrono::Duration::hours(1);

    let e3 = make_entry("key3", "v1", "h3");

    index.insert(e3);
    index.insert(e1);
    index.insert(e2);

    let candidates = index.lru_candidates();
    assert_eq!(candidates[0].key, "key1");
    assert_eq!(candidates[1].key, "key2");
    assert_eq!(candidates[2].key, "key3");
}

#[test]
fn remove_entry() {
    let mut index = EntryIndex::new();
    index.insert(make_entry("key1", "v1", "h1"));

    let removed = index.remove(REPO, MAIN_REF, "key1", "v1");
    assert!(removed.is_some());
    assert_eq!(removed.unwrap().blob_hash, "h1");

    let result = index.lookup(&["key1".to_string()], "v1", REPO, &[MAIN_REF]);
    assert!(result.is_none());
}

#[test]
fn removing_an_entry_keeps_the_same_key_on_other_refs_searchable() {
    let mut index = EntryIndex::new();
    index.insert(make_scoped_entry(
        "rust-abc",
        "v1",
        "main_hash",
        REPO,
        MAIN_REF,
    ));
    index.insert(make_scoped_entry(
        "rust-abc",
        "v1",
        "feature_hash",
        REPO,
        FEATURE_REF,
    ));

    index.remove(REPO, FEATURE_REF, "rust-abc", "v1");
    let result = index.lookup(&["rust-".to_string()], "v1", REPO, &[MAIN_REF]);

    assert_eq!(result.unwrap().blob_hash, "main_hash");
}

#[test]
fn total_size() {
    let mut index = EntryIndex::new();
    let mut e1 = make_entry("k1", "v1", "h1");
    e1.size_bytes = 500;
    let mut e2 = make_entry("k2", "v1", "h2");
    e2.size_bytes = 300;

    index.insert(e1);
    index.insert(e2);

    assert_eq!(index.total_size_bytes(), 800);
}

#[test]
fn persist_and_reload() {
    let tmp = TempDir::new().unwrap();
    let entries_dir = tmp.path();

    let entry = make_entry("my-key", "my-version", "myhash");
    entry.persist(entries_dir).unwrap();

    let loaded = load_entries_from_disk(entries_dir).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].key, "my-key");
    assert_eq!(loaded[0].version, "my-version");
    assert_eq!(loaded[0].blob_hash, "myhash");
    assert_eq!(loaded[0].scope_repo, "owner/repo");
    assert_eq!(loaded[0].scope_ref, "refs/heads/main");
}

#[test]
fn lookup_updates_last_accessed() {
    let mut index = EntryIndex::new();
    let mut entry = make_entry("key1", "v1", "h1");
    let old_time = Utc::now() - chrono::Duration::hours(1);
    entry.last_accessed_at = old_time;
    index.insert(entry);

    let result = index
        .lookup(&["key1".to_string()], "v1", REPO, &[MAIN_REF])
        .unwrap();
    assert!(result.last_accessed_at > old_time);
}

// --- Scope isolation tests ---

#[test]
fn repo_isolation() {
    let mut index = EntryIndex::new();
    index.insert(make_scoped_entry(
        "key1",
        "v1",
        "hash_a",
        "org/repo-a",
        MAIN_REF,
    ));
    index.insert(make_scoped_entry(
        "key1",
        "v1",
        "hash_b",
        "org/repo-b",
        MAIN_REF,
    ));

    // repo-a can only see its own cache
    let result = index.lookup(&["key1".to_string()], "v1", "org/repo-a", &[MAIN_REF]);
    assert_eq!(result.unwrap().blob_hash, "hash_a");

    // repo-b can only see its own cache
    let result = index.lookup(&["key1".to_string()], "v1", "org/repo-b", &[MAIN_REF]);
    assert_eq!(result.unwrap().blob_hash, "hash_b");

    // unknown repo sees nothing
    let result = index.lookup(&["key1".to_string()], "v1", "org/repo-c", &[MAIN_REF]);
    assert!(result.is_none());
}

#[test]
fn ref_fallback_to_default_branch() {
    let mut index = EntryIndex::new();
    // Cache saved on main
    index.insert(make_scoped_entry("key1", "v1", "main_hash", REPO, MAIN_REF));

    // Feature branch can read from main (fallback)
    let result = index.lookup(&["key1".to_string()], "v1", REPO, &[FEATURE_REF, MAIN_REF]);
    assert!(result.is_some());
    assert_eq!(result.unwrap().blob_hash, "main_hash");
}

#[test]
fn feature_branch_prefers_own_cache() {
    let mut index = EntryIndex::new();
    index.insert(make_scoped_entry("key1", "v1", "main_hash", REPO, MAIN_REF));
    index.insert(make_scoped_entry(
        "key1",
        "v1",
        "feature_hash",
        REPO,
        FEATURE_REF,
    ));

    // Feature branch prefers its own cache
    let result = index.lookup(&["key1".to_string()], "v1", REPO, &[FEATURE_REF, MAIN_REF]);
    assert_eq!(result.unwrap().blob_hash, "feature_hash");

    // Main still sees its own
    let result = index.lookup(&["key1".to_string()], "v1", REPO, &[MAIN_REF]);
    assert_eq!(result.unwrap().blob_hash, "main_hash");
}

#[test]
fn no_reverse_fallback() {
    let mut index = EntryIndex::new();
    // Cache only on feature branch -- main should NOT see it
    index.insert(make_scoped_entry(
        "key1",
        "v1",
        "feature_hash",
        REPO,
        FEATURE_REF,
    ));

    let result = index.lookup(&["key1".to_string()], "v1", REPO, &[MAIN_REF]);
    assert!(result.is_none());
}

#[test]
fn ref_fallback_with_prefix_match() {
    let mut index = EntryIndex::new();
    index.insert(make_scoped_entry(
        "rust-cargo-abc",
        "v1",
        "main_prefix",
        REPO,
        MAIN_REF,
    ));

    // Feature branch can prefix-match from main via fallback
    let result = index.lookup(
        &["rust-cargo-".to_string()],
        "v1",
        REPO,
        &[FEATURE_REF, MAIN_REF],
    );
    assert!(result.is_some());
    assert_eq!(result.unwrap().blob_hash, "main_prefix");
}

#[test]
fn backward_compat_empty_scope_fields() {
    // Simulate an entry loaded from disk before scoping was added
    let json = r#"{
        "key": "old-key",
        "version": "v1",
        "blob_hash": "oldhash",
        "size_bytes": 100,
        "created_at": "2025-01-01T00:00:00Z",
        "last_accessed_at": "2025-01-01T00:00:00Z"
    }"#;
    let entry: CacheEntry = serde_json::from_str(json).unwrap();
    assert_eq!(entry.scope_repo, "");
    assert_eq!(entry.scope_ref, "");
}

#[test]
fn pull_request_prefers_base_branch_over_default_branch() {
    let pr_ref = "refs/pull/7/merge";
    let base_ref = "refs/heads/release";
    let mut index = EntryIndex::new();
    index.insert(make_scoped_entry("key1", "v1", "main_hash", REPO, MAIN_REF));
    index.insert(make_scoped_entry("key1", "v1", "base_hash", REPO, base_ref));

    let result = index.lookup(
        &["key1".to_string()],
        "v1",
        REPO,
        &[pr_ref, base_ref, MAIN_REF],
    );

    assert_eq!(result.unwrap().blob_hash, "base_hash");
}

#[test]
fn references_blob_only_within_readable_scope() {
    let mut index = EntryIndex::new();
    index.insert(make_scoped_entry("key1", "v1", "hash1", REPO, FEATURE_REF));

    assert!(index.references_blob("hash1", REPO, &[FEATURE_REF, MAIN_REF]));
    assert!(!index.references_blob("hash1", REPO, &[MAIN_REF]));
    assert!(!index.references_blob("hash1", "other/repo", &[FEATURE_REF]));
    assert!(!index.references_blob("hash2", REPO, &[FEATURE_REF]));
}
