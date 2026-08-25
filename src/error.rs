//! Error types for the openshmem crate.

use std::fmt;

/// Unified error type across the PMIx / UCX / UCC layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// An error surfaced from the PMIx bootstrap layer.
    Pmix(String),
    /// An error surfaced from the UCX RMA/atomics layer (a `ucs_status_t`).
    Ucx(String),
    /// An error surfaced from the UCC collective layer (feature-gated).
    Ucc(String),
    /// A programming error local to this crate (bad argument, not initialized, etc.).
    Usage(&'static str),
    /// The library is not initialized (shmem_init not called or already finalized).
    NotInitialized,
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Pmix(s) => write!(f, "pmix error: {s}"),
            Error::Ucx(s) => write!(f, "ucx error: {s}"),
            Error::Ucc(s) => write!(f, "ucc error: {s}"),
            Error::Usage(s) => write!(f, "usage error: {s}"),
            Error::NotInitialized => write!(f, "openshmem not initialized"),
        }
    }
}

impl std::error::Error for Error {}
