//! Error types for the openshmem crate.

use std::fmt;

use pmix::PmixError;
use ucx_sys::ucs_status_t;

/// Unified error type across the PMIx / UCX / UCC layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// An error surfaced from the PMIx bootstrap layer.
    Pmix(PmixError),
    /// An error surfaced from the UCX RMA/atomics layer.
    Ucx(ucs_status_t),
    /// An error surfaced from the UCC collective layer.
    #[cfg(feature = "collectives")]
    Ucc(ucc::UccStatus),
    /// A programming error local to this crate.
    Usage(&'static str),
    /// The library is not initialized.
    NotInitialized,
    /// The library has already been initialized.
    AlreadyInitialized,
    /// A status that cannot represent an operation failure was returned as an error.
    Internal(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;

impl From<PmixError> for Error {
    fn from(error: PmixError) -> Self {
        if matches!(error, PmixError::Success) {
            Self::Internal("PMIx success was returned where an error was expected")
        } else {
            Self::Pmix(error)
        }
    }
}

impl From<ucs_status_t> for Error {
    fn from(status: ucs_status_t) -> Self {
        if matches!(status, ucs_status_t::UCS_OK) {
            Self::Internal("UCX success was returned where an error was expected")
        } else {
            Self::Ucx(status)
        }
    }
}

#[cfg(feature = "collectives")]
impl From<ucc::UccError> for Error {
    fn from(error: ucc::UccError) -> Self {
        if error.is_error() {
            Self::Ucc(error.into())
        } else {
            Self::Internal("UCC informational status was returned where an error was expected")
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pmix(error) => write!(f, "pmix error: {error}"),
            Self::Ucx(status) => write!(f, "ucx error: {status:?} (code {})", *status as i8),
            #[cfg(feature = "collectives")]
            Self::Ucc(error) => write!(f, "ucc error: {error} (code {})", error.to_raw()),
            Self::Usage(message) => write!(f, "usage error: {message}"),
            Self::NotInitialized => write!(f, "openshmem not initialized"),
            Self::AlreadyInitialized => write!(f, "openshmem already initialized"),
            Self::Internal(message) => write!(f, "internal error: {message}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pmix_error_converts_and_displays() {
        let error = Error::from(PmixError::Error);
        assert!(matches!(&error, Error::Pmix(PmixError::Error)));
        assert!(error.to_string().contains("pmix error"));
    }

    #[test]
    fn ucx_error_converts_and_displays_status_code() {
        let error = Error::from(ucs_status_t::UCS_ERR_INVALID_PARAM);
        assert!(matches!(
            &error,
            Error::Ucx(ucs_status_t::UCS_ERR_INVALID_PARAM)
        ));
        assert!(error.to_string().contains("-5"));
    }

    #[cfg(feature = "collectives")]
    #[test]
    fn ucc_error_converts_only_failures() {
        let error = Error::from(ucc::UccError::ErrInvalidParam);
        assert!(matches!(
            error,
            Error::Ucc(ucc::UccStatus::Known(ucc::UccError::ErrInvalidParam))
        ));
        assert!(matches!(
            Error::from(ucc::UccError::InProgress),
            Error::Internal(_)
        ));
    }
}
