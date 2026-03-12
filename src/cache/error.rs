use thiserror::Error;

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("upload session {0} not found")]
    UploadNotFound(u64),

    #[error("invalid Content-Range header: {0}")]
    InvalidContentRange(String),

    #[error("committed size {committed} does not match expected {expected}")]
    SizeMismatch { committed: u64, expected: u64 },

    #[error("blob not found: {0}")]
    BlobNotFound(String),
}
