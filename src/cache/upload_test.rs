use tempfile::TempDir;

use super::*;

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
        )
        .await
        .unwrap();

    let data = b"hello world";
    tracker.write_chunk(id, 0, data).await.unwrap();

    let (key, version, scope_repo, scope_ref, path, size) =
        tracker.commit(id, data.len() as u64).await.unwrap();
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
        )
        .await
        .unwrap();

    tracker.write_chunk(id, 0, b"hello").await.unwrap();
    tracker.write_chunk(id, 5, b" world").await.unwrap();

    let (_, _, _, _, path, size) = tracker.commit(id, 11).await.unwrap();
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
        )
        .await
        .unwrap();
    tracker.write_chunk(id, 0, b"short").await.unwrap();

    let result = tracker.commit(id, 100).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("does not match"));
}

#[tokio::test]
async fn upload_not_found() {
    let tmp = TempDir::new().unwrap();
    let tracker = make_tracker(&tmp);

    let result = tracker.write_chunk(999, 0, b"data").await;
    assert!(result.is_err());
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
