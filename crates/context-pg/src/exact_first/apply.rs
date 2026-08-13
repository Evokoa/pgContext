//! Explicit foreground application and publication of frozen plans.

use super::{
    advisor::{opclass, quote_identifier},
    *,
};
use context_core::EXACT_FIRST_MAX_TARGETS as MAX_TARGETS;
#[derive(Clone, Debug)]
pub(super) struct StoredPlan {
    pub(super) registration_id: i64,
    pub(super) registration_revision: i64,
    pub(super) plan_id: i64,
    pub(super) plan_revision: i64,
    pub(super) recommendation: String,
    pub(super) generated_ddl: Option<String>,
    pub(super) source_table_oid: pg_sys::Oid,
    pub(super) source_schema_name: String,
    pub(super) source_table_name: String,
    pub(super) binding_ordinal: i16,
    pub(super) index_name: String,
    pub(super) published_index_oid: Option<pg_sys::Oid>,
}

/// Applies one immutable current-revision plan under an explicit policy.
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires the SQL table shape inline"
)]
#[pg_extern(name = "apply_exact_first_plan", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn apply_exact_first_plan(
    collection: String,
    plan_revision: i64,
    policy: String,
) -> TableIterator<
    'static,
    (
        name!(plan_revision, i64),
        name!(plan_status, String),
        name!(readiness_state, String),
        name!(readiness_reason, String),
        name!(build_job_id, Option<i64>),
        name!(index_oid, Option<pg_sys::Oid>),
    ),
> {
    if plan_revision <= 0 {
        invalid_specification("exact-first plan revision must be positive");
    }
    let policy = ExactFirstApplyPolicy::from_catalog(&policy)
        .unwrap_or_else(|| invalid_specification("unsupported exact-first apply policy"));
    let collection_name = CollectionName::new(collection).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            error.to_string(),
        )
    });
    let collection = registration::load_collection(&collection_name);
    require_collection_owner(&collection);
    let plan = load_plan(collection.collection_id, plan_revision);

    match policy {
        ExactFirstApplyPolicy::RecommendOnly | ExactFirstApplyPolicy::ExactOnly => {
            let readiness = load_registration_readiness(&collection);
            return TableIterator::once((
                plan.plan_revision,
                "frozen".to_owned(),
                readiness.readiness.state.as_catalog().to_owned(),
                readiness.readiness.reason.as_catalog().to_owned(),
                None,
                None,
            ));
        }
        ExactFirstApplyPolicy::Enqueue => {
            let build_job_id = enqueue_plan(&plan, collection.collection_id);
            return TableIterator::once((
                plan.plan_revision,
                "queued".to_owned(),
                ExactFirstState::Building.as_catalog().to_owned(),
                ExactFirstReason::BuildActive.as_catalog().to_owned(),
                Some(build_job_id),
                None,
            ));
        }
        ExactFirstApplyPolicy::ApplyForeground => {}
    }

    if plan.recommendation == "exact" {
        let readiness = load_registration_readiness(&collection);
        return TableIterator::once((
            plan.plan_revision,
            "published".to_owned(),
            readiness.readiness.state.as_catalog().to_owned(),
            readiness.readiness.reason.as_catalog().to_owned(),
            None,
            None,
        ));
    }
    if let Some(index_oid) = plan.published_index_oid {
        let readiness = load_registration_readiness(&collection);
        return TableIterator::once((
            plan.plan_revision,
            "published".to_owned(),
            readiness.readiness.state.as_catalog().to_owned(),
            readiness.readiness.reason.as_catalog().to_owned(),
            None,
            Some(index_oid),
        ));
    }
    require_foreground_ddl_authority(&plan);
    lock_foreground_source(&plan);
    lock_foreground_plan(&plan);
    let concurrent_ddl = plan.generated_ddl.as_deref().unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "exact-first optimization plan has no generated DDL",
        )
    });
    let foreground_ddl = concurrent_ddl
        .strip_prefix("CREATE INDEX CONCURRENTLY ")
        .map(|suffix| format!("CREATE INDEX {suffix}"))
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "exact-first plan DDL is not an allow-listed concurrent index build",
            )
        });
    Spi::run(&foreground_ddl).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("exact-first foreground index build failed: {error}"),
        )
    });
    let index_oid = validate_built_index(&plan);
    publish_plan(&plan, index_oid, concurrent_ddl);
    TableIterator::once((
        plan.plan_revision,
        "published".to_owned(),
        ExactFirstState::Indexed.as_catalog().to_owned(),
        ExactFirstReason::OptimizationReady.as_catalog().to_owned(),
        None,
        Some(index_oid),
    ))
}

fn lock_foreground_source(plan: &StoredPlan) {
    let qualified = format!(
        "{}.{}",
        quote_identifier(&plan.source_schema_name),
        quote_identifier(&plan.source_table_name)
    );
    Spi::run(&format!("LOCK TABLE {qualified} IN SHARE MODE")).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to lock exact-first foreground source: {error}"),
        )
    });
    let unchanged = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.to_regclass($1)::oid = $2",
        &[qualified.into(), plan.source_table_oid.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to revalidate exact-first foreground source: {error}"),
        )
    })
    .unwrap_or(false);
    if !unchanged {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "exact-first source relation changed before foreground application",
        );
    }
}

fn lock_foreground_plan(plan: &StoredPlan) {
    let missing = Spi::connect(|client| {
        client
            .select(
                "SELECT exact_first_registration_id
                   FROM pgcontext._exact_first_registrations
                  WHERE exact_first_registration_id = $1
                    AND registration_revision = $2
                    AND current_plan_revision = $3
                  FOR UPDATE",
                Some(1),
                &[
                    plan.registration_id.into(),
                    plan.registration_revision.into(),
                    plan.plan_revision.into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to lock exact-first foreground plan: {error}"),
                )
            })
            .is_empty()
    });
    if missing {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
            "exact-first plan changed before foreground application",
        );
    }
}

fn enqueue_plan(plan: &StoredPlan, collection_id: i64) -> i64 {
    if plan.recommendation == "exact" || plan.generated_ddl.is_none() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "an exact-only plan has no optimization work to enqueue",
        );
    }
    Spi::connect_mut(|client| {
        let current_registration = client
            .select(
                "SELECT exact_first_registration_id
                   FROM pgcontext._exact_first_registrations
                  WHERE exact_first_registration_id = $1
                    AND registration_revision = $2
                    AND current_plan_revision = $3
                  FOR UPDATE",
                Some(1),
                &[
                    plan.registration_id.into(),
                    plan.registration_revision.into(),
                    plan.plan_revision.into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to lock exact-first enqueue: {error}"),
                )
            });
        if current_registration.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
                "exact-first plan changed during enqueue",
            );
        }
        let current = client
            .select(
                "SELECT COALESCE(plan_jobs.status, 'frozen'), plan_jobs.build_job_id
                   FROM pgcontext._exact_first_plans AS plans
                   LEFT JOIN pgcontext._exact_first_plan_jobs AS plan_jobs
                     USING (exact_first_plan_id)
                  WHERE plans.exact_first_plan_id = $1
                  FOR UPDATE OF plans",
                Some(1),
                &[plan.plan_id.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to lock exact-first plan for enqueue: {error}"),
                )
            });
        let row = current.first();
        let status = required(row.get::<String>(1).unwrap_or(None), "plan_status");
        if let Some(build_job_id) = row.get::<i64>(2).unwrap_or(None)
            && matches!(status.as_str(), "queued" | "building" | "validating")
        {
            return build_job_id;
        }
        if !matches!(status.as_str(), "frozen" | "cancelled" | "failed") {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "exact-first plan is not enqueueable",
            );
        }
        let build_job_id = client
            .update(
                "INSERT INTO pgcontext._build_jobs (
                     collection_id, artifact_kind, artifact_name, target_name,
                     job_kind, status, total_units, supervised
                 ) VALUES ($1,'index','exact_first',$2,'artifact_build','planned',1,false)
                 RETURNING build_job_id",
                Some(1),
                &[collection_id.into(), expected_index_name(plan).into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to enqueue exact-first build job: {error}"),
                )
            });
        let build_job_id = required(
            build_job_id.first().get::<i64>(1).unwrap_or(None),
            "build_job_id",
        );
        client
            .update(
                "INSERT INTO pgcontext._exact_first_plan_jobs (
                     exact_first_plan_id, build_job_id, status, attempt,
                     lease_token, lease_expires_at, worker_id, error_code,
                     updated_at
                 ) VALUES ($1, $2, 'queued', 0, NULL, NULL, NULL, NULL,
                           pg_catalog.now())
                 ON CONFLICT (exact_first_plan_id) DO UPDATE
                     SET build_job_id = EXCLUDED.build_job_id,
                         status = 'queued', lease_token = NULL,
                         lease_expires_at = NULL, worker_id = NULL,
                         error_code = NULL, updated_at = pg_catalog.now()",
                None,
                &[plan.plan_id.into(), build_job_id.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to link exact-first plan job: {error}"),
                )
            });
        client
            .update(
                "UPDATE pgcontext._exact_first_registrations
                    SET readiness_state = 'building', readiness_reason = 'build_active',
                        updated_at = pg_catalog.now()
                  WHERE exact_first_registration_id = $1",
                None,
                &[plan.registration_id.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to mark exact-first registration building: {error}"),
                )
            });
        build_job_id
    })
}

pub(super) fn load_plan(collection_id: i64, plan_revision: i64) -> StoredPlan {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT registrations.exact_first_registration_id,
                        registrations.registration_revision,
                        plans.exact_first_plan_id, plans.plan_revision,
                        plans.recommendation, plans.generated_ddl,
                        registrations.source_table_oid,
                        registrations.source_schema_name,
                        registrations.source_table_name,
                        (plans.evidence->>'binding_ordinal')::smallint,
                        plans.evidence->>'index_name',
                        CASE WHEN target_index.indisvalid AND target_index.indisready
                                  AND target_index.indislive
                             THEN targets.index_oid END
                   FROM pgcontext._exact_first_registrations AS registrations
                   JOIN pgcontext._exact_first_plans AS plans
                     USING (exact_first_registration_id)
                   LEFT JOIN pgcontext._exact_first_targets AS targets
                     ON targets.exact_first_plan_id = plans.exact_first_plan_id
                    AND targets.lifecycle_state = 'current'
                    AND targets.structurally_validated
                   LEFT JOIN pg_catalog.pg_index AS target_index
                     ON target_index.indexrelid = targets.index_oid
                  WHERE registrations.collection_id = $1
                    AND plans.plan_revision = $2
                    AND plans.registration_revision = registrations.registration_revision
                    AND registrations.current_plan_revision = plans.plan_revision",
                Some(1),
                &[collection_id.into(), plan_revision.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to load exact-first plan: {error}"),
                )
            });
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "exact-first plan is missing, stale, or no longer current",
            );
        }
        let row = rows.first();
        StoredPlan {
            registration_id: required(row.get::<i64>(1).unwrap_or(None), "registration_id"),
            registration_revision: required(
                row.get::<i64>(2).unwrap_or(None),
                "registration_revision",
            ),
            plan_id: required(row.get::<i64>(3).unwrap_or(None), "plan_id"),
            plan_revision: required(row.get::<i64>(4).unwrap_or(None), "plan_revision"),
            recommendation: required(row.get::<String>(5).unwrap_or(None), "recommendation"),
            generated_ddl: row.get::<String>(6).unwrap_or(None),
            source_table_oid: required(row.get::<pg_sys::Oid>(7).unwrap_or(None), "source_oid"),
            source_schema_name: required(row.get::<String>(8).unwrap_or(None), "source_schema"),
            source_table_name: required(row.get::<String>(9).unwrap_or(None), "source_table"),
            binding_ordinal: required(row.get::<i16>(10).unwrap_or(None), "binding_ordinal"),
            index_name: required(row.get::<String>(11).unwrap_or(None), "index_name"),
            published_index_oid: row.get::<pg_sys::Oid>(12).unwrap_or(None),
        }
    })
}

fn require_foreground_ddl_authority(plan: &StoredPlan) {
    let allowed = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.pg_has_role(SESSION_USER, class.relowner, 'USAGE')
                AND pg_catalog.has_schema_privilege(
                    SESSION_USER, class.relnamespace, 'CREATE')
           FROM pg_catalog.pg_class AS class
          WHERE class.oid = $1",
        &[plan.source_table_oid.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to authorize exact-first foreground DDL: {error}"),
        )
    })
    .unwrap_or(false);
    if !allowed {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            "exact-first foreground apply requires source ownership and schema CREATE privilege",
        );
    }
}

pub(super) fn expected_index_name(plan: &StoredPlan) -> String {
    plan.index_name.clone()
}

pub(super) fn validate_built_index(plan: &StoredPlan) -> pg_sys::Oid {
    let qualified = format!(
        "{}.{}",
        quote_identifier(&plan.source_schema_name),
        quote_identifier(&expected_index_name(plan))
    );
    let index_oid = Spi::get_one_with_args::<pg_sys::Oid>(
        "SELECT pg_catalog.to_regclass($1)",
        &[qualified.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to resolve built exact-first index: {error}"),
        )
    })
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "exact-first foreground build did not create the expected index",
        )
    });
    let binding = Spi::get_two_with_args::<String, String>(
        "SELECT columns.binding_kind, columns.metric
           FROM pgcontext._exact_first_columns AS columns
          WHERE columns.exact_first_registration_id = $1
            AND columns.binding_ordinal = $2",
        &[plan.registration_id.into(), plan.binding_ordinal.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to load exact-first index binding: {error}"),
        )
    });
    let binding_kind = required(binding.0, "binding_kind");
    let metric = required(binding.1, "binding_metric");
    let recommendation = match plan.recommendation.as_str() {
        "hnsw" => context_core::ExactFirstIndexRecommendation::Hnsw,
        "ivfflat" => context_core::ExactFirstIndexRecommendation::IvfFlat,
        _ => raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "exact-first plan recommendation is not indexable",
        ),
    };
    let expected_opclass = opclass(&binding_kind, &metric, recommendation).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "exact-first plan operator class is not supported",
        )
    });
    let expected_method = match recommendation {
        context_core::ExactFirstIndexRecommendation::Hnsw => "pgcontext_hnsw",
        context_core::ExactFirstIndexRecommendation::IvfFlat => "pgcontext_ivfflat",
        context_core::ExactFirstIndexRecommendation::ExactOnly => raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "exact-first exact-only plan unexpectedly reached index validation",
        ),
    };
    let valid = Spi::get_one_with_args::<bool>(
        "SELECT index.indrelid = $2
                AND index.indisvalid AND index.indisready AND index.indislive
                AND index.indimmediate AND index.indpred IS NULL
                AND index.indexprs IS NULL AND index.indnkeyatts = 1
                AND index.indkey[0] = columns.column_attnum
                AND access_method.amname = $4
                AND opclass_namespace.nspname = 'pgcontext'
                AND opclass.opcname = pg_catalog.split_part($5, '.', 2)
           FROM pg_catalog.pg_index AS index
           JOIN pg_catalog.pg_class AS index_class ON index_class.oid = index.indexrelid
           JOIN pg_catalog.pg_am AS access_method ON access_method.oid = index_class.relam
           JOIN pg_catalog.pg_opclass AS opclass ON opclass.oid = index.indclass[0]
           JOIN pg_catalog.pg_namespace AS opclass_namespace
             ON opclass_namespace.oid = opclass.opcnamespace
           JOIN pgcontext._exact_first_columns AS columns
             ON columns.exact_first_registration_id = $3
            AND columns.binding_ordinal = $6
          WHERE index.indexrelid = $1",
        &[
            index_oid.into(),
            plan.source_table_oid.into(),
            plan.registration_id.into(),
            expected_method.into(),
            expected_opclass.as_str().into(),
            plan.binding_ordinal.into(),
        ],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to validate built exact-first index: {error}"),
        )
    })
    .unwrap_or(false);
    if !valid {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "exact-first foreground index failed structural validation",
        );
    }
    index_oid
}

pub(super) fn publish_plan(plan: &StoredPlan, index_oid: pg_sys::Oid, generated_ddl: &str) {
    let fingerprint = Sha256::digest(generated_ddl.as_bytes()).to_vec();
    Spi::connect_mut(|client| {
        let locked = client
            .select(
                "SELECT exact_first_registration_id
                   FROM pgcontext._exact_first_registrations
                  WHERE exact_first_registration_id = $1
                    AND registration_revision = $2
                    AND current_plan_revision = $3
                  FOR UPDATE",
                Some(1),
                &[
                    plan.registration_id.into(),
                    plan.registration_revision.into(),
                    plan.plan_revision.into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to lock exact-first publication: {error}"),
                )
            });
        if locked.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
                "exact-first registration changed during publication",
            );
        }
        client
            .update(
                "UPDATE pgcontext._exact_first_targets
                    SET lifecycle_state = 'retired'
                  WHERE exact_first_registration_id = $1
                    AND lifecycle_state = 'current'",
                None,
                &[plan.registration_id.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to retire prior exact-first target: {error}"),
                )
            });
        client
            .update(
                "INSERT INTO pgcontext._exact_first_targets (
                     exact_first_registration_id, exact_first_plan_id,
                     index_oid, index_fingerprint_sha256, lifecycle_state,
                     structurally_validated, published_at
                 ) VALUES ($1,$2,$3,$4,'current',true,pg_catalog.now())",
                None,
                &[
                    plan.registration_id.into(),
                    plan.plan_id.into(),
                    index_oid.into(),
                    fingerprint.into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to insert exact-first target: {error}"),
                )
            });
        client
            .update(
                "DELETE FROM pgcontext._exact_first_targets
                  WHERE exact_first_target_id IN (
                        SELECT exact_first_target_id
                          FROM pgcontext._exact_first_targets
                         WHERE exact_first_registration_id = $1
                           AND lifecycle_state <> 'current'
                         ORDER BY created_at DESC, exact_first_target_id DESC
                        OFFSET $2
                  )",
                None,
                &[plan.registration_id.into(), (MAX_TARGETS - 1).into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to prune exact-first target history: {error}"),
                )
            });
        client
            .update(
                "DELETE FROM pgcontext._exact_first_plan_jobs
                  WHERE exact_first_plan_id = $1",
                None,
                &[plan.plan_id.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to clear exact-first operational plan state: {error}"),
                )
            });
        client
            .update(
                "UPDATE pgcontext._exact_first_registrations
                    SET readiness_state = 'indexed',
                        readiness_reason = 'optimization_ready',
                        updated_at = pg_catalog.now()
                  WHERE exact_first_registration_id = $1
                    AND registration_revision = $2
                    AND current_plan_revision = $3",
                None,
                &[
                    plan.registration_id.into(),
                    plan.registration_revision.into(),
                    plan.plan_revision.into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to publish exact-first foreground plan: {error}"),
                )
            });
    });
}
