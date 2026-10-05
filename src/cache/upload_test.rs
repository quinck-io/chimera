use tempfile::TempDir;

use super::*;

fn main_scope() -> CacheScope {
    CacheScope {
        repo: "owner/repo".into(),
        git_ref: "refs/heads/main".into(),
        fallback_refs: vec!["refs/heads/main".into()],
    }
}

fn make_tracker(tmp: &TempDir) -> UploadTracker {
    let tmp_dir = tmp.path().join("uploads");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    UploadTracker::new(tmp_dir)
}

#[tokio::test]
async fn reserve_and_commit() {
    let tmp = TempDir::new().unwrap();
    let tracker = make_tracker(&tmp);

    let id = tracker
        .reserve(
            "my-key".into(),
            "my-version".into(),
            "owner/repo".into(),
            "refs/heads/main".into(),
            "job-token".into(),
        )
        .await
        .unwrap();

    let data = b"hello world";
    tracker
        .write_chunk(id, &main_scope(), 0, data)
        .await
        .unwrap();

    let (key, version, scope_repo, scope_ref, path, size) = tracker
        .commit(id, &main_scope(), data.len() as u64)
        .await
        .unwrap();
    assert_eq!(key, "my-key");
    assert_eq!(version, "my-version");
    assert_eq!(scope_repo, "owner/repo");
    assert_eq!(scope_ref, "refs/heads/main");
    assert_eq!(size, data.len() as u64);

    let content = std::fs::read(path).unwrap();
    assert_eq!(content, data);
}

#[tokio::test]
async fn chunked_upload() {
    let tmp = TempDir::new().unwrap();
    let tracker = make_tracker(&tmp);

    let id = tracker
        .reserve(
            "k".into(),
            "v".into(),
            "owner/repo".into(),
            "refs/heads/main".into(),
            "job-token".into(),
        )
        .await
        .unwrap();

    tracker
        .write_chunk(id, &main_scope(), 0, b"hello")
        .await
        .unwrap();
    tracker
        .write_chunk(id, &main_scope(), 5, b" world")
        .await
        .unwrap();

    let (_, _, _, _, path, size) = tracker.commit(id, &main_scope(), 11).await.unwrap();
    assert_eq!(size, 11);

    let content = std::fs::read(path).unwrap();
    assert_eq!(content, b"hello world");
}

#[tokio::test]
async fn size_mismatch() {
    let tmp = TempDir::new().unwrap();
    let tracker = make_tracker(&tmp);

    let id = tracker
        .reserve(
            "k".into(),
            "v".into(),
            "owner/repo".into(),
            "refs/heads/main".into(),
            "job-token".into(),
        )
        .await
        .unwrap();
    tracker
        .write_chunk(id, &main_scope(), 0, b"short")
        .await
        .unwrap();

    let result = tracker.commit(id, &main_scope(), 100).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("does not match"));
}

#[tokio::test]
async fn upload_not_found() {
    let tmp = TempDir::new().unwrap();
    let tracker = make_tracker(&tmp);

    let result = tracker.write_chunk(999, &main_scope(), 0, b"data").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn other_scope_cannot_write_or_commit_upload() {
    let tmp = TempDir::new().unwrap();
    let tracker = make_tracker(&tmp);
    let id = tracker
        .reserve(
            "k".into(),
            "v".into(),
            "owner/repo".into(),
            "refs/heads/main".into(),
            "job-token".into(),
        )
        .await
        .unwrap();
    let other_branch = CacheScope {
        git_ref: "refs/pull/7/merge".into(),
        ..main_scope()
    };
    let other_repo = CacheScope {
        repo: "attacker/repo".into(),
        ..main_scope()
    };

    let foreign_write = tracker.write_chunk(id, &other_branch, 0, b"evil").await;
    let foreign_commit = tracker.commit(id, &other_repo, 0).await;

    assert!(foreign_write.is_err());
    assert!(foreign_commit.is_err());
    tracker
        .write_chunk(id, &main_scope(), 0, b"ok")
        .await
        .unwrap();
    let (_, _, _, _, path, _) = tracker.commit(id, &main_scope(), 2).await.unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"ok");
}

#[test]
fn parse_content_range_valid() {
    let (start, end) = parse_content_range("bytes 0-99/*").unwrap();
    assert_eq!(start, 0);
    assert_eq!(end, 99);
}

#[test]
fn parse_content_range_with_total() {
    let (start, end) = parse_content_range("bytes 100-199/200").unwrap();
    assert_eq!(start, 100);
    assert_eq!(end, 199);
}

#[test]
fn parse_content_range_invalid() {
    assert!(parse_content_range("invalid").is_err());
    assert!(parse_content_range("bytes abc-def/*").is_err());
    assert!(parse_content_range("bytes 0/*").is_err());
}

#[tokio::test]
async fn cleanup_stale_files() {
    let tmp = TempDir::new().unwrap();
    let upload_dir = tmp.path().join("uploads");
    std::fs::create_dir_all(&upload_dir).unwrap();

    // Create stale upload files
    std::fs::write(upload_dir.join("upload-1.tmp"), "stale").unwrap();
    std::fs::write(upload_dir.join("upload-2.tmp"), "stale").unwrap();
    // Non-upload file should be left alone
    std::fs::write(upload_dir.join("other.txt"), "keep").unwrap();

    UploadTracker::cleanup_stale_files(&upload_dir);

    assert!(!upload_dir.join("upload-1.tmp").exists());
    assert!(!upload_dir.join("upload-2.tmp").exists());
    assert!(upload_dir.join("other.txt").exists());
}

async fn reserve_for(tracker: &UploadTracker, token: &str) -> u64 {
    tracker
        .reserve(
            "k".into(),
            "v".into(),
            "owner/repo".into(),
            "refs/heads/main".into(),
            token.into(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn discard_abandoned_drops_only_sessions_of_dead_tokens() {
    let tmp = TempDir::new().unwrap();
    let tracker = make_tracker(&tmp);
    let dead = reserve_for(&tracker, "ended-job").await;
    let live = reserve_for(&tracker, "running-job").await;
    tracker
        .write_chunk(dead, &main_scope(), 0, b"partial")
        .await
        .unwrap();
    tracker
        .write_chunk(live, &main_scope(), 0, b"data")
        .await
        .unwrap();

    let discarded = tracker
        .discard_abandoned(|token| token == "running-job")
        .await;

    assert_eq!(discarded, 1);
    assert!(tracker.commit(dead, &main_scope(), 7).await.is_err());
    assert!(tracker.commit(live, &main_scope(), 4).await.is_ok());
}

#[tokio::test]
async fn discard_abandoned_removes_partial_upload_file() {
    let tmp = TempDir::new().unwrap();
    let tracker = make_tracker(&tmp);
    reserve_for(&tracker, "ended-job").await;
    let uploads_dir = tmp.path().join("uploads");
    assert_eq!(std::fs::read_dir(&uploads_dir).unwrap().count(), 1);

    tracker.discard_abandoned(|_| false).await;

    assert_eq!(std::fs::read_dir(&uploads_dir).unwrap().count(), 0);
}
