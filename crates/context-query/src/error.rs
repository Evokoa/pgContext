//! Transport-neutral query execution errors.

use std::fmt;

use context_core::PointId;

/// Result type used by the pure query boundary.
pub type Result<T> = std::result::Result<T, QueryError>;

/// Semantic and port-contract failures produced by query orchestration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryError {
    /// Query or budget input is invalid.
    InvalidInput {
        /// Input field that failed validation.
        field: &'static str,
        /// Stable human-readable reason.
        reason: String,
    },
    /// An infrastructure adapter failed while performing a named stage.
    PortFailure {
        /// Port/stage that failed.
        stage: &'static str,
        /// Adapter-provided detail without transport codes.
        message: String,
    },
    /// A port returned more owned values than the executor requested.
    PortContractViolation {
        /// Port/stage that violated its bound.
        stage: &'static str,
        /// Maximum number of values requested.
        requested: usize,
        /// Number of values returned.
        returned: usize,
    },
    /// A port returned a logical point that was not present in its input page.
    UnexpectedPointId {
        /// Port/stage that introduced the point.
        stage: &'static str,
        /// Unexpected logical identifier.
        point_id: PointId,
    },
    /// A checked work projection exceeded its hard application budget.
    WorkBudgetExceeded {
        /// Stable budget name.
        budget: &'static str,
        /// Projected work.
        actual: usize,
        /// Maximum accepted work.
        maximum: usize,
    },
    /// A checked application work projection overflowed.
    ArithmeticOverflow {
        /// Stable operation name.
        operation: &'static str,
    },
}

impl fmt::Display for QueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput { field, reason } => {
                write!(formatter, "invalid {field}: {reason}")
            }
            Self::PortFailure { stage, message } => {
                write!(formatter, "{stage} failed: {message}")
            }
            Self::PortContractViolation {
                stage,
                requested,
                returned,
            } => write!(
                formatter,
                "{stage} returned {returned} values after a request bounded at {requested}"
            ),
            Self::UnexpectedPointId { stage, point_id } => {
                write!(
                    formatter,
                    "{stage} returned unexpected point ID {}",
                    point_id.get()
                )
            }
            Self::WorkBudgetExceeded {
                budget,
                actual,
                maximum,
            } => write!(formatter, "{budget} budget exceeded: {actual} > {maximum}"),
            Self::ArithmeticOverflow { operation } => {
                write!(formatter, "arithmetic overflow during {operation}")
            }
        }
    }
}

impl std::error::Error for QueryError {}

impl From<context_core::Error> for QueryError {
    fn from(error: context_core::Error) -> Self {
        Self::InvalidInput {
            field: "query",
            reason: error.to_string(),
        }
    }
}
