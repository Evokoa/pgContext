//! SQL-facing backend-local build-job metadata.

mod backend_identity;
mod build_job_types;
mod build_job_validation;

use pgrx::{prelude::*, spi};

#[cfg(any(test, feature = "pg_test"))]
use std::sync::atomic::{AtomicU8, Ordering};

use crate::domain_types::{ArtifactKind, artifact_kind_from_catalog, artifact_kind_from_sql};
use crate::error::raise_sql_error;
use crate::pgcontext::BuildJobStatus;
use backend_identity::{backend_is_active, current_backend_identity};
use build_job_types::{ActiveBuildJobCandidate, BuildJobRow};
use build_job_validation::{
    build_generation_kind, build_status_from_catalog, ensure_build_runner_supported,
    parse_build_status_command, validate_non_empty_text, validate_non_negative_units,
    validate_positive_units,
};

type BuildJobResult = (
    i64,
    String,
    String,
    String,
    String,
    BuildJobStatus,
    Option<i32>,
    i32,
    i64,
    i64,
    bool,
    Option<String>,
);

#[cfg(any(test, feature = "pg_test"))]
static BUILD_JOB_FAILPOINT: AtomicU8 = AtomicU8::new(0);

#[cfg(feature = "pg_test")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildJobFailpoint {
    Checkpoint = 1,
    SourceRead = 2,
    Cancel = 3,
    BackendLossRecovery = 4,
    Retry = 5,
    SchemaDriftValidation = 6,
}

fn build_job_failpoint(stage: u8, label: &'static str) {
    #[cfg(any(test, feature = "pg_test"))]
    if BUILD_JOB_FAILPOINT.load(Ordering::SeqCst) == stage {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("injected build job failpoint: {label}"),
        );
    }
    let _ = (stage, label);
}

#[cfg(feature = "pg_test")]
#[pg_extern]
fn test_set_build_job_failpoint(name: Option<String>) {
    let failpoint = match name.as_deref() {
        None => 0,
        Some("before_checkpoint") => BuildJobFailpoint::Checkpoint as u8,
        Some("before_source_read") => BuildJobFailpoint::SourceRead as u8,
        Some("before_cancel") => BuildJobFailpoint::Cancel as u8,
        Some("before_backend_loss_recovery") => BuildJobFailpoint::BackendLossRecovery as u8,
        Some("before_retry") => BuildJobFailpoint::Retry as u8,
        Some("before_schema_drift_validation") => BuildJobFailpoint::SchemaDriftValidation as u8,
        Some(value) => raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("unknown build job failpoint: {value}"),
        ),
    };
    BUILD_JOB_FAILPOINT.store(failpoint, Ordering::SeqCst);
}

/// Starts backend-local metadata for a pgContext artifact or projection.
///
/// This function does not make native PostgreSQL `CREATE INDEX` resumable and
/// does not spawn a Rust worker. It records ownership, progress budget, and the
/// current PostgreSQL backend identity for a pgContext-owned generation so the
/// caller can execute it synchronously or retry it from another backend after
/// failure.
///
/// # Errors
///
/// Raises `undefined_object` when `collection` does not exist. Raises
/// `insufficient_privilege` when the caller is not a collection owner member.
/// Raises `invalid_parameter_value` for unsupported build kinds, empty names,
/// or negative totals.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "start_build_job", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn start_build_job(
    collection: String,
    artifact_kind: String,
    artifact_name: String,
    target_name: String,
    total_units: i64,
) -> TableIterator<
    'static,
    (
        name!(build_job_id, i64),
        name!(collection_name, String),
        name!(artifact_kind, String),
        name!(artifact_name, String),
        name!(target_name, String),
        name!(status, BuildJobStatus),
        name!(backend_pid, Option<i32>),
        name!(attempt, i32),
        name!(processed_units, i64),
        name!(total_units, i64),
        name!(cancel_requested, bool),
        name!(error_message, Option<String>),
    ),
> {
    let artifact_kind = artifact_kind_from_sql(&artifact_kind);
    let _generation_kind = build_generation_kind(artifact_kind);
    validate_non_empty_text(&artifact_name, "artifact_name");
    validate_non_empty_text(&target_name, "target_name");
    validate_non_negative_units(total_units, "total_units");

    let collection_id = resolve_owned_collection_id(&collection);
    let row = insert_build_job(
        collection_id,
        artifact_kind,
        &artifact_name,
        &target_name,
        total_units,
    );
    TableIterator::once(build_job_result(row))
}

/// Lists build jobs for a collection owned by the caller.
///
/// Running jobs whose backend identity no longer appears in `pg_stat_activity`
/// are reported as [`BuildJobStatus::Abandoned`] so retry decisions do not
/// depend on in-process Rust state.
///
/// # Errors
///
/// Raises `undefined_object` when `collection` does not exist. Raises
/// `insufficient_privilege` when the caller is not a collection owner member.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "build_jobs", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn build_jobs(
    collection: String,
) -> TableIterator<
    'static,
    (
        name!(build_job_id, i64),
        name!(collection_name, String),
        name!(artifact_kind, String),
        name!(artifact_name, String),
        name!(target_name, String),
        name!(status, BuildJobStatus),
        name!(backend_pid, Option<i32>),
        name!(attempt, i32),
        name!(processed_units, i64),
        name!(total_units, i64),
        name!(cancel_requested, bool),
        name!(error_message, Option<String>),
    ),
> {
    let collection_id = resolve_owned_collection_id(&collection);
    TableIterator::new(
        select_build_jobs(collection_id)
            .into_iter()
            .map(build_job_result),
    )
}

/// Updates backend-local build progress.
///
/// The currently connected PostgreSQL backend must own a running or
/// cancel-requested job before it can update progress. Terminal statuses clear
/// backend ownership and set `completed_at`.
///
/// # Errors
///
/// Raises `undefined_object` when `build_job_id` does not exist. Raises
/// `insufficient_privilege` when the caller is not a collection owner member.
/// Raises `object_not_in_prerequisite_state` when another active backend owns
/// the job or the transition is invalid. Raises `invalid_parameter_value` for
/// invalid status values, incomplete completion, backward progress, or progress
/// beyond `total_units`.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "update_build_job", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn update_build_job(
    build_job_id: i64,
    processed_units: i64,
    status: String,
    error_message: default!(Option<String>, "NULL"),
) -> TableIterator<
    'static,
    (
        name!(build_job_id, i64),
        name!(collection_name, String),
        name!(artifact_kind, String),
        name!(artifact_name, String),
        name!(target_name, String),
        name!(status, BuildJobStatus),
        name!(backend_pid, Option<i32>),
        name!(attempt, i32),
        name!(processed_units, i64),
        name!(total_units, i64),
        name!(cancel_requested, bool),
        name!(error_message, Option<String>),
    ),
> {
    validate_non_negative_units(processed_units, "processed_units");
    let next_status = parse_build_status_command(&status);
    let current = resolve_visible_build_job(build_job_id);
    ensure_current_backend_can_update(&current);
    if processed_units > current.total_units {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "build job progress exceeds total: {processed_units} > {}",
                current.total_units
            ),
        );
    }
    if processed_units < current.processed_units {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "build job progress cannot go backwards: {processed_units} < {}",
                current.processed_units
            ),
        );
    }
    if next_status == BuildJobStatus::Completed && processed_units != current.total_units {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!(
                "completed build job progress must equal total: {processed_units} <> {}",
                current.total_units
            ),
        );
    }
    let row = update_build_job_row(
        build_job_id,
        processed_units,
        next_status,
        error_message,
        None,
    );
    TableIterator::once(build_job_result(row))
}

/// Requests cancellation for a visible running build job.
///
/// Cancellation is cooperative: the owning backend observes the request through
/// `build_jobs` or the next `update_build_job` call and records `cancelled`
/// after cleaning up.
///
/// # Errors
///
/// Raises `undefined_object` when `build_job_id` does not exist. Raises
/// `insufficient_privilege` when the caller is not a collection owner member.
/// Raises `object_not_in_prerequisite_state` for terminal jobs.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "request_build_cancel", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn request_build_cancel(
    build_job_id: i64,
) -> TableIterator<
    'static,
    (
        name!(build_job_id, i64),
        name!(collection_name, String),
        name!(artifact_kind, String),
        name!(artifact_name, String),
        name!(target_name, String),
        name!(status, BuildJobStatus),
        name!(backend_pid, Option<i32>),
        name!(attempt, i32),
        name!(processed_units, i64),
        name!(total_units, i64),
        name!(cancel_requested, bool),
        name!(error_message, Option<String>),
    ),
> {
    let current = resolve_visible_build_job(build_job_id);
    match effective_build_status(&current) {
        BuildJobStatus::Running => {
            TableIterator::once(build_job_result(request_build_cancel_row(build_job_id)))
        }
        BuildJobStatus::CancelRequested => TableIterator::once(build_job_result(current)),
        status => raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!("cannot cancel build job {build_job_id} in status {status:?}"),
        ),
    }
}

/// Retries a failed, cancelled, or abandoned build job in the current backend.
///
/// # Errors
///
/// Raises `undefined_object` when `build_job_id` does not exist. Raises
/// `insufficient_privilege` when the caller is not a collection owner member.
/// Raises `object_not_in_prerequisite_state` when the job is still actively
/// running or already completed.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "retry_build_job", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn retry_build_job(
    build_job_id: i64,
) -> TableIterator<
    'static,
    (
        name!(build_job_id, i64),
        name!(collection_name, String),
        name!(artifact_kind, String),
        name!(artifact_name, String),
        name!(target_name, String),
        name!(status, BuildJobStatus),
        name!(backend_pid, Option<i32>),
        name!(attempt, i32),
        name!(processed_units, i64),
        name!(total_units, i64),
        name!(cancel_requested, bool),
        name!(error_message, Option<String>),
    ),
> {
    let current = resolve_visible_build_job(build_job_id);
    match effective_build_status(&current) {
        BuildJobStatus::Failed | BuildJobStatus::Cancelled | BuildJobStatus::Abandoned => {
            build_job_failpoint(5, "before_retry");
            TableIterator::once(build_job_result(retry_build_job_row(build_job_id)))
        }
        status => raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!("cannot retry build job {build_job_id} in status {status:?}"),
        ),
    }
}

/// Runs a backend-local build job synchronously in the current backend.
///
/// This runner is intentionally narrow: it advances experimental `segment` and
/// `mmap` artifact jobs through the existing progress table without spawning
/// workers or sharing Rust heap state across PostgreSQL backends. Later
/// artifact implementations can replace the simulated unit loop with real
/// source-table rebuild steps while keeping the same ownership, retry, and
/// crash-recovery contract. Cancellation observed by this function is limited
/// to requests already visible before a runner step starts; a concurrent
/// `request_build_cancel` may wait for this SQL call's transaction to release
/// its row lock.
///
/// # Errors
///
/// Raises `undefined_object` when `build_job_id` does not exist. Raises
/// `insufficient_privilege` when the caller is not a collection owner member.
/// Raises `object_not_in_prerequisite_state` for terminal jobs, jobs owned by
/// another active backend, or artifact kinds that do not have a local runner
/// yet. Raises `invalid_parameter_value` when `units_per_step` is not positive.
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
#[pg_extern(name = "run_build_job", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn run_build_job(
    build_job_id: i64,
    units_per_step: default!(i64, "1"),
) -> TableIterator<
    'static,
    (
        name!(build_job_id, i64),
        name!(collection_name, String),
        name!(artifact_kind, String),
        name!(artifact_name, String),
        name!(target_name, String),
        name!(status, BuildJobStatus),
        name!(backend_pid, Option<i32>),
        name!(attempt, i32),
        name!(processed_units, i64),
        name!(total_units, i64),
        name!(cancel_requested, bool),
        name!(error_message, Option<String>),
    ),
> {
    validate_positive_units(units_per_step, "units_per_step");

    let current = resolve_visible_build_job(build_job_id);
    ensure_build_runner_supported(current.artifact_kind);
    ensure_current_backend_can_update(&current);

    if matches!(
        effective_build_status(&current),
        BuildJobStatus::CancelRequested
    ) || current.cancel_requested
    {
        build_job_failpoint(3, "before_cancel");
        let row = update_build_job_row(
            build_job_id,
            current.processed_units,
            BuildJobStatus::Cancelled,
            Some("build job cancelled before runner step".to_owned()),
            None,
        );
        return TableIterator::once(build_job_result(row));
    }

    if current.processed_units == current.total_units {
        let row = update_build_job_row(
            build_job_id,
            current.total_units,
            BuildJobStatus::Completed,
            None,
            None,
        );
        return TableIterator::once(build_job_result(row));
    }

    build_job_failpoint(6, "before_schema_drift_validation");
    build_job_failpoint(2, "before_source_read");
    let (last_source_point_id, scanned_units) =
        scan_source_point_batch(build_job_id, current.last_source_point_id, units_per_step);
    replay_build_deltas(build_job_id);
    if scanned_units == 0 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!(
                "build source exhausted before declared total: {} < {}",
                current.processed_units, current.total_units
            ),
        );
    }
    let next_processed = current.processed_units + scanned_units;
    if next_processed > current.total_units {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!(
                "build source batch exceeds declared total: {next_processed} > {}",
                current.total_units
            ),
        );
    }
    let next_status = if next_processed == current.total_units {
        BuildJobStatus::Completed
    } else {
        BuildJobStatus::Running
    };
    build_job_failpoint(1, "before_checkpoint");
    let row = update_build_job_row(
        build_job_id,
        next_processed,
        next_status,
        None,
        Some(last_source_point_id),
    );
    TableIterator::once(build_job_result(row))
}

include!("build_jobs/persistence.rs");
