//! Typed codec failures.

use context_core::Error as CoreError;

/// Result returned by codec operations.
pub type Result<T> = core::result::Result<T, CodecError>;

/// Errors produced by codec validation, training, encoding, and scoring.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum CodecError {
    /// Core vector validation failed.
    #[error("{0}")]
    Core(#[from] CoreError),
    /// Vector or codebook dimensions are incompatible.
    #[error("dimension mismatch: left has {left} dimensions, right has {right}")]
    DimensionMismatch {
        /// Expected dimensions.
        left: usize,
        /// Actual dimensions.
        right: usize,
    },
    /// Encoded data or trained codec state violates its format contract.
    #[error("invalid codec data: {0}")]
    InvalidCode(String),
}
