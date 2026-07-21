use pgrx::prelude::PgSqlErrorCode;

use context_build::BuildJobKind;

use super::BuildJobStatus;
use crate::domain_types::ArtifactKind;
use crate::error::raise_sql_error;

/// Classifies a pgContext-owned generation target.
///
/// `index` and `sparse_index` are historical target labels for derived
/// pgContext projections; they never make a native PostgreSQL `CREATE INDEX`
/// operation resumable.
pub(super) const fn build_generation_kind(kind: ArtifactKind) -> BuildJobKind {
    match kind {
        ArtifactKind::Segment | ArtifactKind::Mmap => BuildJobKind::Artifact,
        ArtifactKind::Index | ArtifactKind::SparseIndex => BuildJobKind::Projection,
    }
}

pub(super) fn validate_non_empty_text(value: &str, argument_name: &'static str) {
    if value.trim().is_empty() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{argument_name} must not be empty"),
        );
    }
}

pub(super) fn validate_non_negative_units(value: i64, argument_name: &'static str) {
    if value < 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{argument_name} must be non-negative: {value}"),
        );
    }
}

pub(super) fn validate_positive_units(value: i64, argument_name: &'static str) {
    if value <= 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("{argument_name} must be positive: {value}"),
        );
    }
}

pub(super) fn ensure_build_runner_supported(artifact_kind: ArtifactKind) {
    match artifact_kind {
        ArtifactKind::Segment | ArtifactKind::Mmap => {}
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!(
                "build runner supports only segment and mmap artifact kinds for now: {}",
                artifact_kind.as_sql()
            ),
        ),
    }
}

pub(super) fn parse_build_status_command(status: &str) -> BuildJobStatus {
    match status {
        "running" => BuildJobStatus::Running,
        "completed" => BuildJobStatus::Completed,
        "failed" => BuildJobStatus::Failed,
        "cancelled" => BuildJobStatus::Cancelled,
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("unsupported build job status: {status}"),
        ),
    }
}

pub(super) fn build_status_from_catalog(status: &str) -> BuildJobStatus {
    match status {
        "planned" => BuildJobStatus::Planned,
        "running" => BuildJobStatus::Running,
        "cancel_requested" => BuildJobStatus::CancelRequested,
        "cancelled" => BuildJobStatus::Cancelled,
        "completed" => BuildJobStatus::Completed,
        "failed" => BuildJobStatus::Failed,
        "abandoned" => BuildJobStatus::Abandoned,
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("unexpected build job status in catalog: {status}"),
        ),
    }
}
