use std::io::ErrorKind;

use context_storage::{SegmentBytes, SegmentError, SegmentFileError, load_segment_file};
use pgrx::prelude::*;

use super::{
    ArtifactSegmentKind, ArtifactSegmentRow, artifact_absolute_path,
    artifact_relative_path_is_confined,
};
use crate::domain_types::ArtifactLifecycleState;
use crate::error::raise_sql_error;

type ArtifactSegmentDiagnosticResult = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    bool,
    Option<String>,
    i64,
    Option<i64>,
    i64,
    Option<i64>,
);

pub(super) fn artifact_segment_diagnostic_result(
    row: ArtifactSegmentRow,
) -> ArtifactSegmentDiagnosticResult {
    if row.lifecycle_state == ArtifactLifecycleState::Retired {
        return diagnostic_row(
            row,
            "metadata_only",
            "artifact manifest is retired",
            None,
            None,
        );
    }
    let Some(relative_path) = row.relative_path.as_deref() else {
        return diagnostic_row(
            row,
            "metadata_only",
            "artifact has no materialized file",
            None,
            None,
        );
    };
    if !artifact_relative_path_is_confined(relative_path) {
        return diagnostic_row(
            row,
            "path_rejected",
            "artifact relative path is outside pgcontext_artifacts",
            None,
            None,
        );
    }
    let path = artifact_absolute_path(relative_path);
    match load_segment_file(&path) {
        Ok(segment) => loaded_diagnostic(row, segment),
        Err(SegmentFileError::Io {
            operation: "open",
            source,
            ..
        }) if source.kind() == ErrorKind::NotFound => diagnostic_row(
            row,
            "artifact_missing",
            "artifact file is missing",
            None,
            None,
        ),
        Err(SegmentFileError::Format(SegmentError::ChecksumMismatch)) => diagnostic_row(
            row,
            "checksum_mismatch",
            "segment checksum mismatch",
            None,
            None,
        ),
        Err(SegmentFileError::Format(error)) => {
            diagnostic_row(row, "artifact_corrupt", error.to_string(), None, None)
        }
        Err(error) => diagnostic_row(
            row,
            "artifact_corrupt",
            file_error_detail(&error),
            None,
            None,
        ),
    }
}

fn loaded_diagnostic(
    row: ArtifactSegmentRow,
    segment: SegmentBytes,
) -> ArtifactSegmentDiagnosticResult {
    let file_kind = ArtifactSegmentKind::from(segment.header().kind());
    let file_format_version =
        i32::try_from(segment.header().version().as_u32()).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "segment format version exceeds PostgreSQL integer range",
            )
        });
    let file_payload_bytes = i64::try_from(segment.payload().len()).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "segment payload length exceeds PostgreSQL bigint range",
        )
    });
    let file_checksum = i64::from_ne_bytes(segment.header().checksum().to_ne_bytes());
    let metadata_matches = row.segment_kind == file_kind
        && row.format_version == file_format_version
        && row.payload_bytes == file_payload_bytes
        && row.checksum == file_checksum;
    if metadata_matches {
        diagnostic_row(
            row,
            "ready",
            "artifact file matches catalog metadata",
            Some(file_payload_bytes),
            Some(file_checksum),
        )
    } else {
        diagnostic_row(
            row,
            "metadata_mismatch",
            "artifact file metadata differs from catalog",
            Some(file_payload_bytes),
            Some(file_checksum),
        )
    }
}

fn diagnostic_row(
    row: ArtifactSegmentRow,
    status: impl Into<String>,
    detail: impl Into<String>,
    file_payload_bytes: Option<i64>,
    file_checksum: Option<i64>,
) -> ArtifactSegmentDiagnosticResult {
    let status = status.into();
    let cleanup_eligible = row.lifecycle_state != ArtifactLifecycleState::Retired
        && row
            .relative_path
            .as_deref()
            .is_some_and(artifact_relative_path_is_confined);
    let repair_advice = repair_advice(&row, &status).to_owned();
    (
        row.artifact_kind.as_sql().to_owned(),
        row.artifact_name,
        row.target_name,
        row.lifecycle_state.as_sql().to_owned(),
        status,
        detail.into(),
        repair_advice,
        cleanup_eligible,
        row.relative_path,
        row.payload_bytes,
        file_payload_bytes,
        row.checksum,
        file_checksum,
    )
}

fn repair_advice(row: &ArtifactSegmentRow, status: &str) -> &'static str {
    match status {
        "ready" => "no repair needed",
        "metadata_only" if row.lifecycle_state == ArtifactLifecycleState::Retired => {
            "artifact manifest is retired; no materialized file cleanup is pending"
        }
        "metadata_only" => "no materialized file cleanup is pending",
        "artifact_missing" => {
            "retire the manifest or rebuild the artifact after investigating the missing file"
        }
        "path_rejected" => "fix or remove the invalid catalog path before retiring the artifact",
        "checksum_mismatch" | "artifact_corrupt" | "metadata_mismatch" => {
            "retire the manifest and rebuild the artifact from source data"
        }
        _ => "inspect the artifact status before taking repair action",
    }
}

fn file_error_detail(error: &SegmentFileError) -> &'static str {
    match error {
        SegmentFileError::InvalidPath { .. } => "artifact file path is invalid",
        SegmentFileError::TempNameExhausted { .. } => "artifact file temporary path unavailable",
        SegmentFileError::FileTooLarge { .. } => "artifact file exceeds maximum segment size",
        SegmentFileError::Io { .. } => "artifact file could not be loaded",
        SegmentFileError::Format(_) => "artifact file is malformed",
    }
}
