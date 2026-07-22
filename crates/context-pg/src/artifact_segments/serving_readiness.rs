use std::io::ErrorKind;

use context_storage::{
    MappedSegment, SegmentError, SegmentFileError, SegmentHeader, map_segment_file,
};
use pgrx::prelude::*;

use crate::domain_types::{ArtifactKind, ArtifactLifecycleState};

use super::{
    ArtifactSegmentKind, ArtifactSegmentRow, artifact_absolute_path,
    artifact_relative_path_is_confined, lock_artifact_segment_target_shared,
    select_artifact_segments,
};
use crate::error::raise_sql_error;

type ArtifactSegmentServingReadinessResult = (
    String,
    String,
    String,
    String,
    String,
    bool,
    i64,
    i64,
    String,
);

type ArtifactSegmentMmapPayloadResult = (String, String, String, i64, Vec<u8>);

struct LoadedServingSegment {
    mapped_bytes: i64,
    segment: MappedSegment,
}

struct ServingReadinessFailure {
    status: String,
    mapped_bytes: i64,
    detail: String,
}

struct ArtifactReaderPin {
    artifact_id: i64,
    backend_pid: i32,
    backend_identity: String,
}

#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(
    schema = "pgcontext",
    name = "artifact_segment_serving_readiness",
    security_definer
)]
#[search_path(pg_catalog, pgcontext)]
pub fn artifact_segment_serving_readiness(
    collection: String,
    max_mapped_bytes: i64,
) -> TableIterator<
    'static,
    (
        name!(artifact_kind, String),
        name!(artifact_name, String),
        name!(target_name, String),
        name!(lifecycle_state, String),
        name!(status, String),
        name!(serving_ready, bool),
        name!(mapped_bytes, i64),
        name!(max_mapped_bytes, i64),
        name!(detail, String),
    ),
> {
    validate_non_negative_budget(max_mapped_bytes);
    TableIterator::new(
        select_artifact_segments(&collection)
            .into_iter()
            .map(move |row| serving_readiness_result(row, max_mapped_bytes)),
    )
}

#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(
    schema = "pgcontext",
    name = "artifact_segment_mmap_payload",
    security_definer
)]
#[search_path(pg_catalog, pgcontext)]
pub fn artifact_segment_mmap_payload(
    collection: String,
    artifact_name: String,
    max_mapped_bytes: i64,
) -> TableIterator<
    'static,
    (
        name!(artifact_kind, String),
        name!(artifact_name, String),
        name!(target_name, String),
        name!(mapped_bytes, i64),
        name!(payload, Vec<u8>),
    ),
> {
    let session_is_superuser = Spi::get_one::<bool>(
        "SELECT roles.rolsuper
           FROM pg_catalog.pg_roles AS roles
          WHERE roles.rolname = SESSION_USER",
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check mmap payload caller: {error}"),
        )
    })
    .unwrap_or(false);
    if !session_is_superuser {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "raw mmap artifact payload access is internal",
        );
    }
    validate_non_negative_budget(max_mapped_bytes);
    let row = find_mmap_artifact_row(&collection, &artifact_name);
    lock_artifact_segment_target_shared(&row);
    let row = find_mmap_artifact_row(&collection, &artifact_name);
    let _pin = acquire_artifact_reader_pin(&row);
    let loaded = match load_serving_ready_segment(&row, max_mapped_bytes) {
        Ok(loaded) => loaded,
        Err(failure) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!(
                "mmap artifact is not serving-ready: {} ({})",
                failure.status, failure.detail
            ),
        ),
    };
    let result = mmap_payload_row(row, loaded);
    TableIterator::once(result)
}

/// Runs an internal operation while a serving-ready artifact is mapped and
/// durably pinned to the current backend.
///
/// The caller must enter through a SECURITY DEFINER SQL boundary that has
/// re-derived collection membership. No payload borrow can outlive `action`,
/// and the reader pin is released only after every borrow has ended.
pub(crate) fn with_mapped_artifact_payload<R>(
    collection: &str,
    artifact_name: &str,
    max_mapped_bytes: i64,
    action: impl FnOnce(&[u8]) -> R,
) -> R {
    validate_non_negative_budget(max_mapped_bytes);
    let row = find_mmap_artifact_row(collection, artifact_name);
    lock_artifact_segment_target_shared(&row);
    let row = find_mmap_artifact_row(collection, artifact_name);
    let _pin = acquire_artifact_reader_pin(&row);
    let loaded = match load_serving_ready_segment(&row, max_mapped_bytes) {
        Ok(loaded) => loaded,
        Err(failure) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!(
                "mmap artifact is not serving-ready: {} ({})",
                failure.status, failure.detail
            ),
        ),
    };
    action(loaded.segment.payload())
}

fn find_mmap_artifact_row(collection: &str, artifact_name: &str) -> ArtifactSegmentRow {
    select_artifact_segments(collection)
        .into_iter()
        .filter(|row| {
            row.artifact_kind == ArtifactKind::Mmap
                && row.artifact_name == artifact_name
                && row.lifecycle_state == ArtifactLifecycleState::FileMaterialized
                && artifact_config_revision_is_current(row)
        })
        .max_by_key(|row| row.generation)
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                format!("serving-ready mmap artifact not found: {collection}/{artifact_name}"),
            )
        })
}

fn artifact_config_revision_is_current(row: &ArtifactSegmentRow) -> bool {
    let current = Spi::get_one_with_args::<i64>(
        "SELECT pgcontext.current_vector_config_revision($1)",
        &[row.collection_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact configuration revision lookup failed: {error}"),
        )
    });
    row.config_revision == current
}

fn acquire_artifact_reader_pin(row: &ArtifactSegmentRow) -> ArtifactReaderPin {
    let (backend_pid, backend_identity) = current_backend_identity();
    Spi::run_with_args(
        "INSERT INTO pgcontext._artifact_reader_pins (
             artifact_id, backend_pid, backend_identity, pin_count
         ) VALUES ($1, $2, $3, 1)
         ON CONFLICT (artifact_id, backend_pid, backend_identity)
         DO UPDATE SET pin_count = pgcontext._artifact_reader_pins.pin_count + 1,
                       updated_at = pg_catalog.now()",
        &[
            row.artifact_id.into(),
            backend_pid.into(),
            backend_identity.as_str().into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact reader pin acquisition failed: {error}"),
        )
    });
    ArtifactReaderPin {
        artifact_id: row.artifact_id,
        backend_pid,
        backend_identity,
    }
}

fn release_artifact_reader_pin(pin: &ArtifactReaderPin) {
    let delete_result = Spi::run_with_args(
        "DELETE FROM pgcontext._artifact_reader_pins
          WHERE artifact_id = $1
            AND backend_pid = $2
            AND backend_identity = $3
            AND pin_count = 1",
        &[
            pin.artifact_id.into(),
            pin.backend_pid.into(),
            pin.backend_identity.as_str().into(),
        ],
    );
    if delete_result.is_err() {
        return;
    }
    let _ = Spi::run_with_args(
        "UPDATE pgcontext._artifact_reader_pins
            SET pin_count = pin_count - 1, updated_at = pg_catalog.now()
          WHERE artifact_id = $1
            AND backend_pid = $2
            AND backend_identity = $3
            AND pin_count > 1",
        &[
            pin.artifact_id.into(),
            pin.backend_pid.into(),
            pin.backend_identity.as_str().into(),
        ],
    );
}

impl Drop for ArtifactReaderPin {
    fn drop(&mut self) {
        release_artifact_reader_pin(self);
    }
}

fn current_backend_identity() -> (i32, String) {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT pid, backend_start::text
               FROM pg_catalog.pg_stat_activity
              WHERE pid = pg_catalog.pg_backend_pid()",
            Some(1),
            &[],
        )?;
        let row = rows.first();
        Ok::<_, spi::Error>((
            required_column(row.get::<i32>(1)?, "backend_pid"),
            required_column(row.get::<String>(2)?, "backend_identity"),
        ))
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact reader backend identity query failed: {error}"),
        )
    })
}

fn required_column<T>(value: Option<T>, column: &'static str) -> T {
    value.unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact reader query returned null {column}"),
        )
    })
}

fn serving_readiness_result(
    row: ArtifactSegmentRow,
    max_mapped_bytes: i64,
) -> ArtifactSegmentServingReadinessResult {
    match load_serving_ready_segment(&row, max_mapped_bytes) {
        Ok(loaded) => readiness_row(
            row,
            "ready",
            true,
            loaded.mapped_bytes,
            max_mapped_bytes,
            "artifact file matches catalog metadata and memory budget",
        ),
        Err(failure) => readiness_row(
            row,
            failure.status,
            false,
            failure.mapped_bytes,
            max_mapped_bytes,
            failure.detail,
        ),
    }
}

fn load_serving_ready_segment(
    row: &ArtifactSegmentRow,
    max_mapped_bytes: i64,
) -> Result<LoadedServingSegment, ServingReadinessFailure> {
    let catalog_mapped_bytes = mapped_bytes(row.payload_bytes);
    if row.artifact_kind != ArtifactKind::Mmap {
        return Err(readiness_failure(
            "not_mmap_artifact",
            catalog_mapped_bytes,
            "only mmap artifacts can be serving-ready",
        ));
    }
    if row.lifecycle_state != ArtifactLifecycleState::FileMaterialized {
        return Err(readiness_failure(
            "not_file_materialized",
            catalog_mapped_bytes,
            "mmap serving requires a file-materialized artifact",
        ));
    }
    let Some(relative_path) = row.relative_path.as_deref() else {
        return Err(readiness_failure(
            "not_file_materialized",
            catalog_mapped_bytes,
            "mmap serving requires a materialized file path",
        ));
    };
    if !artifact_relative_path_is_confined(relative_path) {
        return Err(readiness_failure(
            "path_rejected",
            catalog_mapped_bytes,
            "artifact relative path is outside pgcontext_artifacts",
        ));
    }

    let path = artifact_absolute_path(relative_path);
    let segment = match map_segment_file(&path) {
        Ok(segment) => segment,
        Err(SegmentFileError::Io {
            operation: "open",
            source,
            ..
        }) if source.kind() == ErrorKind::NotFound => {
            return Err(readiness_failure(
                "artifact_missing",
                catalog_mapped_bytes,
                "artifact file is missing",
            ));
        }
        Err(SegmentFileError::Format(SegmentError::ChecksumMismatch)) => {
            return Err(readiness_failure(
                "checksum_mismatch",
                catalog_mapped_bytes,
                "segment checksum mismatch",
            ));
        }
        Err(error) => {
            return Err(readiness_failure(
                "artifact_corrupt",
                catalog_mapped_bytes,
                file_error_detail(&error),
            ));
        }
    };

    let file_mapped_bytes = mapped_bytes(file_payload_bytes(&segment));
    if !metadata_matches(row, &segment) {
        return Err(readiness_failure(
            "metadata_mismatch",
            file_mapped_bytes,
            "artifact file metadata differs from catalog",
        ));
    }
    if file_mapped_bytes > max_mapped_bytes {
        return Err(readiness_failure(
            "memory_budget_exceeded",
            file_mapped_bytes,
            "artifact mapped bytes exceed the serving memory budget",
        ));
    }

    Ok(LoadedServingSegment {
        mapped_bytes: file_mapped_bytes,
        segment,
    })
}

fn readiness_failure(
    status: impl Into<String>,
    mapped_bytes: i64,
    detail: impl Into<String>,
) -> ServingReadinessFailure {
    ServingReadinessFailure {
        status: status.into(),
        mapped_bytes,
        detail: detail.into(),
    }
}

fn mmap_payload_row(
    row: ArtifactSegmentRow,
    loaded: LoadedServingSegment,
) -> ArtifactSegmentMmapPayloadResult {
    (
        row.artifact_kind.as_sql().to_owned(),
        row.artifact_name,
        row.target_name,
        loaded.mapped_bytes,
        loaded.segment.payload().to_vec(),
    )
}

fn readiness_row(
    row: ArtifactSegmentRow,
    status: impl Into<String>,
    serving_ready: bool,
    mapped_bytes: i64,
    max_mapped_bytes: i64,
    detail: impl Into<String>,
) -> ArtifactSegmentServingReadinessResult {
    (
        row.artifact_kind.as_sql().to_owned(),
        row.artifact_name,
        row.target_name,
        row.lifecycle_state.as_sql().to_owned(),
        status.into(),
        serving_ready,
        mapped_bytes,
        max_mapped_bytes,
        detail.into(),
    )
}

fn metadata_matches(row: &ArtifactSegmentRow, segment: &MappedSegment) -> bool {
    let file_kind = ArtifactSegmentKind::from(segment.header().kind());
    let file_format_version =
        i32::try_from(segment.header().version().as_u32()).unwrap_or_else(|_| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "segment format version exceeds PostgreSQL integer range",
            )
        });
    let file_checksum = i64::from_ne_bytes(segment.header().checksum().to_ne_bytes());

    row.segment_kind == file_kind
        && row.format_version == file_format_version
        && row.payload_bytes == file_payload_bytes(segment)
        && row.checksum == file_checksum
}

fn file_payload_bytes(segment: &MappedSegment) -> i64 {
    i64::try_from(segment.payload().len()).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "segment payload length exceeds PostgreSQL bigint range",
        )
    })
}

fn mapped_bytes(payload_bytes: i64) -> i64 {
    payload_bytes
        .checked_add(
            i64::try_from(SegmentHeader::ENCODED_LEN).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "segment header length exceeds PostgreSQL bigint range",
                )
            }),
        )
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "artifact mapped byte estimate exceeds PostgreSQL bigint range",
            )
        })
}

fn validate_non_negative_budget(max_mapped_bytes: i64) {
    if max_mapped_bytes < 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("max_mapped_bytes must be non-negative: {max_mapped_bytes}"),
        );
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
