//! Durable controller protocol for genuine top-level concurrent index builds.

use super::{
    apply::{StoredPlan, load_plan, publish_plan, validate_built_index},
    *,
};
use pgrx::spi::SpiClient;

use context_core::{
    EXACT_FIRST_MAX_ATTEMPTS as MAX_ATTEMPTS,
    EXACT_FIRST_MAX_ERROR_CODE_BYTES as MAX_ERROR_CODE_BYTES,
    EXACT_FIRST_MAX_LEASE_MILLIS as MAX_LEASE_MILLIS,
    EXACT_FIRST_MAX_NAME_BYTES as MAX_WORKER_ID_BYTES,
};

/// Claims one queued plan and returns its reviewed top-level DDL.
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires the SQL table shape inline"
)]
#[pg_extern(name = "claim_exact_first_build", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn claim_exact_first_build(
    collection: String,
    worker_id: String,
    lease_millis: i32,
) -> TableIterator<
    'static,
    (
        name!(plan_revision, i64),
        name!(lease_token, i64),
        name!(lease_expires_micros, i64),
        name!(generated_ddl, String),
    ),
> {
    validate_worker_id(&worker_id);
    validate_lease_millis(lease_millis);
    let (_collection, collection_registration) = authorized_collection(collection);
    let plan_revision = current_plan_revision(collection_registration.collection_id);
    let plan = load_plan(collection_registration.collection_id, plan_revision);
    let generated_ddl = plan.generated_ddl.clone().unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "exact-first plan has no optimization DDL to claim",
        )
    });
    let claim = Spi::connect_mut(|client| {
        let rows = client
            .select(
                "SELECT plan_jobs.status, plan_jobs.attempt,
                        plan_jobs.lease_expires_at IS NULL
                            OR plan_jobs.lease_expires_at <= pg_catalog.clock_timestamp() AS expired,
                        plan_jobs.build_job_id
                   FROM pgcontext._exact_first_plans AS plans
                   JOIN pgcontext._exact_first_plan_jobs AS plan_jobs
                     USING (exact_first_plan_id)
                  WHERE plans.exact_first_plan_id = $1
                  FOR UPDATE OF plan_jobs",
                Some(1),
                &[plan.plan_id.into()],
            )
            .unwrap_or_else(|error| internal("lock exact-first claim", error));
        let row = rows.first();
        let status = required(row.get::<String>(1).unwrap_or(None), "plan_status");
        let attempt = required(row.get::<i32>(2).unwrap_or(None), "plan_attempt");
        let expired = required(row.get::<bool>(3).unwrap_or(None), "lease_expired");
        let build_job_id = required(row.get::<i64>(4).unwrap_or(None), "build_job_id");
        if status == "cancel_requested" {
            if expired {
                terminalize_cancelled(client, &plan, build_job_id);
            }
            return None;
        }
        if status == "building" && !expired {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_IN_USE,
                "exact-first build already has an active lease",
            );
        }
        if !matches!(status.as_str(), "queued" | "building" | "cancel_requested") {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "exact-first build is not claimable",
            );
        }
        if attempt >= MAX_ATTEMPTS {
            terminalize_failed(client, &plan, build_job_id, "attempts_exhausted");
            return None;
        }
        let claimed = client
            .update(
                "UPDATE pgcontext._exact_first_plan_jobs
                    SET status = 'building', attempt = attempt + 1,
                        lease_token = pg_catalog.nextval(
                            'pgcontext._build_jobs_build_job_id_seq'::regclass),
                        lease_expires_at = pg_catalog.clock_timestamp()
                            + pg_catalog.make_interval(secs => $2::double precision / 1000.0),
                        worker_id = $3, error_code = NULL,
                        updated_at = pg_catalog.now()
                  WHERE exact_first_plan_id = $1
              RETURNING lease_token,
                        (extract(epoch FROM lease_expires_at) * 1000000)::bigint",
                Some(1),
                &[
                    plan.plan_id.into(),
                    lease_millis.into(),
                    worker_id.as_str().into(),
                ],
            )
            .unwrap_or_else(|error| internal("claim exact-first plan", error));
        let claimed = claimed.first();
        let token = required(claimed.get::<i64>(1).unwrap_or(None), "lease_token");
        let expiry = required(
            claimed.get::<i64>(2).unwrap_or(None),
            "lease_expires_micros",
        );
        client
            .update(
                "UPDATE pgcontext._build_jobs
                    SET status = 'running', attempt = $2,
                        backend_pid = pg_catalog.pg_backend_pid(), backend_identity = $3,
                        lease_expires_at = pg_catalog.to_timestamp($4::double precision / 1000000.0),
                        error_message = NULL, updated_at = pg_catalog.now()
                  WHERE build_job_id = $1",
                None,
                &[
                    build_job_id.into(),
                    (attempt + 1).into(),
                    worker_id.as_str().into(),
                    expiry.into(),
                ],
            )
            .unwrap_or_else(|error| internal("claim exact-first build job", error));
        Some((token, expiry))
    });
    let Some((token, expiry)) = claim else {
        return TableIterator::empty();
    };
    TableIterator::once((plan.plan_revision, token, expiry, generated_ddl))
}

/// Renews one active controller lease while top-level DDL is running.
#[pg_extern(name = "heartbeat_exact_first_build", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn heartbeat_exact_first_build(
    collection: String,
    plan_revision: i64,
    lease_token: i64,
    lease_millis: i32,
) -> bool {
    validate_lease_millis(lease_millis);
    let (_collection, collection_registration) = authorized_collection(collection);
    let plan = load_plan(collection_registration.collection_id, plan_revision);
    Spi::get_one_with_args::<bool>(
        "WITH renewed AS (
             UPDATE pgcontext._exact_first_plan_jobs
                SET lease_expires_at = pg_catalog.clock_timestamp()
                    + pg_catalog.make_interval(secs => $3::double precision / 1000.0),
                    updated_at = pg_catalog.now()
              WHERE exact_first_plan_id = $1
                AND lease_token = $2
                AND status = 'building'
                AND lease_expires_at > pg_catalog.clock_timestamp()
          RETURNING build_job_id, lease_expires_at
         ), jobs AS (
             UPDATE pgcontext._build_jobs AS jobs
                SET lease_expires_at = renewed.lease_expires_at,
                    updated_at = pg_catalog.now()
               FROM renewed
              WHERE jobs.build_job_id = renewed.build_job_id
          RETURNING 1
         ) SELECT EXISTS (SELECT 1 FROM renewed)",
        &[plan.plan_id.into(), lease_token.into(), lease_millis.into()],
    )
    .unwrap_or_else(|error| internal("heartbeat exact-first build", error))
    .unwrap_or(false)
}

/// Validates and publishes the index created by the claimed top-level DDL.
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires the SQL table shape inline"
)]
#[pg_extern(name = "publish_exact_first_build", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn publish_exact_first_build(
    collection: String,
    plan_revision: i64,
    lease_token: i64,
) -> TableIterator<
    'static,
    (
        name!(plan_status, String),
        name!(readiness_state, String),
        name!(index_oid, pg_sys::Oid),
    ),
> {
    let (_collection, collection_registration) = authorized_collection(collection);
    let plan = load_plan(collection_registration.collection_id, plan_revision);
    if let Some(index_oid) = plan.published_index_oid {
        return TableIterator::once((
            "published".to_owned(),
            ExactFirstState::Indexed.as_catalog().to_owned(),
            index_oid,
        ));
    }
    let build_job_id = require_active_lease(&plan, lease_token);
    Spi::run_with_args(
        "UPDATE pgcontext._exact_first_plan_jobs SET status = 'validating'
          WHERE exact_first_plan_id = $1 AND lease_token = $2",
        &[plan.plan_id.into(), lease_token.into()],
    )
    .unwrap_or_else(|error| internal("mark exact-first plan validating", error));
    let index_oid = validate_built_index(&plan);
    let ddl = plan.generated_ddl.as_deref().unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "exact-first build plan has no generated DDL",
        )
    });
    publish_plan(&plan, index_oid, ddl);
    Spi::run_with_args(
        "UPDATE pgcontext._build_jobs
            SET status = 'completed', processed_units = total_units,
                validation_passed = true, lease_expires_at = NULL,
                backend_pid = NULL, backend_identity = NULL,
                completed_at = pg_catalog.now(), updated_at = pg_catalog.now()
          WHERE build_job_id = $1",
        &[build_job_id.into()],
    )
    .unwrap_or_else(|error| internal("complete exact-first build job", error));
    TableIterator::once((
        "published".to_owned(),
        ExactFirstState::Indexed.as_catalog().to_owned(),
        index_oid,
    ))
}

/// Requests cooperative cancellation of the current exact-first build.
#[pg_extern(name = "cancel_exact_first_build", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn cancel_exact_first_build(collection: String) -> bool {
    let (_collection, collection_registration) = authorized_collection(collection);
    let revision = current_plan_revision(collection_registration.collection_id);
    let plan = load_plan(collection_registration.collection_id, revision);
    Spi::connect_mut(|client| {
        let rows = client
            .update(
                "UPDATE pgcontext._exact_first_plan_jobs
                    SET status = CASE WHEN status = 'queued' THEN 'cancelled'
                                      ELSE 'cancel_requested' END,
                        lease_token = CASE WHEN status = 'queued' THEN NULL ELSE lease_token END,
                        lease_expires_at = CASE WHEN status = 'queued' THEN NULL ELSE lease_expires_at END,
                        updated_at = pg_catalog.now()
                  WHERE exact_first_plan_id = $1
                    AND status IN ('queued','building')
              RETURNING status, build_job_id",
                Some(1),
                &[plan.plan_id.into()],
            )
            .unwrap_or_else(|error| internal("cancel exact-first plan", error));
        if rows.is_empty() {
            return false;
        }
        let row = rows.first();
        let status = required(row.get::<String>(1).unwrap_or(None), "plan_status");
        let build_job_id = required(row.get::<i64>(2).unwrap_or(None), "build_job_id");
        let job_status = if status == "cancelled" {
            "cancelled"
        } else {
            "cancel_requested"
        };
        client
            .update(
                "UPDATE pgcontext._build_jobs
                    SET status = $2, cancel_requested = true,
                        completed_at = CASE WHEN $2 = 'cancelled' THEN pg_catalog.now()
                                            ELSE completed_at END,
                        updated_at = pg_catalog.now()
                  WHERE build_job_id = $1",
                None,
                &[build_job_id.into(), job_status.into()],
            )
            .unwrap_or_else(|error| internal("cancel exact-first build job", error));
        true
    })
}

/// Requeues one cancelled or failed current plan without changing its digest.
#[pg_extern(name = "retry_exact_first_build", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn retry_exact_first_build(collection: String) -> bool {
    let (_collection, collection_registration) = authorized_collection(collection);
    let revision = current_plan_revision(collection_registration.collection_id);
    let plan = load_plan(collection_registration.collection_id, revision);
    Spi::connect_mut(|client| {
        let rows = client
            .update(
                "UPDATE pgcontext._exact_first_plan_jobs
                    SET status = 'queued', lease_token = NULL,
                        lease_expires_at = NULL, worker_id = NULL,
                        error_code = NULL, updated_at = pg_catalog.now()
                  WHERE exact_first_plan_id = $1
                    AND status IN ('cancelled','failed')
                    AND attempt < $2
              RETURNING build_job_id",
                Some(1),
                &[plan.plan_id.into(), MAX_ATTEMPTS.into()],
            )
            .unwrap_or_else(|error| internal("retry exact-first plan", error));
        if rows.is_empty() {
            return false;
        }
        let build_job_id = required(rows.first().get::<i64>(1).unwrap_or(None), "build_job_id");
        client
            .update(
                "UPDATE pgcontext._build_jobs
                    SET status = 'planned', cancel_requested = false,
                        backend_pid = NULL, backend_identity = NULL,
                        lease_expires_at = NULL, error_message = NULL,
                        completed_at = NULL, updated_at = pg_catalog.now()
                  WHERE build_job_id = $1",
                None,
                &[build_job_id.into()],
            )
            .unwrap_or_else(|error| internal("retry exact-first build job", error));
        true
    })
}

/// Reports a bounded operational failure for the active fenced attempt.
#[pg_extern(name = "fail_exact_first_build", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn fail_exact_first_build(
    collection: String,
    plan_revision: i64,
    lease_token: i64,
    error_code: String,
) -> bool {
    validate_error_code(&error_code);
    let (_collection, collection_registration) = authorized_collection(collection);
    let plan = load_plan(collection_registration.collection_id, plan_revision);
    let build_job_id = require_active_lease(&plan, lease_token);
    Spi::connect_mut(|client| {
        terminalize_failed(client, &plan, build_job_id, &error_code);
    });
    true
}

/// Returns bounded content-free progress for the current plan.
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires the SQL table shape inline"
)]
#[pg_extern(name = "exact_first_progress", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn exact_first_progress(
    collection: String,
) -> TableIterator<
    'static,
    (
        name!(plan_revision, Option<i64>),
        name!(plan_status, Option<String>),
        name!(build_job_id, Option<i64>),
        name!(attempt, i32),
        name!(processed_units, i64),
        name!(total_units, i64),
        name!(invalid_samples, i64),
        name!(error_code, Option<String>),
    ),
> {
    let (_collection, collection_registration) = authorized_collection(collection);
    let result = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT plans.plan_revision,
                        CASE WHEN targets.exact_first_target_id IS NOT NULL
                                  AND target_index.indisvalid
                                  AND target_index.indisready
                                  AND target_index.indislive
                             THEN 'published'
                             ELSE COALESCE(plan_jobs.status, 'frozen') END,
                        plan_jobs.build_job_id,
                        COALESCE(plan_jobs.attempt, 0),
                        COALESCE(jobs.processed_units, 0),
                        COALESCE(jobs.total_units, 0),
                        (SELECT count(*)
                           FROM pgcontext._exact_first_invalid_samples AS samples
                          WHERE samples.exact_first_registration_id =
                                registrations.exact_first_registration_id),
                        plan_jobs.error_code
                   FROM pgcontext._exact_first_registrations AS registrations
                   LEFT JOIN pgcontext._exact_first_plans AS plans
                     ON plans.exact_first_registration_id = registrations.exact_first_registration_id
                    AND plans.plan_revision = registrations.current_plan_revision
                   LEFT JOIN pgcontext._exact_first_plan_jobs AS plan_jobs
                     ON plan_jobs.exact_first_plan_id = plans.exact_first_plan_id
                   LEFT JOIN pgcontext._exact_first_targets AS targets
                     ON targets.exact_first_plan_id = plans.exact_first_plan_id
                    AND targets.lifecycle_state = 'current'
                    AND targets.structurally_validated
                   LEFT JOIN pg_catalog.pg_index AS target_index
                     ON target_index.indexrelid = targets.index_oid
                   LEFT JOIN pgcontext._build_jobs AS jobs
                     ON jobs.build_job_id = plan_jobs.build_job_id
                  WHERE registrations.collection_id = $1",
                Some(1),
                &[collection_registration.collection_id.into()],
            )
            .unwrap_or_else(|error| internal("load exact-first progress", error));
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                "exact-first registration does not exist",
            );
        }
        let row = rows.first();
        (
            row.get::<i64>(1).unwrap_or(None),
            row.get::<String>(2).unwrap_or(None),
            row.get::<i64>(3).unwrap_or(None),
            required(row.get::<i32>(4).unwrap_or(None), "attempt"),
            required(row.get::<i64>(5).unwrap_or(None), "processed_units"),
            required(row.get::<i64>(6).unwrap_or(None), "total_units"),
            required(row.get::<i64>(7).unwrap_or(None), "invalid_samples"),
            row.get::<String>(8).unwrap_or(None),
        )
    });
    TableIterator::once(result)
}

fn authorized_collection(collection: String) -> (CollectionName, CollectionRegistration) {
    let collection = CollectionName::new(collection).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            error.to_string(),
        )
    });
    let registration = registration::load_collection(&collection);
    require_collection_owner(&registration);
    (collection, registration)
}

fn current_plan_revision(collection_id: i64) -> i64 {
    Spi::get_one_with_args::<i64>(
        "SELECT current_plan_revision
           FROM pgcontext._exact_first_registrations
          WHERE collection_id = $1",
        &[collection_id.into()],
    )
    .unwrap_or_else(|error| internal("load current exact-first plan", error))
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "exact-first registration has no current plan",
        )
    })
}

fn require_active_lease(plan: &StoredPlan, lease_token: i64) -> i64 {
    Spi::get_one_with_args::<i64>(
        "SELECT plan_jobs.build_job_id
           FROM pgcontext._exact_first_plan_jobs AS plan_jobs
          WHERE plan_jobs.exact_first_plan_id = $1
            AND plan_jobs.lease_token = $2
            AND plan_jobs.status = 'building'
            AND plan_jobs.lease_expires_at > pg_catalog.clock_timestamp()
          FOR UPDATE OF plan_jobs",
        &[plan.plan_id.into(), lease_token.into()],
    )
    .unwrap_or_else(|error| internal("validate exact-first lease", error))
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "exact-first build lease is stale, expired, or cancelled",
        )
    })
}

fn terminalize_cancelled(client: &mut SpiClient<'_>, plan: &StoredPlan, build_job_id: i64) {
    client
        .update(
            "UPDATE pgcontext._exact_first_plan_jobs
                SET status = 'cancelled', lease_token = NULL,
                    lease_expires_at = NULL, worker_id = NULL,
                    updated_at = pg_catalog.now()
              WHERE exact_first_plan_id = $1",
            None,
            &[plan.plan_id.into()],
        )
        .unwrap_or_else(|error| internal("terminalize exact-first cancellation", error));
    client
        .update(
            "UPDATE pgcontext._build_jobs
                SET status = 'cancelled', cancel_requested = true,
                    backend_pid = NULL, backend_identity = NULL,
                    lease_expires_at = NULL, completed_at = pg_catalog.now(),
                    updated_at = pg_catalog.now()
              WHERE build_job_id = $1",
            None,
            &[build_job_id.into()],
        )
        .unwrap_or_else(|error| internal("terminalize exact-first cancelled job", error));
}

fn terminalize_failed(
    client: &mut SpiClient<'_>,
    plan: &StoredPlan,
    build_job_id: i64,
    error_code: &str,
) {
    client
        .update(
            "UPDATE pgcontext._exact_first_plan_jobs
                SET status = 'failed', error_code = $2,
                    lease_token = NULL, lease_expires_at = NULL,
                    worker_id = NULL, updated_at = pg_catalog.now()
              WHERE exact_first_plan_id = $1",
            None,
            &[plan.plan_id.into(), error_code.into()],
        )
        .unwrap_or_else(|error| internal("fail exact-first plan", error));
    client
        .update(
            "UPDATE pgcontext._build_jobs
                SET status = 'failed', error_message = $2,
                    backend_pid = NULL, backend_identity = NULL,
                    lease_expires_at = NULL, completed_at = pg_catalog.now(),
                    updated_at = pg_catalog.now()
              WHERE build_job_id = $1",
            None,
            &[build_job_id.into(), error_code.into()],
        )
        .unwrap_or_else(|error| internal("fail exact-first build job", error));
}

fn validate_worker_id(worker_id: &str) {
    if worker_id.is_empty()
        || worker_id.len() > MAX_WORKER_ID_BYTES
        || worker_id.chars().any(char::is_control)
    {
        invalid_specification("exact-first worker ID is empty, oversized, or contains controls");
    }
}

fn validate_error_code(error_code: &str) {
    if error_code.is_empty()
        || error_code.len() > MAX_ERROR_CODE_BYTES
        || !error_code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        invalid_specification("exact-first error code must be a bounded lowercase identifier");
    }
}

fn validate_lease_millis(lease_millis: i32) {
    if !(1..=MAX_LEASE_MILLIS).contains(&lease_millis) {
        invalid_specification("exact-first lease must be between 1 and 60000 milliseconds");
    }
}

fn internal(subject: &str, error: impl std::fmt::Display) -> ! {
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
        format!("failed to {subject}: {error}"),
    )
}
