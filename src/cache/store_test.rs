use std::io::Write;

use tempfile::TempDir;

use super::*;

fn make_store(tmp: &TempDir) -> BlobStore {
    let data_dir = tmp.path().join("data");
    let tmp_dir = tmp.path().join("tmp");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&tmp_dir).unwrap();
    BlobStore::new(data_dir, tmp_dir)
}

fn write_tmp_file(store: &BlobStore, content: &[u8]) -> PathBuf {
    let path = store.tmp_dir().join(uuid::Uuid::new_v4().to_string());
    let mut f = std::fs::File::create(&path).unwrap();
    f.write_all(content).unwrap();
    path
}

#[tokio::test]
async fn store_and_retrieve() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    let src = write_tmp_file(&store, b"hello world");
    let hash = store.store_from_file(&src).await.unwrap();

    assert!(!hash.is_empty());
    assert!(store.exists(&hash));

    let path = store.blob_path(&hash).unwrap();
    let content = std::fs::read(path).unwrap();
    assert_eq!(content, b"hello world");
}

#[tokio::test]
async fn dedup_same_content() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    let src1 = write_tmp_file(&store, b"same content");
    let hash1 = store.store_from_file(&src1).await.unwrap();

    let src2 = write_tmp_file(&store, b"same content");
    let hash2 = store.store_from_file(&src2).await.unwrap();

    assert_eq!(hash1, hash2);
    // Source file should be cleaned up on dedup
    assert!(!src2.exists());
}

#[tokio::test]
async fn different_content_different_hash() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    let src1 = write_tmp_file(&store, b"content a");
    let hash1 = store.store_from_file(&src1).await.unwrap();

    let src2 = write_tmp_file(&store, b"content b");
    let hash2 = store.store_from_file(&src2).await.unwrap();

    assert_ne!(hash1, hash2);
}

#[tokio::test]
async fn ref_counting() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    let src = write_tmp_file(&store, b"refcount test");
    let hash = store.store_from_file(&src).await.unwrap();

    store.incref(&hash).await;
    store.incref(&hash).await;

    // First decref: count 2→1, blob stays
    assert!(!store.decref(&hash).await);
    assert!(store.exists(&hash));

    // Second decref: count 1→0, blob deleted
    assert!(store.decref(&hash).await);
    assert!(!store.exists(&hash));
}

#[tokio::test]
async fn blob_not_found() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    let result = store.blob_path("nonexistent");
    assert!(result.is_err());
}

#[tokio::test]
async fn total_bytes() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    assert_eq!(store.total_bytes().unwrap(), 0);

    let src = write_tmp_file(&store, b"12345");
    store.store_from_file(&src).await.unwrap();

    assert_eq!(store.total_bytes().unwrap(), 5);
}

#[tokio::test]
async fn all_hashes() {
    let tmp = TempDir::new().unwrap();
    let store = make_store(&tmp);

    let src1 = write_tmp_file(&store, b"aaa");
    let hash1 = store.store_from_file(&src1).await.unwrap();

    let src2 = write_tmp_file(&store, b"bbb");
    let hash2 = store.store_from_file(&src2).await.unwrap();

    let mut hashes = store.all_hashes().unwrap();
    hashes.sort();
    let mut expected = vec![hash1, hash2];
    expected.sort();
    assert_eq!(hashes, expected);
}

#[tokio::test]
async fn concurrent_writes_same_content() {
    let tmp = TempDir::new().unwrap();
    let store = std::sync::Arc::new(make_store(&tmp));

    let mut handles = Vec::new();
    for _ in 0..10 {
        let s = store.clone();
        handles.push(tokio::spawn(async move {
            let src = write_tmp_file(&s, b"concurrent content");
            s.store_from_file(&src).await.unwrap()
        }));
    }

    let mut hashes = Vec::new();
    for h in handles {
        hashes.push(h.await.unwrap());
    }

    // All should produce the same hash
    assert!(hashes.windows(2).all(|w| w[0] == w[1]));
}
