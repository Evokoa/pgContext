//! SQL adapters for rebuildable segment artifact bytes.

use std::{
    collections::HashSet,
    fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
};

#[cfg(any(test, feature = "pg_test"))]
use std::sync::atomic::{AtomicU8, Ordering};

use context_index::{HnswGraph, HnswPointId};
use context_storage::{
    CURRENT_SEGMENT_FORMAT_VERSION, HnswGraphArtifactRecord, HnswGraphPayloadError, SegmentBytes,
    SegmentError, SegmentFileError, SegmentHeader, SegmentKind, SegmentWriteStage,
    decode_hnsw_graph_payload, encode_hnsw_graph_payload_v2, encode_segment, load_segment_file,
    validate_mmap_segment, write_segment_atomic_with_hook,
};
use pgrx::JsonB;
use pgrx::prelude::*;

use crate::domain_types::{
    ArtifactKind, ArtifactLifecycleState, artifact_kind_from_catalog,
    artifact_lifecycle_state_from_catalog,
};
use crate::error::{raise_core_error, raise_sql_error};

#[cfg(any(test, feature = "pg_test"))]
static ARTIFACT_PUBLISH_FAILPOINT: AtomicU8 = AtomicU8::new(0);

fn artifact_publish_failpoint(stage: u8, label: &'static str) {
    #[cfg(any(test, feature = "pg_test"))]
    if ARTIFACT_PUBLISH_FAILPOINT.load(Ordering::SeqCst) == stage {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("injected artifact publication failpoint: {label}"),
        );
    }
    let _ = (stage, label);
}

#[cfg(feature = "pg_test")]
#[pg_extern]
fn test_set_artifact_publish_failpoint(name: Option<String>) {
    let stage = match name.as_deref() {
        None => 0,
        Some("before_output_write") => 1,
        Some("before_file_fsync") => 2,
        Some("before_rename") => 3,
        Some("before_directory_fsync") => 4,
        Some("before_catalog_activate") => 5,
        Some("before_retire") => 6,
        Some(value) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("unknown artifact publication failpoint: {value}"),
        ),
    };
    ARTIFACT_PUBLISH_FAILPOINT.store(stage, Ordering::SeqCst);
}

mod diagnostics;
mod quantization;
mod serving_readiness;
pub(crate) use serving_readiness::with_mapped_artifact_payload;

type ArtifactSegmentResult = (
    i64,
    String,
    i64,
    String,
    String,
    String,
    String,
    i32,
    i64,
    i64,
    String,
);

type ArtifactSegmentFileResult = (
    i64,
    String,
    i64,
    String,
    String,
    String,
    String,
    i32,
    i64,
    i64,
    Option<String>,
    String,
);

type ArtifactSegmentMemoryResult = (String, String, String, String, i64, i64, i64, bool);
type ArtifactSegmentRetireResult = (
    i64,
    String,
    String,
    String,
    String,
    Option<String>,
    bool,
    String,
);
type ArtifactSegmentCleanupResult = (
    i64,
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    bool,
    String,
);

#[derive(Debug, Clone)]
struct ArtifactCollection {
    collection_id: i64,
    collection_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArtifactSegmentKind {
    HnswGraph,
}

impl ArtifactSegmentKind {
    fn from_sql(kind: &str) -> Self {
        match kind {
            "hnsw_graph" => Self::HnswGraph,
            _ => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!("unsupported segment kind: {kind}"),
            ),
        }
    }

    const fn storage_kind(self) -> SegmentKind {
        match self {
            Self::HnswGraph => SegmentKind::HnswGraph,
        }
    }

    const fn as_sql(self) -> &'static str {
        match self {
            Self::HnswGraph => "hnsw_graph",
        }
    }

    fn from_catalog(kind: String) -> Self {
        match kind.as_str() {
            "hnsw_graph" => Self::HnswGraph,
            _ => raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                format!("unexpected segment kind in catalog: {kind}"),
            ),
        }
    }
}

impl From<SegmentKind> for ArtifactSegmentKind {
    fn from(kind: SegmentKind) -> Self {
        match kind {
            SegmentKind::HnswGraph => Self::HnswGraph,
        }
    }
}

/// Encodes a rebuildable segment artifact with the storage header and checksum.
#[pg_extern(immutable, parallel_safe)]
pub fn encode_artifact_segment(kind: String, payload: Vec<u8>) -> Vec<u8> {
    let kind = ArtifactSegmentKind::from_sql(&kind);
    match encode_segment(kind.storage_kind(), &payload) {
        Ok(segment) => segment,
        Err(error) => raise_segment_error(error),
    }
}

/// Builds a deterministic mmap graph artifact from visible source rows.
#[pg_extern(volatile)]
#[search_path(pg_catalog, pgcontext)]
pub fn build_mmap_hnsw_artifact(build_job_id: i64) -> Vec<u8> {
    let job = resolve_visible_mmap_build_job(build_job_id);
    let collection_name = context_core::CollectionName::new(job.collection_name.clone())
        .unwrap_or_else(|error| raise_core_error(error));
    let mut registered =
        crate::table_search::resolve_registered_vector(&collection_name, job.collection_id);
    crate::table_search::validate_search_drift(job.collection_id, &mut registered);
    crate::table_search::require_table_select_privilege(&registered);
    let table = crate::table_search::quote_qualified_identifier(
        &registered.schema_name,
        &registered.table_name,
    );
    let column = crate::table_search::quote_identifier(&registered.vector_column_name);
    let sql = format!(
        "SELECT points.point_id, pgcontext.vector_to_real_array(source.{column})
           FROM pgcontext._visible_collection_points AS points
           JOIN {table} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $1 AND points.deleted_at IS NULL
          ORDER BY points.point_id"
    );
    let source_vectors = Spi::connect(|client| {
        let rows = client.select(&sql, None, &[job.collection_id.into()])?;
        let mut source_vectors = Vec::with_capacity(rows.len());
        for row in rows {
            let point_id = required_column(row.get::<i64>(1)?, "point_id");
            let values = required_column(row.get::<Vec<f32>>(2)?, "vector");
            let point_id = u64::try_from(point_id).map_err(|_| spi::Error::InvalidPosition)?;
            let vector =
                context_core::DenseVector::new(values).map_err(|_| spi::Error::InvalidPosition)?;
            source_vectors.push((point_id, vector));
        }
        Ok::<_, spi::Error>(source_vectors)
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("source artifact build failed: {error}"),
        )
    });
    let config = crate::settings::hnsw_config_from_gucs();
    let mut graph = HnswGraph::new(registered.metric, config);
    for (point_id, vector) in source_vectors {
        graph
            .insert(HnswPointId::new(point_id), vector)
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                    format!("source artifact HNSW build failed: {error}"),
                )
            });
    }
    let records = graph
        .node_snapshots()
        .into_iter()
        .map(|snapshot| {
            let node_id = u32::try_from(snapshot.node_id().get()).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "source artifact HNSW node id exceeds u32",
                )
            });
            let neighbors = snapshot
                .base_neighbors()
                .iter()
                .map(|neighbor| {
                    u32::try_from(neighbor.get()).unwrap_or_else(|_| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                            "source artifact HNSW neighbor id exceeds u32",
                        )
                    })
                })
                .collect();
            HnswGraphArtifactRecord::new(
                node_id,
                snapshot.point_id().get(),
                snapshot.vector().clone(),
                neighbors,
            )
        })
        .collect::<Vec<_>>();
    let quantization =
        quantization::quantize_graph_records(&records, &registered.quantization_options)
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                    format!("source artifact quantization is not buildable: {error}"),
                )
            });
    let payload =
        encode_hnsw_graph_payload_v2(&records, quantization.as_ref()).unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                format!("source artifact graph is not buildable: {error}"),
            )
        });
    encode_artifact_segment("hnsw_graph".to_owned(), payload)
}

/// Validates a rebuildable segment artifact without copying its payload bytes.
#[pg_extern(immutable, parallel_safe)]
pub fn validate_artifact_segment(
    segment: Vec<u8>,
) -> TableIterator<
    'static,
    (
        name!(kind, String),
        name!(payload_bytes, i64),
        name!(checksum, i64),
    ),
> {
    let view = match validate_mmap_segment(&segment) {
        Ok(view) => view,
        Err(error) => raise_segment_error(error),
    };
    let kind = ArtifactSegmentKind::from(view.header().kind())
        .as_sql()
        .to_owned();
    let payload_bytes = i64::try_from(view.payload().len()).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "segment payload length exceeds PostgreSQL bigint range",
        )
    });
    let checksum = i64::from_ne_bytes(view.header().checksum().to_ne_bytes());

    TableIterator::once((kind, payload_bytes, checksum))
}

/// Validates a rebuildable HNSW graph artifact segment and its portable payload.
#[pg_extern(immutable, parallel_safe)]
pub fn validate_hnsw_graph_artifact(
    segment: Vec<u8>,
) -> TableIterator<
    'static,
    (
        name!(record_count, i64),
        name!(dimensions, i32),
        name!(base_neighbor_count, i64),
    ),
> {
    let view = match validate_mmap_segment(&segment) {
        Ok(view) => view,
        Err(error) => raise_segment_error(error),
    };
    if view.header().kind() != SegmentKind::HnswGraph {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "artifact segment is not an HNSW graph",
        );
    }
    let records = match decode_hnsw_graph_payload(view.payload()) {
        Ok(records) => records,
        Err(error) => raise_hnsw_graph_payload_error(error),
    };
    let record_count = i64::try_from(records.len()).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "HNSW graph record count exceeds PostgreSQL bigint range",
        )
    });
    let dimensions = i32::try_from(records[0].vector().dimension()).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "HNSW graph dimensions exceed PostgreSQL integer range",
        )
    });
    let base_neighbor_count = records
        .iter()
        .try_fold(0_i64, |total, record| {
            let neighbors = i64::try_from(record.base_neighbors().len()).ok()?;
            total.checked_add(neighbors)
        })
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "HNSW graph neighbor count exceeds PostgreSQL bigint range",
            )
        });

    TableIterator::once((record_count, dimensions, base_neighbor_count))
}

#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn publish_artifact_segment(
    build_job_id: i64,
    segment: Vec<u8>,
) -> TableIterator<
    'static,
    (
        name!(artifact_id, i64),
        name!(collection_name, String),
        name!(build_job_id, i64),
        name!(artifact_kind, String),
        name!(artifact_name, String),
        name!(target_name, String),
        name!(segment_kind, String),
        name!(format_version, i32),
        name!(payload_bytes, i64),
        name!(checksum, i64),
        name!(lifecycle_state, String),
    ),
> {
    let job = resolve_publishable_build_job(build_job_id);
    let validated = validated_segment(&segment);
    validate_artifact_payload_policy(&job, &validated);
    let metadata = validated.metadata;
    lock_artifact_publish_target(&job);
    let generation = next_artifact_generation(&job);
    let artifact_id = insert_artifact_segment(&job, generation, &metadata, None);
    TableIterator::once(artifact_segment_result(resolve_visible_artifact_segment(
        artifact_id,
    )))
}

#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(security_definer, volatile)]
#[search_path(pg_catalog, pgcontext)]
pub fn publish_artifact_segment_file(
    build_job_id: i64,
    segment: Vec<u8>,
) -> TableIterator<
    'static,
    (
        name!(artifact_id, i64),
        name!(collection_name, String),
        name!(build_job_id, i64),
        name!(artifact_kind, String),
        name!(artifact_name, String),
        name!(target_name, String),
        name!(segment_kind, String),
        name!(format_version, i32),
        name!(payload_bytes, i64),
        name!(checksum, i64),
        name!(relative_path, Option<String>),
        name!(lifecycle_state, String),
    ),
> {
    let job = resolve_publishable_build_job(build_job_id);
    let validated = validated_segment(&segment);
    validate_artifact_payload_policy(&job, &validated);
    replay_build_delta_tail(build_job_id);
    lock_artifact_publish_target(&job);
    let generation = next_artifact_generation(&job);
    let relative_path = artifact_relative_path(&job, generation);

    let destination = artifact_absolute_path(&relative_path);
    ensure_artifact_parent(&destination);
    match write_segment_atomic_with_hook(
        &destination,
        validated.storage_kind,
        validated.payload,
        |stage| {
            match stage {
                SegmentWriteStage::BeforeWrite => {
                    artifact_publish_failpoint(1, "before_output_write")
                }
                SegmentWriteStage::BeforeFileSync => {
                    artifact_publish_failpoint(2, "before_file_fsync")
                }
                SegmentWriteStage::BeforeRename => artifact_publish_failpoint(3, "before_rename"),
                SegmentWriteStage::BeforeDirectorySync => {
                    artifact_publish_failpoint(4, "before_directory_fsync")
                }
            }
            Ok(())
        },
    ) {
        Ok(_segment) => {}
        Err(error) => raise_segment_file_error(error),
    }
    artifact_publish_failpoint(5, "before_catalog_activate");
    let artifact_id =
        insert_artifact_segment(&job, generation, &validated.metadata, Some(&relative_path));
    artifact_publish_failpoint(6, "before_retire");
    retire_superseded_artifact_generations(&job, artifact_id);

    TableIterator::once(artifact_segment_file_result(
        resolve_visible_artifact_segment(artifact_id),
    ))
}

fn validate_artifact_payload_policy(job: &PublishableBuildJob, segment: &ValidatedSegment<'_>) {
    if job.artifact_kind != ArtifactKind::Mmap || segment.storage_kind != SegmentKind::HnswGraph {
        return;
    }
    let quantization_options = Spi::connect(|client| {
        let rows = client.select(
            "SELECT vectors.quantization_options
               FROM pgcontext._visible_collection_vectors AS vectors
              WHERE vectors.collection_id = $1
              ORDER BY vectors.vector_id",
            Some(1),
            &[job.collection_id.into()],
        )?;
        if rows.is_empty() {
            Ok::<_, spi::Error>(None)
        } else {
            Ok(Some(required_column(
                rows.first().get::<JsonB>(1)?,
                "quantization_options",
            )))
        }
    })
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("artifact quantization policy lookup failed: {error}"),
        )
    });
    let Some(quantization_options) = quantization_options else {
        return;
    };
    let quantization_options = quantization_options.0;
    let configured_mode = quantization_options
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("none");
    let graph = match context_storage::decode_hnsw_graph_payload_versioned(segment.payload) {
        Ok(graph) => graph,
        Err(_) if configured_mode == "none" => return,
        Err(error) => raise_hnsw_graph_payload_error(error),
    };
    if let Err(error) = quantization::validate_graph_quantization_policy(
        graph.records(),
        graph.quantization(),
        &quantization_options,
    ) {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("artifact quantization policy mismatch: {error}"),
        );
    }
}

fn replay_build_delta_tail(build_job_id: i64) {
    // No table lock. This used to take SHARE ROW EXCLUSIVE on
    // `_collection_points` to quiesce writers around the delete, but any
    // transaction that touched points earlier (an upsert followed by a
    // publish — the natural client sequence) already holds ROW EXCLUSIVE
    // there, and two such transactions requesting the self-conflicting
    // upgrade deadlock against each other reliably.
    //
    // The lock also bought nothing: the published payload is built outside
    // this function (passed in, or scanned in an earlier statement), so no
    // table lock here can make payload and delta tail consistent. The
    // DELETE's own snapshot is the boundary — a delta committed before it is
    // declared subsumed by the incoming payload, one committed after
    // survives for the query-time merge, and a delta that ends up both
    // folded and surviving is deduplicated by point id during candidate
    // assembly.
    Spi::run_with_args(
        "DELETE FROM pgcontext._build_deltas WHERE build_job_id = $1",
        &[build_job_id.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("build delta tail replay failed: {error}"),
        )
    });
}

#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "artifact_segments", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn list_artifact_segments(
    collection: String,
) -> TableIterator<
    'static,
    (
        name!(artifact_id, i64),
        name!(collection_name, String),
        name!(build_job_id, i64),
        name!(artifact_kind, String),
        name!(artifact_name, String),
        name!(target_name, String),
        name!(segment_kind, String),
        name!(format_version, i32),
        name!(payload_bytes, i64),
        name!(checksum, i64),
        name!(relative_path, Option<String>),
        name!(lifecycle_state, String),
    ),
> {
    TableIterator::new(
        select_artifact_segments(&collection)
            .into_iter()
            .map(artifact_segment_file_result),
    )
}

#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "artifact_segment_memory", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn artifact_segment_memory(
    collection: String,
) -> TableIterator<
    'static,
    (
        name!(artifact_kind, String),
        name!(artifact_name, String),
        name!(target_name, String),
        name!(lifecycle_state, String),
        name!(payload_bytes, i64),
        name!(header_bytes, i64),
        name!(mapped_bytes, i64),
        name!(file_materialized, bool),
    ),
> {
    TableIterator::new(
        select_artifact_segments(&collection)
            .into_iter()
            .map(artifact_segment_memory_result),
    )
}

#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "retire_artifact_segment", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn retire_artifact_segment(
    artifact_id: i64,
) -> TableIterator<
    'static,
    (
        name!(artifact_id, i64),
        name!(collection_name, String),
        name!(artifact_kind, String),
        name!(artifact_name, String),
        name!(target_name, String),
        name!(previous_relative_path, Option<String>),
        name!(file_removed, bool),
        name!(lifecycle_state, String),
    ),
> {
    let current = resolve_visible_artifact_segment(artifact_id);
    lock_artifact_segment_target(&current);
    let current = resolve_visible_artifact_segment(artifact_id);
    let previous_relative_path = current.relative_path.clone();
    if let Some(relative_path) = previous_relative_path.as_deref()
        && !artifact_relative_path_is_confined(relative_path)
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "artifact relative path is outside pgcontext_artifacts",
        );
    }
    let retired = retire_artifact_segment_row(artifact_id);
    // A later cleanup transaction reclaims the path only after this state
    // transition has committed and all durable reader pins are gone.
    let file_removed = false;

    TableIterator::once(artifact_segment_retire_result(
        retired,
        previous_relative_path,
        file_removed,
    ))
}

#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "cleanup_artifact_segments", security_definer, volatile)]
#[search_path(pg_catalog, pgcontext)]
pub fn cleanup_artifact_segments(
    collection: String,
    dry_run: bool,
) -> TableIterator<
    'static,
    (
        name!(artifact_id, i64),
        name!(collection_name, String),
        name!(artifact_kind, String),
        name!(artifact_name, String),
        name!(target_name, String),
        name!(status, String),
        name!(cleanup_action, String),
        name!(relative_path, Option<String>),
        name!(file_removed, bool),
        name!(lifecycle_state, String),
    ),
> {
    let Some(collection) = resolve_visible_artifact_collection(&collection) else {
        return TableIterator::new(Vec::<ArtifactSegmentCleanupResult>::new());
    };
    let rows = select_artifact_segments_by_collection_id(collection.collection_id)
        .into_iter()
        .collect::<Vec<_>>();
    prevalidate_artifact_cleanup_paths(&rows);

    let referenced_paths = artifact_manifest_paths(&rows);
    let mut rows = rows
        .into_iter()
        .filter_map(|row| cleanup_artifact_segment(row, dry_run))
        .collect::<Vec<_>>();
    rows.extend(cleanup_orphan_artifact_files(
        &collection,
        &referenced_paths,
        dry_run,
    ));
    TableIterator::new(rows)
}

#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "artifact_segment_diagnostics", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn artifact_segment_diagnostics(
    collection: String,
) -> TableIterator<
    'static,
    (
        name!(artifact_kind, String),
        name!(artifact_name, String),
        name!(target_name, String),
        name!(lifecycle_state, String),
        name!(status, String),
        name!(detail, String),
        name!(repair_advice, String),
        name!(cleanup_eligible, bool),
        name!(relative_path, Option<String>),
        name!(payload_bytes, i64),
        name!(file_payload_bytes, Option<i64>),
        name!(checksum, i64),
        name!(file_checksum, Option<i64>),
    ),
> {
    TableIterator::new(
        select_artifact_segments(&collection)
            .into_iter()
            .map(diagnostics::artifact_segment_diagnostic_result),
    )
}

include!("artifact_segments_results.rs");

include!("artifact_segments_file_cleanup.rs");

include!("artifact_segments/persistence.rs");
#[cfg(test)]
mod tests {
    use super::{
        ArtifactKind, ArtifactLifecycleState, ArtifactSegmentKind, ArtifactSegmentRow,
        artifact_cleanup_snapshot_matches,
    };

    #[test]
    fn cleanup_snapshot_rejects_republished_manifest() {
        let stale = artifact_row("pgcontext_artifacts/1/10_mmap.pgctxseg", 10, 7, 42);
        let republished = artifact_row("pgcontext_artifacts/1/11_mmap.pgctxseg", 11, 9, 99);

        assert!(!artifact_cleanup_snapshot_matches(&stale, &republished));
    }

    #[test]
    fn cleanup_snapshot_accepts_unchanged_manifest() {
        let current = artifact_row("pgcontext_artifacts/1/10_mmap.pgctxseg", 10, 7, 42);

        assert!(artifact_cleanup_snapshot_matches(&current, &current));
    }

    fn artifact_row(
        relative_path: &str,
        build_job_id: i64,
        payload_bytes: i64,
        checksum: i64,
    ) -> ArtifactSegmentRow {
        ArtifactSegmentRow {
            artifact_id: 1,
            collection_id: 1,
            collection_name: "collection".to_owned(),
            build_job_id,
            artifact_kind: ArtifactKind::Mmap,
            artifact_name: "view-a".to_owned(),
            target_name: "public.collection".to_owned(),
            generation: build_job_id,
            segment_kind: ArtifactSegmentKind::HnswGraph,
            format_version: 1,
            payload_bytes,
            checksum,
            config_revision: Some(1),
            relative_path: Some(relative_path.to_owned()),
            lifecycle_state: ArtifactLifecycleState::FileMaterialized,
        }
    }
}
