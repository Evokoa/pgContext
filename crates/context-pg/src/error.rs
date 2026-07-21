//! SQLSTATE mapping helpers shared by pgContext SQL adapters.

use context_core::{ContextError, Error as CoreError};
use context_query::QueryError;
use pgrx::prelude::*;

/// Returns the stable SQLSTATE string for a pgContext error category.
#[allow(dead_code)]
#[must_use]
pub const fn sqlstate_for_context_error(error: ContextError) -> &'static str {
    match error {
        ContextError::InvalidVector => "22P02",
        ContextError::DimensionMismatch
        | ContextError::InvalidFilter
        | ContextError::UnsafePredicate => "22023",
        ContextError::UnknownCollection | ContextError::UnknownVector => "42704",
        ContextError::UnknownPayloadField => "42703",
        ContextError::AclDenied => "42501",
        ContextError::IndexNotReady => "55000",
        ContextError::IndexCorrupt => "XX001",
        ContextError::RecallBudgetExceeded => "54000",
        ContextError::UnsupportedMetric | ContextError::UnsupportedPostgresVersion => "0A000",
        ContextError::Internal => "XX000",
    }
}

/// Returns the PostgreSQL error code for a pure query error.
#[must_use]
pub const fn sql_error_code_for_query_error(error: &QueryError) -> PgSqlErrorCode {
    match error {
        QueryError::InvalidInput { .. } => PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        QueryError::WorkBudgetExceeded { .. } | QueryError::ArithmeticOverflow { .. } => {
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED
        }
        QueryError::PortFailure { .. }
        | QueryError::PortContractViolation { .. }
        | QueryError::UnexpectedPointId { .. } => PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
    }
}

/// Returns the stable PostgreSQL-facing message for a pure query error.
#[must_use]
pub fn query_error_message(error: &QueryError) -> String {
    match error {
        QueryError::InvalidInput { reason, .. } => reason.clone(),
        QueryError::ArithmeticOverflow {
            operation: "late_interaction_comparison_projection",
        } => "late interaction comparison budget overflow".to_owned(),
        QueryError::WorkBudgetExceeded {
            budget: "late_interaction_comparisons",
            actual,
            maximum,
        } => format!("late interaction comparison budget exceeded: {actual} > {maximum}"),
        error => error.to_string(),
    }
}

/// Returns the PostgreSQL error code for a pgContext error category.
#[allow(dead_code)]
#[must_use]
pub const fn sql_error_code_for_context_error(error: ContextError) -> PgSqlErrorCode {
    match error {
        ContextError::InvalidVector => PgSqlErrorCode::ERRCODE_INVALID_TEXT_REPRESENTATION,
        ContextError::DimensionMismatch
        | ContextError::InvalidFilter
        | ContextError::UnsafePredicate => PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        ContextError::UnknownCollection | ContextError::UnknownVector => {
            PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT
        }
        ContextError::UnknownPayloadField => PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
        ContextError::AclDenied => PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
        ContextError::IndexNotReady => PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
        ContextError::IndexCorrupt => PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
        ContextError::RecallBudgetExceeded => PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
        ContextError::UnsupportedMetric | ContextError::UnsupportedPostgresVersion => {
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED
        }
        ContextError::Internal => PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
    }
}

/// Raises a PostgreSQL `ERROR` for a stable pgContext error category.
#[allow(dead_code)]
pub fn raise_context_error(error: ContextError, message: impl Into<String>) -> ! {
    raise_sql_error(sql_error_code_for_context_error(error), message)
}

/// Raises a PostgreSQL `ERROR` for a framework-free core error.
pub fn raise_core_error(error: CoreError) -> ! {
    raise_context_error(error.context_error(), error.to_string())
}

/// Raises a PostgreSQL `ERROR` for a framework-free query error.
pub fn raise_query_error(error: QueryError) -> ! {
    let error_code = sql_error_code_for_query_error(&error);
    let message = query_error_message(&error);
    raise_sql_error(error_code, message)
}

/// Raises a PostgreSQL `ERROR` with the supplied SQLSTATE and message.
#[allow(unreachable_code)]
pub fn raise_sql_error(message_code: PgSqlErrorCode, message: impl Into<String>) -> ! {
    pgrx::ereport!(ERROR, message_code, message.into());
    unreachable!("pgrx ERROR reports abort the current PostgreSQL transaction")
}

/// Raises a PostgreSQL `ERROR` with the supplied SQLSTATE, message, and HINT.
#[allow(unreachable_code)]
pub fn raise_sql_error_with_hint(
    message_code: PgSqlErrorCode,
    message: impl Into<String>,
    hint: impl Into<String>,
) -> ! {
    pg_sys::panic::ErrorReport::new(message_code, message.into(), pgrx::function_name!())
        .set_hint(hint.into())
        .report(PgLogLevel::ERROR);
    unreachable!("pgrx ERROR reports abort the current PostgreSQL transaction")
}

#[cfg(test)]
mod tests {
    use super::{
        query_error_message, sql_error_code_for_context_error, sql_error_code_for_query_error,
        sqlstate_for_context_error,
    };
    use context_core::ContextError;
    use context_query::QueryError;
    use pgrx::prelude::PgSqlErrorCode;

    #[test]
    fn context_error_sqlstate_strings_are_stable() {
        let cases = [
            (ContextError::InvalidVector, "22P02"),
            (ContextError::DimensionMismatch, "22023"),
            (ContextError::InvalidFilter, "22023"),
            (ContextError::UnsafePredicate, "22023"),
            (ContextError::UnknownCollection, "42704"),
            (ContextError::UnknownVector, "42704"),
            (ContextError::UnknownPayloadField, "42703"),
            (ContextError::AclDenied, "42501"),
            (ContextError::IndexNotReady, "55000"),
            (ContextError::IndexCorrupt, "XX001"),
            (ContextError::RecallBudgetExceeded, "54000"),
            (ContextError::UnsupportedMetric, "0A000"),
            (ContextError::UnsupportedPostgresVersion, "0A000"),
            (ContextError::Internal, "XX000"),
        ];

        for (error, sqlstate) in cases {
            assert_eq!(sqlstate_for_context_error(error), sqlstate);
        }
    }

    #[test]
    fn context_errors_map_to_postgres_error_codes() {
        assert_eq!(
            sql_error_code_for_context_error(ContextError::InvalidVector),
            PgSqlErrorCode::ERRCODE_INVALID_TEXT_REPRESENTATION
        );
        assert_eq!(
            sql_error_code_for_context_error(ContextError::InvalidFilter),
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE
        );
        assert_eq!(
            sql_error_code_for_context_error(ContextError::IndexCorrupt),
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED
        );
        assert_eq!(
            sql_error_code_for_context_error(ContextError::UnsupportedPostgresVersion),
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED
        );
    }

    #[test]
    fn query_errors_map_only_at_the_postgres_boundary() {
        let invalid = QueryError::InvalidInput {
            field: "weight",
            reason: "query branch weight must be finite".to_owned(),
        };
        assert_eq!(
            sql_error_code_for_query_error(&invalid),
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE
        );
        assert_eq!(
            query_error_message(&invalid),
            "query branch weight must be finite"
        );

        let budget = QueryError::WorkBudgetExceeded {
            budget: "late_interaction_comparisons",
            actual: 18,
            maximum: 17,
        };
        assert_eq!(
            sql_error_code_for_query_error(&budget),
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED
        );
        assert_eq!(
            query_error_message(&budget),
            "late interaction comparison budget exceeded: 18 > 17"
        );

        let internal = QueryError::PortContractViolation {
            stage: "candidate_source",
            requested: 1,
            returned: 2,
        };
        assert_eq!(
            sql_error_code_for_query_error(&internal),
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR
        );
        assert_eq!(
            query_error_message(&internal),
            "candidate_source returned 2 values after a request bounded at 1"
        );

        let port_failure = QueryError::PortFailure {
            stage: "telemetry",
            message: "sink unavailable".to_owned(),
        };
        assert_eq!(
            sql_error_code_for_query_error(&port_failure),
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR
        );
        assert_eq!(
            query_error_message(&port_failure),
            "telemetry failed: sink unavailable"
        );

        let unexpected = QueryError::UnexpectedPointId {
            stage: "source_rechecker",
            point_id: context_core::PointId::new(99),
        };
        assert_eq!(
            sql_error_code_for_query_error(&unexpected),
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR
        );

        let overflow = QueryError::ArithmeticOverflow {
            operation: "late_interaction_comparison_projection",
        };
        assert_eq!(
            sql_error_code_for_query_error(&overflow),
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED
        );
        assert_eq!(
            query_error_message(&overflow),
            "late interaction comparison budget overflow"
        );

        let generic_overflow = QueryError::ArithmeticOverflow {
            operation: "multi_vector_comparison_projection",
        };
        assert_eq!(
            query_error_message(&generic_overflow),
            "arithmetic overflow during multi_vector_comparison_projection"
        );

        let generic_budget = QueryError::WorkBudgetExceeded {
            budget: "candidate pages",
            actual: 9,
            maximum: 8,
        };
        assert_eq!(
            sql_error_code_for_query_error(&generic_budget),
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED
        );
        assert_eq!(
            query_error_message(&generic_budget),
            "candidate pages budget exceeded: 9 > 8"
        );
    }
}
