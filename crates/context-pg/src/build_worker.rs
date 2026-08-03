//! PostgreSQL-supervised execution for durable generation jobs.

#[cfg(feature = "pg_test")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use context_build::{BuildJobKind, BuildJobStatus};
use pgrx::bgworkers::{BackgroundWorker, BackgroundWorkerBuilder, SignalWakeFlags};
use pgrx::{pg_sys, prelude::*, spi};

use crate::settings;

const WORKER_WAIT: Duration = Duration::from_millis(100);
const WORKER_IDLE_POLLS: usize = 50;
const WORK_UNITS_PER_STEP: i64 = 256;

#[cfg(feature = "pg_test")]
static DELAY_VALIDATION_PAST_LEASE: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "pg_test")]
static DELAY_PUBLICATION_PAST_LEASE: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerStep {
    Idle,
    Progressed,
}

#[derive(Debug)]
struct ClaimedJob {
    build_job_id: i64,
    collection_id: i64,
    job_kind: BuildJobKind,
    artifact_name: String,
    target_name: String,
    status: BuildJobStatus,
    attempt: i32,
    backend_pid: i32,
    backend_identity: String,
    processed_units: i64,
    total_units: i64,
    last_source_point_id: i64,
    source_high_water: i64,
    source_version: Option<i64>,
    config_revision: i64,
    cancel_requested: bool,
    newly_claimed: bool,
}

/// Launches one supervised worker for committed or soon-to-be-committed jobs.
///
/// Dynamic worker registration is best effort. PostgreSQL saturation and an
/// operator-disabled worker pool leave the durable job in `planned` state.
pub(crate) fn launch_for_current_database() -> bool {
    if !settings::build_workers_enabled() {
        return false;
    }
    let database_oid = current_database_oid();
    let Some(owner_oid) = extension_owner_oid() else {
        return false;
    };
    let Some(argument) = encode_worker_argument(database_oid, owner_oid) else {
        return false;
    };
    BackgroundWorkerBuilder::new("pgContext generation build")
        .set_library("pgcontext")
        .set_function("pgcontext_build_worker_main")
        .set_argument(Some(argument))
        .set_restart_time(Some(Duration::from_secs(1)))
        .enable_spi_access()
        .load_dynamic()
        .is_ok()
}

fn current_database_oid() -> u32 {
    // SAFETY: SQL execution occurs only after database connection setup.
    unsafe { pg_sys::MyDatabaseId.to_u32() }
}

fn extension_owner_oid() -> Option<u32> {
    Spi::get_one::<pg_sys::Oid>(
        "SELECT extowner FROM pg_catalog.pg_extension WHERE extname = 'pgcontext'",
    )
    .ok()
    .flatten()
    .map(pg_sys::Oid::to_u32)
}

fn encode_worker_argument(database_oid: u32, owner_oid: u32) -> Option<pg_sys::Datum> {
    let packed = (u64::from(owner_oid) << 32) | u64::from(database_oid);
    Some(pg_sys::Datum::from(usize::try_from(packed).ok()?))
}

fn decode_worker_argument(argument: pg_sys::Datum) -> (u32, u32) {
    let packed = u64::try_from(argument.value()).unwrap_or_default();
    (
        u32::try_from(packed & u64::from(u32::MAX)).unwrap_or_default(),
        u32::try_from(packed >> 32).unwrap_or_default(),
    )
}

/// Runs one bounded durable job transition in the current connected backend.
///
/// This is also the deterministic test/manual-pump seam used when dynamic
/// workers are intentionally disabled.
pub(crate) fn process_one_step() -> bool {
    match process_one_step_inner() {
        Ok(step) => step == WorkerStep::Progressed,
        Err(error) => pgrx::error!("supervised build step failed atomically: {error}"),
    }
}

#[cfg(feature = "pg_test")]
pub(crate) fn delay_next_validation_past_lease_for_test() {
    DELAY_VALIDATION_PAST_LEASE.store(true, Ordering::SeqCst);
}

#[cfg(feature = "pg_test")]
pub(crate) fn delay_next_publication_past_lease_for_test() {
    DELAY_PUBLICATION_PAST_LEASE.store(true, Ordering::SeqCst);
}

#[cfg(feature = "pg_test")]
fn maybe_delay_past_lease(flag: &AtomicBool) -> Result<(), spi::Error> {
    if flag.swap(false, Ordering::SeqCst) {
        Spi::run("SELECT pg_catalog.pg_sleep(2.1)")?;
    }
    Ok(())
}

#[cfg(feature = "pg_test")]
pub(crate) fn stale_attempt_is_fenced_for_test(build_job_id: i64) -> bool {
    let loaded = Spi::connect(|client| {
        let rows = client.select(
            "SELECT status, attempt, backend_pid, backend_identity
               FROM pgcontext._build_jobs
              WHERE build_job_id = $1",
            Some(1),
            &[build_job_id.into()],
        )?;
        let row = rows.first();
        let status = required(row.get::<String>(1)?)?;
        Ok::<_, spi::Error>((
            BuildJobStatus::from_catalog(&status).ok_or(spi::Error::InvalidPosition)?,
            required(row.get::<i32>(2)?)?,
            required(row.get::<i32>(3)?)?,
            required(row.get::<String>(4)?)?,
        ))
    });
    let Ok((status, attempt, backend_pid, backend_identity)) = loaded else {
        return false;
    };
    let stale = ClaimedJob {
        build_job_id,
        collection_id: 0,
        job_kind: BuildJobKind::Certification,
        artifact_name: String::new(),
        target_name: String::new(),
        status,
        attempt,
        backend_pid,
        backend_identity,
        processed_units: 0,
        total_units: 0,
        last_source_point_id: 0,
        source_high_water: 0,
        source_version: None,
        config_revision: 0,
        cancel_requested: false,
        newly_claimed: false,
    };
    if Spi::run_with_args(
        "UPDATE pgcontext._build_jobs SET attempt = attempt + 1 WHERE build_job_id = $1",
        &[build_job_id.into()],
    )
    .is_err()
    {
        return false;
    }
    set_validating(&stale).is_err()
}

fn process_one_step_inner() -> Result<WorkerStep, spi::Error> {
    let Some(job) = claim_or_resume_job()? else {
        return Ok(WorkerStep::Idle);
    };
    if job.cancel_requested || job.status == BuildJobStatus::CancelRequested {
        finish_cancelled(&job)?;
        return Ok(WorkerStep::Progressed);
    }
    if job.newly_claimed {
        return Ok(WorkerStep::Progressed);
    }
    match (job.job_kind, job.status) {
        (BuildJobKind::Certification, BuildJobStatus::Running) => {
            checkpoint_certification_batch(&job)?
        }
        (BuildJobKind::Certification, BuildJobStatus::Validating) => {
            validate_certification_generation(&job)?
        }
        (BuildJobKind::Certification, BuildJobStatus::Publishing) => publish_generation(&job)?,
        (BuildJobKind::Compaction, BuildJobStatus::Running) => compact_hnsw_pair(&job)?,
        (BuildJobKind::Compaction, BuildJobStatus::Validating) => validate_hnsw_compaction(&job)?,
        (BuildJobKind::Compaction, BuildJobStatus::Publishing) => finish_hnsw_compaction(&job)?,
        _ => fail_job(&job, "unsupported supervised job executor")?,
    }
    Ok(WorkerStep::Progressed)
}

fn claim_or_resume_job() -> Result<Option<ClaimedJob>, spi::Error> {
    Spi::connect_mut(|client| {
        let rows = client.update(
            "WITH worker AS MATERIALIZED (
                 SELECT pg_catalog.pg_backend_pid() AS pid,
                        pg_catalog.to_char(
                            activity.backend_start AT TIME ZONE 'UTC',
                            'YYYY-MM-DD\"T\"HH24:MI:SS.US'
                        ) AS identity
                   FROM pg_catalog.pg_stat_activity AS activity
                  WHERE activity.pid = pg_catalog.pg_backend_pid()
             ), candidate AS (
                 SELECT jobs.build_job_id,
                        jobs.status AS previous_status,
                        worker.pid,
                        worker.identity
                   FROM pgcontext._build_jobs AS jobs
                   CROSS JOIN worker
                  WHERE jobs.supervised
                    AND jobs.job_kind IN ('certification', 'compaction')
                    AND (
                        jobs.status IN ('planned', 'abandoned')
                        OR (
                            jobs.status IN ('running', 'cancel_requested', 'validating', 'publishing')
                            AND (
                                (jobs.backend_pid = worker.pid
                                 AND jobs.backend_identity = worker.identity)
                                OR jobs.lease_expires_at <= pg_catalog.clock_timestamp()
                            )
                        )
                    )
                  ORDER BY jobs.build_job_id
                  FOR UPDATE OF jobs SKIP LOCKED
                  LIMIT 1
             ), source_snapshot AS (
                 SELECT candidate.build_job_id,
                        candidate.previous_status,
                        candidate.pid,
                        candidate.identity,
                        revisions.source_version,
                        CASE WHEN jobs.job_kind = 'compaction' THEN 0
                             ELSE COALESCE(pg_catalog.max(points.point_id), 0)
                        END AS high_water,
                        CASE WHEN jobs.job_kind = 'compaction' THEN 1
                             ELSE pg_catalog.count(points.point_id)
                        END AS total_units
                   FROM candidate
                   JOIN pgcontext._build_jobs AS jobs USING (build_job_id)
                   JOIN pgcontext._collection_source_revisions AS revisions
                     ON revisions.collection_id = jobs.collection_id
                   LEFT JOIN pgcontext._collection_points AS points
                     ON points.collection_id = jobs.collection_id
                  GROUP BY candidate.build_job_id, candidate.previous_status,
                           candidate.pid, candidate.identity, revisions.source_version,
                           jobs.job_kind
             ), claimed AS (
                 UPDATE pgcontext._build_jobs AS jobs
                    SET attempt = jobs.attempt + CASE
                            WHEN jobs.status = 'abandoned'
                              OR (jobs.lease_expires_at <= pg_catalog.clock_timestamp()
                                  AND NOT (
                                      jobs.backend_pid = source_snapshot.pid
                                      AND jobs.backend_identity = source_snapshot.identity
                                  ))
                            THEN 1 ELSE 0 END,
                        status = CASE
                            WHEN jobs.status IN ('planned', 'abandoned')
                            THEN CASE
                                WHEN CASE WHEN jobs.status = 'planned'
                                          THEN source_snapshot.total_units
                                          ELSE jobs.total_units END = 0
                                THEN 'validating' ELSE 'running' END
                            ELSE jobs.status END,
                        backend_pid = source_snapshot.pid,
                        backend_identity = source_snapshot.identity,
                        lease_expires_at = pg_catalog.clock_timestamp() + INTERVAL '2 seconds',
                        source_version = CASE WHEN jobs.status = 'planned'
                            THEN source_snapshot.source_version ELSE jobs.source_version END,
                        source_high_water = CASE WHEN jobs.status = 'planned'
                            THEN source_snapshot.high_water ELSE jobs.source_high_water END,
                        total_units = CASE WHEN jobs.status = 'planned'
                            THEN source_snapshot.total_units ELSE jobs.total_units END,
                        processed_units = CASE WHEN jobs.status = 'planned'
                            THEN 0 ELSE jobs.processed_units END,
                        last_source_point_id = CASE WHEN jobs.status = 'planned'
                            THEN 0 ELSE jobs.last_source_point_id END,
                        completed_at = NULL,
                        updated_at = pg_catalog.now()
                   FROM source_snapshot
                  WHERE jobs.build_job_id = source_snapshot.build_job_id
              RETURNING jobs.build_job_id, jobs.collection_id, jobs.job_kind,
                        jobs.artifact_name, jobs.target_name, jobs.status, jobs.attempt,
                        jobs.backend_pid, jobs.backend_identity,
                        jobs.processed_units, jobs.total_units,
                        jobs.last_source_point_id, jobs.source_high_water,
                        jobs.source_version, jobs.cancel_requested,
                        jobs.config_revision,
                        source_snapshot.previous_status
             )
             SELECT * FROM claimed",
            Some(1),
            &[],
        )?;
        if rows.is_empty() {
            return Ok(None);
        }
        let row = rows.first();
        let job_kind_text = required(row.get::<String>(3)?)?;
        let status_text = required(row.get::<String>(6)?)?;
        let previous_status_text = required(row.get::<String>(17)?)?;
        let previous_status = BuildJobStatus::from_catalog(&previous_status_text)
            .ok_or(spi::Error::InvalidPosition)?;
        let status =
            BuildJobStatus::from_catalog(&status_text).ok_or(spi::Error::InvalidPosition)?;
        if !previous_status.allows_transition(status) {
            return Err(spi::Error::InvalidPosition);
        }
        Ok(Some(ClaimedJob {
            build_job_id: required(row.get::<i64>(1)?)?,
            collection_id: required(row.get::<i64>(2)?)?,
            job_kind: BuildJobKind::from_catalog(&job_kind_text)
                .ok_or(spi::Error::InvalidPosition)?,
            artifact_name: required(row.get::<String>(4)?)?,
            target_name: required(row.get::<String>(5)?)?,
            status,
            attempt: required(row.get::<i32>(7)?)?,
            backend_pid: required(row.get::<i32>(8)?)?,
            backend_identity: required(row.get::<String>(9)?)?,
            processed_units: required(row.get::<i64>(10)?)?,
            total_units: required(row.get::<i64>(11)?)?,
            last_source_point_id: required(row.get::<i64>(12)?)?,
            source_high_water: required(row.get::<i64>(13)?)?,
            source_version: row.get::<i64>(14)?,
            cancel_requested: required(row.get::<bool>(15)?)?,
            config_revision: required(row.get::<i64>(16)?)?,
            newly_claimed: matches!(
                previous_status,
                BuildJobStatus::Planned | BuildJobStatus::Abandoned
            ),
        }))
    })
}

fn checkpoint_certification_batch(job: &ClaimedJob) -> Result<(), spi::Error> {
    let remaining = job.total_units - job.processed_units;
    if remaining <= 0 {
        return set_validating(job);
    }
    let limit = remaining.min(WORK_UNITS_PER_STEP);
    let (last_source_point_id, scanned_units, active_ids, active_keys) = Spi::connect(|client| {
        let rows = client.select(
            "SELECT points.point_id, points.source_key, points.deleted_at IS NULL
                   FROM pgcontext._collection_points AS points
                  WHERE points.collection_id = $1
                    AND points.point_id > $2
                    AND points.point_id <= $3
                  ORDER BY points.point_id
                  LIMIT $4",
            None,
            &[
                job.collection_id.into(),
                job.last_source_point_id.into(),
                job.source_high_water.into(),
                limit.into(),
            ],
        )?;
        let mut last = job.last_source_point_id;
        let mut count = 0_i64;
        let mut ids = Vec::new();
        let mut keys = Vec::new();
        for row in rows {
            last = required(row.get::<i64>(1)?)?;
            let key = required(row.get::<String>(2)?)?;
            let active = required(row.get::<bool>(3)?)?;
            count += 1;
            if active {
                ids.push(last);
                keys.push(key);
            }
        }
        Ok::<_, spi::Error>((last, count, ids, keys))
    })?;
    if scanned_units == 0 {
        return fail_job(job, "source exhausted before declared high-water boundary");
    }
    if !active_ids.is_empty() {
        Spi::run_with_args(
            "INSERT INTO pgcontext._generation_build_rows (
                        build_job_id, point_id, source_key
                 )
                 SELECT $1, ids.point_id, keys.source_key
                   FROM pg_catalog.unnest($2::bigint[]) WITH ORDINALITY
                        AS ids(point_id, ordinality)
                   JOIN pg_catalog.unnest($3::text[]) WITH ORDINALITY
                        AS keys(source_key, ordinality)
                     USING (ordinality)
                 ON CONFLICT (build_job_id, point_id) DO UPDATE
                    SET source_key = EXCLUDED.source_key",
            &[
                job.build_job_id.into(),
                active_ids.into(),
                active_keys.into(),
            ],
        )?;
    }
    let processed_units = job.processed_units + scanned_units;
    let status = if processed_units == job.total_units {
        BuildJobStatus::Validating
    } else {
        BuildJobStatus::Running
    };
    fenced_job_transition(
        job,
        status,
        "UPDATE pgcontext._build_jobs
            SET processed_units = $1,
                last_source_point_id = $2,
                status = $3,
                lease_expires_at = pg_catalog.clock_timestamp() + INTERVAL '2 seconds',
                updated_at = pg_catalog.now()
          WHERE build_job_id = $4
            AND attempt = $5
            AND backend_pid = $6
            AND backend_identity = $7
      RETURNING build_job_id",
        &[
            processed_units.into(),
            last_source_point_id.into(),
            status.as_catalog().into(),
            job.build_job_id.into(),
            job.attempt.into(),
            job.backend_pid.into(),
            job.backend_identity.clone().into(),
        ],
    )
}

fn compact_hnsw_pair(job: &ClaimedJob) -> Result<(), spi::Error> {
    if !hnsw_compaction_target_is_valid(job)? {
        return fail_job(
            job,
            "HNSW compaction target no longer matches the collection source",
        );
    }
    let compacted = Spi::get_one_with_args::<bool>(
        "SELECT pgcontext._compact_hnsw_segment_pair($1::oid::regclass, $2)",
        &[job.target_name.clone().into(), job.config_revision.into()],
    );
    if compacted.is_err() {
        return fail_job(job, "bounded HNSW segment compaction failed");
    }
    fenced_job_transition(
        job,
        BuildJobStatus::Validating,
        "UPDATE pgcontext._build_jobs
            SET processed_units = 1,
                status = 'validating',
                lease_expires_at = pg_catalog.clock_timestamp() + INTERVAL '2 seconds',
                updated_at = pg_catalog.now()
          WHERE build_job_id = $1
            AND attempt = $2
            AND backend_pid = $3
            AND backend_identity = $4
      RETURNING build_job_id",
        &[
            job.build_job_id.into(),
            job.attempt.into(),
            job.backend_pid.into(),
            job.backend_identity.clone().into(),
        ],
    )
}

fn validate_hnsw_compaction(job: &ClaimedJob) -> Result<(), spi::Error> {
    if !hnsw_compaction_target_is_valid(job)? {
        return fail_job(job, "HNSW compaction target changed before validation");
    }
    let valid = Spi::get_one_with_args::<bool>(
        "SELECT stats.segment_count BETWEEN 0 AND $2
                AND stats.active_delta_records >= 0
                AND stats.immutable_rows >= 0
           FROM pgcontext.hnsw_segment_stats($1::oid::regclass) AS stats",
        &[
            job.target_name.clone().into(),
            i32::try_from(crate::hnsw_am::HNSW_MAX_SEGMENTS)
                .unwrap_or(i32::MAX)
                .into(),
        ],
    )?
    .unwrap_or(false);
    if !valid {
        return fail_job(job, "bounded HNSW segment publication failed validation");
    }
    fenced_job_transition(
        job,
        BuildJobStatus::Publishing,
        "UPDATE pgcontext._build_jobs
            SET status = 'publishing',
                validation_passed = true,
                validation_findings = 0,
                lease_expires_at = pg_catalog.clock_timestamp() + INTERVAL '2 seconds',
                updated_at = pg_catalog.now()
          WHERE build_job_id = $1
            AND attempt = $2
            AND backend_pid = $3
            AND backend_identity = $4
      RETURNING build_job_id",
        &[
            job.build_job_id.into(),
            job.attempt.into(),
            job.backend_pid.into(),
            job.backend_identity.clone().into(),
        ],
    )
}

fn finish_hnsw_compaction(job: &ClaimedJob) -> Result<(), spi::Error> {
    fenced_job_transition(
        job,
        BuildJobStatus::Completed,
        "UPDATE pgcontext._build_jobs
            SET status = 'completed',
                backend_pid = NULL,
                backend_identity = NULL,
                lease_expires_at = NULL,
                completed_at = pg_catalog.now(),
                updated_at = pg_catalog.now()
          WHERE build_job_id = $1
            AND attempt = $2
            AND backend_pid = $3
            AND backend_identity = $4
      RETURNING build_job_id",
        &[
            job.build_job_id.into(),
            job.attempt.into(),
            job.backend_pid.into(),
            job.backend_identity.clone().into(),
        ],
    )
}

fn hnsw_compaction_target_is_valid(job: &ClaimedJob) -> Result<bool, spi::Error> {
    Ok(Spi::get_one_with_args::<bool>(
        "SELECT class.relkind = 'i'
                AND access_method.amname = 'pgcontext_hnsw'
                AND catalog_index.indrelid = collections.source_table_oid
           FROM pgcontext._build_jobs AS jobs
           JOIN pgcontext._collections AS collections USING (collection_id)
           JOIN pg_catalog.pg_class AS class ON class.oid = jobs.target_name::oid
           JOIN pg_catalog.pg_am AS access_method ON access_method.oid = class.relam
           JOIN pg_catalog.pg_index AS catalog_index ON catalog_index.indexrelid = class.oid
          WHERE jobs.build_job_id = $1
            AND jobs.attempt = $2
            AND jobs.job_kind = 'compaction'
            AND jobs.target_name = $3",
        &[
            job.build_job_id.into(),
            job.attempt.into(),
            job.target_name.clone().into(),
        ],
    )?
    .unwrap_or(false))
}

fn set_validating(job: &ClaimedJob) -> Result<(), spi::Error> {
    fenced_job_transition(
        job,
        BuildJobStatus::Validating,
        "UPDATE pgcontext._build_jobs
            SET status = 'validating',
                lease_expires_at = pg_catalog.clock_timestamp() + INTERVAL '2 seconds',
                updated_at = pg_catalog.now()
          WHERE build_job_id = $1
            AND attempt = $2
            AND backend_pid = $3
            AND backend_identity = $4
      RETURNING build_job_id",
        &[
            job.build_job_id.into(),
            job.attempt.into(),
            job.backend_pid.into(),
            job.backend_identity.clone().into(),
        ],
    )
}

fn validate_certification_generation(job: &ClaimedJob) -> Result<(), spi::Error> {
    let source_version = lock_source_revision(job.collection_id)?;
    Spi::run_with_args(
        "DELETE FROM pgcontext._generation_build_rows AS staged
          USING (
              SELECT DISTINCT deltas.point_id
                FROM pgcontext._build_deltas AS deltas
               WHERE deltas.build_job_id = $1
          ) AS changed
         WHERE staged.build_job_id = $1
           AND staged.point_id = changed.point_id",
        &[job.build_job_id.into()],
    )?;
    Spi::run_with_args(
        "INSERT INTO pgcontext._generation_build_rows (
                    build_job_id, point_id, source_key
             )
             SELECT $1, points.point_id, points.source_key
               FROM pgcontext._collection_points AS points
               JOIN (
                   SELECT DISTINCT deltas.point_id
                     FROM pgcontext._build_deltas AS deltas
                    WHERE deltas.build_job_id = $1
               ) AS changed USING (point_id)
              WHERE points.collection_id = $2
                AND points.deleted_at IS NULL
             ON CONFLICT (build_job_id, point_id) DO UPDATE
                SET source_key = EXCLUDED.source_key",
        &[job.build_job_id.into(), job.collection_id.into()],
    )?;

    let (set_matches, point_count, checksum) = Spi::connect(|client| {
        let rows = client.select(
            "SELECT
                 NOT EXISTS (
                     (SELECT staged.point_id, staged.source_key
                        FROM pgcontext._generation_build_rows AS staged
                       WHERE staged.build_job_id = $1
                      EXCEPT
                      SELECT points.point_id, points.source_key
                        FROM pgcontext._collection_points AS points
                       WHERE points.collection_id = $2
                         AND points.deleted_at IS NULL)
                     UNION ALL
                     (SELECT points.point_id, points.source_key
                        FROM pgcontext._collection_points AS points
                       WHERE points.collection_id = $2
                         AND points.deleted_at IS NULL
                      EXCEPT
                      SELECT staged.point_id, staged.source_key
                        FROM pgcontext._generation_build_rows AS staged
                       WHERE staged.build_job_id = $1)
                 ) AS set_matches,
                 pg_catalog.count(*) AS point_count,
                 COALESCE(
                     pg_catalog.bit_xor(
                         pg_catalog.hashtextextended(staged.source_key, staged.point_id)
                     ),
                     0
                 ) AS checksum
              FROM pgcontext._generation_build_rows AS staged
             WHERE staged.build_job_id = $1",
            Some(1),
            &[job.build_job_id.into(), job.collection_id.into()],
        )?;
        let row = rows.first();
        Ok::<_, spi::Error>((
            required(row.get::<bool>(1)?)?,
            required(row.get::<i64>(2)?)?,
            required(row.get::<i64>(3)?)?,
        ))
    })?;
    if !set_matches {
        return fail_job(
            job,
            "certification staging does not match authoritative points",
        );
    }

    let payload = format!(
        "{{\"format\":1,\"source_version\":{source_version},\"point_count\":{point_count},\"source_set_checksum_algorithm\":\"pg_hashtextextended_xor_v1\",\"source_set_checksum\":{checksum}}}"
    )
    .into_bytes();
    let payload_checksum = Spi::get_one_with_args::<i64>(
        "SELECT pg_catalog.hashtextextended(pg_catalog.encode($1::bytea, 'hex'), 0)",
        &[payload.clone().into()],
    )?
    .ok_or(spi::Error::InvalidPosition)?;
    let generation = Spi::get_one_with_args::<i64>(
        "INSERT INTO pgcontext._generation_manifests (
                    collection_id, build_job_id, publication_alias,
                    source_version, config_revision, lifecycle_state,
                    validation_passed, validation_findings
             )
             SELECT jobs.collection_id, jobs.build_job_id, jobs.artifact_name,
                    $2, COALESCE(NULLIF(jobs.config_revision, 0), 1),
                    'validated', true, 0
               FROM pgcontext._build_jobs AS jobs
              WHERE jobs.build_job_id = $1
             ON CONFLICT (build_job_id) DO UPDATE
                 SET source_version = EXCLUDED.source_version,
                     config_revision = EXCLUDED.config_revision,
                     lifecycle_state = 'validated',
                     validation_passed = true,
                     validation_findings = 0,
                     updated_at = pg_catalog.now()
         RETURNING generation",
        &[job.build_job_id.into(), source_version.into()],
    )?
    .ok_or(spi::Error::InvalidPosition)?;
    Spi::run_with_args(
        "INSERT INTO pgcontext._generation_artifacts (
                    generation, artifact_kind, artifact_name,
                    payload_bytes, checksum, payload
             )
             VALUES ($1, 'certification_evidence', $2,
                     pg_catalog.octet_length($3::bytea), $4, $3)
             ON CONFLICT (generation, artifact_kind, artifact_name) DO UPDATE
                SET payload_bytes = EXCLUDED.payload_bytes,
                    checksum = EXCLUDED.checksum,
                    payload = EXCLUDED.payload",
        &[
            generation.into(),
            job.artifact_name.clone().into(),
            payload.into(),
            payload_checksum.into(),
        ],
    )?;
    Spi::run_with_args(
        "UPDATE pgcontext._generation_manifests AS manifests
            SET total_payload_bytes = inventory.total_payload_bytes,
                updated_at = pg_catalog.now()
           FROM (
               SELECT pg_catalog.sum(artifacts.payload_bytes) AS total_payload_bytes
                 FROM pgcontext._generation_artifacts AS artifacts
                WHERE artifacts.generation = $1
           ) AS inventory
          WHERE manifests.generation = $1",
        &[generation.into()],
    )?;
    #[cfg(feature = "pg_test")]
    maybe_delay_past_lease(&DELAY_VALIDATION_PAST_LEASE)?;
    fenced_job_transition(
        job,
        BuildJobStatus::Publishing,
        "UPDATE pgcontext._build_jobs
            SET status = 'publishing',
                source_version = $1,
                validation_passed = true,
                validation_findings = 0,
                lease_expires_at = pg_catalog.clock_timestamp() + INTERVAL '2 seconds',
                updated_at = pg_catalog.now()
          WHERE build_job_id = $2
            AND attempt = $3
            AND backend_pid = $4
            AND backend_identity = $5
      RETURNING build_job_id",
        &[
            source_version.into(),
            job.build_job_id.into(),
            job.attempt.into(),
            job.backend_pid.into(),
            job.backend_identity.clone().into(),
        ],
    )
}

fn publish_generation(job: &ClaimedJob) -> Result<(), spi::Error> {
    let current_source_version = lock_source_revision(job.collection_id)?;
    if job.source_version != Some(current_source_version) {
        return fenced_job_transition(
            job,
            BuildJobStatus::Validating,
            "UPDATE pgcontext._build_jobs
                SET status = 'validating',
                    validation_passed = NULL,
                    validation_findings = NULL,
                    lease_expires_at = pg_catalog.clock_timestamp() + INTERVAL '2 seconds',
                    updated_at = pg_catalog.now()
              WHERE build_job_id = $1
                AND attempt = $2
                AND backend_pid = $3
                AND backend_identity = $4
          RETURNING build_job_id",
            &[
                job.build_job_id.into(),
                job.attempt.into(),
                job.backend_pid.into(),
                job.backend_identity.clone().into(),
            ],
        );
    }
    let generation = Spi::get_one_with_args::<i64>(
        "SELECT generation
           FROM pgcontext._generation_manifests
          WHERE build_job_id = $1
            AND lifecycle_state IN ('validated', 'published')
            AND validation_passed",
        &[job.build_job_id.into()],
    )?
    .ok_or(spi::Error::InvalidPosition)?;
    let payload_is_valid = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.count(*) > 0
                AND pg_catalog.bool_and(
                    artifacts.checksum_algorithm = 'pg_hashtextextended_hex_v1'
                    AND
                    artifacts.payload_bytes = pg_catalog.octet_length(artifacts.payload)
                    AND artifacts.checksum = pg_catalog.hashtextextended(
                        pg_catalog.encode(artifacts.payload, 'hex'), 0
                    )
                )
           FROM pgcontext._generation_artifacts AS artifacts
          WHERE artifacts.generation = $1",
        &[generation.into()],
    )?
    .unwrap_or(false);
    if !payload_is_valid {
        return fail_job(job, "generation artifact payload integrity check failed");
    }
    Spi::run_with_args(
        "SELECT pgcontext._publish_generation($1)",
        &[generation.into()],
    )?;
    #[cfg(feature = "pg_test")]
    maybe_delay_past_lease(&DELAY_PUBLICATION_PAST_LEASE)?;
    fenced_job_transition(
        job,
        BuildJobStatus::Completed,
        "UPDATE pgcontext._build_jobs
            SET status = 'completed',
                published_generation = $1,
                backend_pid = NULL,
                backend_identity = NULL,
                lease_expires_at = NULL,
                completed_at = pg_catalog.now(),
                updated_at = pg_catalog.now()
          WHERE build_job_id = $2
            AND attempt = $3
            AND backend_pid = $4
            AND backend_identity = $5
      RETURNING build_job_id",
        &[
            generation.into(),
            job.build_job_id.into(),
            job.attempt.into(),
            job.backend_pid.into(),
            job.backend_identity.clone().into(),
        ],
    )
}

fn lock_source_revision(collection_id: i64) -> Result<i64, spi::Error> {
    Spi::connect_mut(|client| {
        let rows = client.update(
            "SELECT source_version
               FROM pgcontext._collection_source_revisions
              WHERE collection_id = $1
              FOR UPDATE",
            Some(1),
            &[collection_id.into()],
        )?;
        required(rows.first().get::<i64>(1)?)
    })
}

fn finish_cancelled(job: &ClaimedJob) -> Result<(), spi::Error> {
    fenced_job_transition(
        job,
        BuildJobStatus::Cancelled,
        "UPDATE pgcontext._build_jobs
            SET status = 'cancelled',
                backend_pid = NULL,
                backend_identity = NULL,
                lease_expires_at = NULL,
                completed_at = pg_catalog.now(),
                updated_at = pg_catalog.now()
          WHERE build_job_id = $1
            AND attempt = $2
            AND backend_pid = $3
            AND backend_identity = $4
      RETURNING build_job_id",
        &[
            job.build_job_id.into(),
            job.attempt.into(),
            job.backend_pid.into(),
            job.backend_identity.clone().into(),
        ],
    )
}

fn fail_job(job: &ClaimedJob, message: &'static str) -> Result<(), spi::Error> {
    fenced_job_transition(
        job,
        BuildJobStatus::Failed,
        "UPDATE pgcontext._build_jobs
            SET status = 'failed',
                error_message = $1,
                backend_pid = NULL,
                backend_identity = NULL,
                lease_expires_at = NULL,
                completed_at = pg_catalog.now(),
                updated_at = pg_catalog.now()
          WHERE build_job_id = $2
            AND attempt = $3
            AND backend_pid = $4
            AND backend_identity = $5
      RETURNING build_job_id",
        &[
            message.into(),
            job.build_job_id.into(),
            job.attempt.into(),
            job.backend_pid.into(),
            job.backend_identity.clone().into(),
        ],
    )
}

fn fenced_job_transition(
    job: &ClaimedJob,
    next_status: BuildJobStatus,
    sql: &str,
    args: &[pgrx::datum::DatumWithOid<'_>],
) -> Result<(), spi::Error> {
    if !job.status.allows_transition(next_status) {
        return Err(spi::Error::InvalidPosition);
    }
    let updated = Spi::get_one_with_args::<i64>(sql, args)?;
    if updated.is_some() {
        Ok(())
    } else {
        Err(spi::Error::InvalidPosition)
    }
}

fn required<T>(value: Option<T>) -> Result<T, spi::Error> {
    value.ok_or(spi::Error::InvalidPosition)
}

/// PostgreSQL entry point for supervised dynamic generation workers.
#[pg_guard]
#[unsafe(no_mangle)]
pub extern "C-unwind" fn pgcontext_build_worker_main(argument: pg_sys::Datum) {
    let (database_oid, owner_oid) = decode_worker_argument(argument);
    if database_oid == 0 || owner_oid == 0 {
        return;
    }
    BackgroundWorker::attach_signal_handlers(SignalWakeFlags::SIGHUP | SignalWakeFlags::SIGTERM);
    BackgroundWorker::connect_worker_to_spi_by_oid(
        Some(pg_sys::Oid::from_u32(database_oid)),
        Some(pg_sys::Oid::from_u32(owner_oid)),
    );

    let mut idle_polls = 0_usize;
    while BackgroundWorker::wait_latch(Some(WORKER_WAIT)) {
        if BackgroundWorker::transaction(process_one_step) {
            idle_polls = 0;
        } else {
            idle_polls += 1;
            if idle_polls >= WORKER_IDLE_POLLS {
                return;
            }
        }
    }
}
