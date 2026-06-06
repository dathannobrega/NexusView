//! Error model for the engine.
//!
//! Every fallible public operation returns [`Result`]. The data path never
//! panics (RNF-05): malformed input is sanitized or surfaced as a typed error,
//! and the FFI layer additionally wraps calls in `catch_unwind` as a backstop.

use thiserror::Error;

/// All errors the engine can produce.
#[derive(Debug, Error)]
pub enum NexusError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    #[error("file is empty")]
    EmptyFile,

    #[error("memory map failed: {0}")]
    Mmap(String),

    #[error("invalid parser schema: {0}")]
    InvalidSchema(String),

    #[error("invalid search query: {0}")]
    InvalidQuery(String),

    #[error("index {index} out of bounds (len {len})")]
    OutOfBounds { index: usize, len: usize },

    #[error("no columns could be determined for the dataset")]
    NoColumns,
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, NexusError>;
