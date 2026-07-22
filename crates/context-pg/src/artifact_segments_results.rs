// Catalog result shapes and conversions included by `artifact_segments.rs`.
// These private DTOs keep PostgreSQL tuple contracts out of context-storage.

#[derive(Debug, Clone)]
struct PublishableBuildJob {
    build_job_id: i64,
    collection_id: i64,
    collection_name: String,
    artifact_kind: ArtifactKind,
    artifact_name: String,
    target_name: String,
    config_revision: Option<i64>,
}

#[derive(Debug, Clone)]
struct ValidatedSegmentMetadata {
    segment_kind: ArtifactSegmentKind,
    format_version: i32,
    payload_bytes: i64,
    checksum: i64,
}

#[derive(Debug, Clone)]
struct ArtifactSegmentRow {
    artifact_id: i64,
    collection_id: i64,
    collection_name: String,
    build_job_id: i64,
    artifact_kind: ArtifactKind,
    artifact_name: String,
    target_name: String,
    generation: i64,
    segment_kind: ArtifactSegmentKind,
    format_version: i32,
    payload_bytes: i64,
    checksum: i64,
    config_revision: Option<i64>,
    relative_path: Option<String>,
    lifecycle_state: ArtifactLifecycleState,
}

fn artifact_segment_result(row: ArtifactSegmentRow) -> ArtifactSegmentResult {
    let (
        artifact_id,
        collection_name,
        build_job_id,
        artifact_kind,
        artifact_name,
        target_name,
        segment_kind,
        format_version,
        payload_bytes,
        checksum,
        _relative_path,
        lifecycle_state,
    ) = artifact_segment_file_result(row);
    (
        artifact_id,
        collection_name,
        build_job_id,
        artifact_kind,
        artifact_name,
        target_name,
        segment_kind,
        format_version,
        payload_bytes,
        checksum,
        lifecycle_state,
    )
}

fn artifact_segment_file_result(row: ArtifactSegmentRow) -> ArtifactSegmentFileResult {
    (
        row.artifact_id,
        row.collection_name,
        row.build_job_id,
        row.artifact_kind.as_sql().to_owned(),
        row.artifact_name,
        row.target_name,
        row.segment_kind.as_sql().to_owned(),
        row.format_version,
        row.payload_bytes,
        row.checksum,
        row.relative_path,
        row.lifecycle_state.as_sql().to_owned(),
    )
}

fn artifact_segment_memory_result(row: ArtifactSegmentRow) -> ArtifactSegmentMemoryResult {
    let header_bytes = i64::try_from(SegmentHeader::ENCODED_LEN).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "segment header length exceeds PostgreSQL bigint range",
        )
    });
    let mapped_bytes = row
        .payload_bytes
        .checked_add(header_bytes)
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "artifact mapped byte estimate exceeds PostgreSQL bigint range",
            )
        });
    (
        row.artifact_kind.as_sql().to_owned(),
        row.artifact_name,
        row.target_name,
        row.lifecycle_state.as_sql().to_owned(),
        row.payload_bytes,
        header_bytes,
        mapped_bytes,
        row.relative_path.is_some(),
    )
}

fn artifact_segment_retire_result(
    row: ArtifactSegmentRow,
    previous_relative_path: Option<String>,
    file_removed: bool,
) -> ArtifactSegmentRetireResult {
    (
        row.artifact_id,
        row.collection_name,
        row.artifact_kind.as_sql().to_owned(),
        row.artifact_name,
        row.target_name,
        previous_relative_path,
        file_removed,
        row.lifecycle_state.as_sql().to_owned(),
    )
}

fn artifact_segment_cleanup_result(
    row: ArtifactSegmentRow,
    status: &str,
    cleanup_action: &str,
    previous_relative_path: Option<String>,
    file_removed: bool,
) -> ArtifactSegmentCleanupResult {
    (
        row.artifact_id,
        row.collection_name,
        row.artifact_kind.as_sql().to_owned(),
        row.artifact_name,
        row.target_name,
        status.to_owned(),
        cleanup_action.to_owned(),
        previous_relative_path,
        file_removed,
        row.lifecycle_state.as_sql().to_owned(),
    )
}

struct ValidatedSegment<'a> {
    metadata: ValidatedSegmentMetadata,
    storage_kind: SegmentKind,
    payload: &'a [u8],
}

fn validated_segment(segment: &[u8]) -> ValidatedSegment<'_> {
    let view = match validate_mmap_segment(segment) {
        Ok(view) => view,
        Err(error) => raise_segment_error(error),
    };
    ValidatedSegment {
        metadata: ValidatedSegmentMetadata {
            segment_kind: ArtifactSegmentKind::from(view.header().kind()),
            format_version: i32::try_from(CURRENT_SEGMENT_FORMAT_VERSION).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "segment format version exceeds PostgreSQL integer range",
                )
            }),
            payload_bytes: i64::try_from(view.payload().len()).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "segment payload length exceeds PostgreSQL bigint range",
                )
            }),
            checksum: i64::from_ne_bytes(view.header().checksum().to_ne_bytes()),
        },
        storage_kind: view.header().kind(),
        payload: view.payload(),
    }
}
