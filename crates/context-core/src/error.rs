//! Error types shared by framework-free pgContext domain APIs.

/// Result type used by `context-core`.
pub type Result<T> = core::result::Result<T, Error>;

/// Stable framework-free semantic error taxonomy for infrastructure adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextError {
    /// Malformed vector, sparse vector, bit vector, or binary vector input.
    InvalidVector,
    /// Valid input has incompatible dimensions or typmods.
    DimensionMismatch,
    /// Malformed or semantically invalid filter JSON.
    InvalidFilter,
    /// Rejected predicate rendering or unsafe identifier/path input.
    UnsafePredicate,
    /// Registered collection does not exist.
    UnknownCollection,
    /// Registered vector name does not exist for the collection.
    UnknownVector,
    /// Registered payload field/path does not exist or drifted.
    UnknownPayloadField,
    /// PostgreSQL ACL, RLS, or ownership check denied access.
    AclDenied,
    /// Requested index or artifact exists but cannot currently serve queries.
    IndexNotReady,
    /// Index or artifact validation found corruption.
    IndexCorrupt,
    /// Search could not satisfy recall or limit within configured resources.
    RecallBudgetExceeded,
    /// Requested metric is not supported for this vector or index kind.
    UnsupportedMetric,
    /// Running PostgreSQL version is outside the supported matrix.
    UnsupportedPostgresVersion,
    /// Bug or unexpected internal invariant violation.
    Internal,
}

/// Framework-free error categories produced by core vector and search code.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum Error {
    /// The dense vector text or value is malformed.
    #[error("invalid vector: {0}")]
    InvalidVector(String),

    /// Two vectors have incompatible dimensions.
    #[error("dimension mismatch: left has {left} dimensions, right has {right}")]
    DimensionMismatch {
        /// Dimension of the left-hand vector.
        left: usize,
        /// Dimension of the right-hand vector.
        right: usize,
    },

    /// The requested search limit is outside supported bounds.
    #[error("invalid search limit: {0}")]
    InvalidSearchLimit(usize),

    /// A SQL-visible catalog identifier failed validation.
    #[error("invalid {kind}: {reason}: {value:?}")]
    InvalidIdentifier {
        /// Identifier category.
        kind: &'static str,
        /// User-provided identifier value.
        value: String,
        /// Stable validation failure reason.
        reason: &'static str,
    },

    /// The requested dense-vector dimension count is outside supported bounds.
    #[error("invalid vector dimensions: {0}")]
    InvalidVectorDimensions(usize),

    /// A source row key is empty or exceeds supported bounds.
    #[error("invalid source key: {0:?}")]
    InvalidSourceKey(String),
}

impl Error {
    /// Returns the stable error category used by SQL adapters.
    #[must_use]
    pub const fn context_error(&self) -> ContextError {
        match self {
            Self::InvalidVector(_) => ContextError::InvalidVector,
            Self::DimensionMismatch { .. } => ContextError::DimensionMismatch,
            Self::InvalidSearchLimit(_)
            | Self::InvalidIdentifier { .. }
            | Self::InvalidVectorDimensions(_)
            | Self::InvalidSourceKey(_) => ContextError::InvalidFilter,
        }
    }
}
