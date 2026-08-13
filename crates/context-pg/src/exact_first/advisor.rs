//! Immutable exact-first advisor plans and generated DDL.

use context_core::{
    EXACT_FIRST_HIGH_CHURN_MILLIHERTZ as HIGH_CHURN_MILLIHERTZ,
    EXACT_FIRST_MAX_DDL_BYTES as MAX_DDL_BYTES,
    EXACT_FIRST_MAX_PLAN_REVISIONS as MAX_PLAN_REVISIONS, EXACT_FIRST_MIN_ANN_ROWS as MIN_ANN_ROWS,
    EXACT_FIRST_MIN_IVF_BUILD_WINDOW_SECONDS as MIN_IVF_BUILD_WINDOW_SECONDS,
    EXACT_FIRST_MIN_IVF_ROWS as MIN_IVF_ROWS,
    EXACT_FIRST_SELECTIVE_FILTER_BPS as SELECTIVE_FILTER_BPS, ExactFirstAdvisorInput,
    ExactFirstAdvisorPolicy, ExactFirstIndexRecommendation, advise_exact_first,
};
use pgrx::JsonB;
use serde::Serialize;
use serde_json::json;

use super::*;

const ADVISOR_VERSION: &str = "exact_first_advisor_v1";
const BUILD_PLAN_VERSION: &str = "exact_first_build_plan_v1";

const ADVISOR_POLICY: ExactFirstAdvisorPolicy = ExactFirstAdvisorPolicy {
    min_ann_rows: MIN_ANN_ROWS,
    min_ivf_rows: MIN_IVF_ROWS,
    high_churn_millihertz: HIGH_CHURN_MILLIHERTZ,
    selective_filter_bps: SELECTIVE_FILTER_BPS,
    min_ivf_build_window_seconds: MIN_IVF_BUILD_WINDOW_SECONDS,
};

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExactFirstObjectives {
    version: String,
    memory_budget_bytes: u64,
    build_window_seconds: u64,
    #[serde(default)]
    update_millihertz: u64,
    #[serde(default = "default_filter_selectivity_bps")]
    filter_selectivity_bps: u16,
    #[serde(default)]
    prefix_certified: bool,
    #[serde(default)]
    scalar_codec_certified: bool,
    #[serde(default)]
    product_codec_certified: bool,
}

#[derive(Clone, Debug)]
struct AdvisorRegistration {
    registration_id: i64,
    registration_revision: i64,
    source_table_oid: pg_sys::Oid,
    source_schema_name: String,
    source_table_name: String,
    binding_ordinal: i16,
    binding_kind: String,
    column_name: String,
    dimensions: u32,
    metric: String,
}

#[derive(Serialize)]
struct FrozenPlan<'a> {
    version: &'static str,
    registration_revision: i64,
    recommendation: &'a str,
    precision: &'a str,
    reason: &'a str,
    evidence: &'a serde_json::Value,
    generated_ddl: Option<&'a str>,
}

/// Freezes one evidence-backed optimization recommendation idempotently.
#[allow(
    clippy::type_complexity,
    reason = "pgrx requires the SQL table shape inline"
)]
#[pg_extern(name = "exact_first_advisor", security_definer)]
#[search_path(pg_catalog, pgcontext)]
pub fn exact_first_advisor(
    collection: String,
    objectives: BoundedExactFirstObjectives,
) -> TableIterator<
    'static,
    (
        name!(plan_revision, i64),
        name!(recommendation, String),
        name!(precision, String),
        name!(reason, String),
        name!(evidence, JsonB),
        name!(generated_ddl, Option<String>),
        name!(plan_sha256, String),
    ),
> {
    let collection_name = CollectionName::new(collection).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            error.to_string(),
        )
    });
    let collection = registration::load_collection(&collection_name);
    require_collection_owner(&collection);
    let objectives = parse_objectives(objectives);
    let registration = load_advisor_registration(collection.collection_id);
    let rows = estimated_rows(registration.source_table_oid);

    if objectives.prefix_certified
        || objectives.scalar_codec_certified
        || objectives.product_codec_certified
    {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            "precision shortcuts require catalog-owned certification evidence",
        );
    }

    let mut decision = advise_exact_first(
        ExactFirstAdvisorInput {
            rows,
            dimensions: registration.dimensions,
            update_millihertz: objectives.update_millihertz,
            filter_selectivity_bps: objectives.filter_selectivity_bps,
            memory_budget_bytes: objectives.memory_budget_bytes,
            build_window_seconds: objectives.build_window_seconds,
            scalar_codec_certified: false,
            product_codec_certified: false,
            prefix_certified: false,
        },
        ADVISOR_POLICY,
    );
    if decision.index == ExactFirstIndexRecommendation::IvfFlat
        && !matches!(registration.binding_kind.as_str(), "dense" | "half")
    {
        decision.index = ExactFirstIndexRecommendation::Hnsw;
    }

    let recommendation = decision.index.as_catalog();
    let precision = decision.precision.as_catalog();
    let reason = decision.reason.as_catalog();
    let mut evidence = json!({
        "contract": ADVISOR_VERSION,
        "rows": rows,
        "dimensions": registration.dimensions,
        "memory_budget_bytes": objectives.memory_budget_bytes,
        "build_window_seconds": objectives.build_window_seconds,
        "update_millihertz": objectives.update_millihertz,
        "filter_selectivity_bps": objectives.filter_selectivity_bps,
        "binding_kind": registration.binding_kind,
        "binding_ordinal": registration.binding_ordinal,
        "metric": registration.metric,
        "thresholds": {
            "min_ann_rows": MIN_ANN_ROWS,
            "min_ivf_rows": MIN_IVF_ROWS,
            "high_churn_millihertz": HIGH_CHURN_MILLIHERTZ,
            "selective_filter_bps": SELECTIVE_FILTER_BPS,
            "min_ivf_build_window_seconds": MIN_IVF_BUILD_WINDOW_SECONDS
        }
    });
    let evidence_bytes = serde_json::to_vec(&evidence).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to encode exact-first advisor evidence: {error}"),
        )
    });
    let evidence_digest = Sha256::digest(&evidence_bytes);
    let index_name = format!(
        "pgcontext_ef_{}_{}_{}_{}",
        registration.registration_id,
        registration.registration_revision,
        registration.binding_ordinal,
        hex_digest(&evidence_digest[..6]),
    );
    evidence["index_name"] = serde_json::Value::String(index_name.clone());
    let generated_ddl = generate_ddl(&registration, decision.index, &index_name);
    let frozen = FrozenPlan {
        version: BUILD_PLAN_VERSION,
        registration_revision: registration.registration_revision,
        recommendation,
        precision,
        reason,
        evidence: &evidence,
        generated_ddl: generated_ddl.as_deref(),
    };
    let frozen_bytes = serde_json::to_vec(&frozen).unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to encode exact-first plan: {error}"),
        )
    });
    let digest = Sha256::digest(&frozen_bytes).to_vec();
    let revision = store_plan(
        &registration,
        recommendation,
        &evidence,
        generated_ddl.as_deref(),
        &digest,
    );
    TableIterator::once((
        revision,
        recommendation.to_owned(),
        precision.to_owned(),
        reason.to_owned(),
        JsonB(evidence),
        generated_ddl,
        hex_digest(&digest),
    ))
}

fn default_filter_selectivity_bps() -> u16 {
    10_000
}

fn parse_objectives(objectives: BoundedExactFirstObjectives) -> ExactFirstObjectives {
    let objectives = serde_json::from_value::<ExactFirstObjectives>(objectives.into_json().0)
        .unwrap_or_else(|error| {
            invalid_specification(format!("invalid exact-first objectives: {error}"))
        });
    if objectives.version != ADVISOR_VERSION {
        invalid_specification("unsupported exact-first advisor version");
    }
    if objectives.filter_selectivity_bps > 10_000
        || objectives.memory_budget_bytes == 0
        || objectives.build_window_seconds == 0
    {
        invalid_specification("exact-first objectives are outside the certified bounds");
    }
    objectives
}

fn load_advisor_registration(collection_id: i64) -> AdvisorRegistration {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT registrations.exact_first_registration_id,
                        registrations.registration_revision,
                        registrations.source_table_oid,
                        registrations.source_schema_name,
                        registrations.source_table_name,
                        columns.binding_ordinal, columns.binding_kind,
                        columns.column_name, columns.dimensions, columns.metric
                   FROM pgcontext._exact_first_registrations AS registrations
                   JOIN pgcontext._exact_first_columns AS columns
                     USING (exact_first_registration_id)
                  WHERE registrations.collection_id = $1
                    AND columns.binding_kind IN ('dense','half','sparse','int8','uint8','bit')
                  ORDER BY columns.binding_ordinal
                  LIMIT 1",
                Some(1),
                &[collection_id.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to load exact-first advisor registration: {error}"),
                )
            });
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
                "exact-first advisor currently requires a registered vector binding",
            );
        }
        let row = rows.first();
        let dimensions = required(row.get::<i32>(9).unwrap_or(None), "binding_dimensions");
        AdvisorRegistration {
            registration_id: required(
                row.get::<i64>(1).unwrap_or(None),
                "exact_first_registration_id",
            ),
            registration_revision: required(
                row.get::<i64>(2).unwrap_or(None),
                "registration_revision",
            ),
            source_table_oid: required(row.get::<pg_sys::Oid>(3).unwrap_or(None), "source_oid"),
            source_schema_name: required(row.get::<String>(4).unwrap_or(None), "source_schema"),
            source_table_name: required(row.get::<String>(5).unwrap_or(None), "source_table"),
            binding_ordinal: required(row.get::<i16>(6).unwrap_or(None), "binding_ordinal"),
            binding_kind: required(row.get::<String>(7).unwrap_or(None), "binding_kind"),
            column_name: required(row.get::<String>(8).unwrap_or(None), "column_name"),
            dimensions: u32::try_from(dimensions).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "exact-first binding dimensions are invalid",
                )
            }),
            metric: required(row.get::<String>(10).unwrap_or(None), "binding_metric"),
        }
    })
}

fn estimated_rows(source_oid: pg_sys::Oid) -> u64 {
    let estimate = Spi::get_one_with_args::<i64>(
        "SELECT greatest(0::real, class.reltuples)::bigint
           FROM pg_catalog.pg_class AS class
          WHERE class.oid = $1",
        &[source_oid.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to estimate exact-first source rows: {error}"),
        )
    })
    .unwrap_or(0);
    u64::try_from(estimate).unwrap_or(0)
}

fn generate_ddl(
    registration: &AdvisorRegistration,
    recommendation: ExactFirstIndexRecommendation,
    index_name: &str,
) -> Option<String> {
    if recommendation == ExactFirstIndexRecommendation::ExactOnly {
        return None;
    }
    let access_method = match recommendation {
        ExactFirstIndexRecommendation::Hnsw => "pgcontext_hnsw",
        ExactFirstIndexRecommendation::IvfFlat => "pgcontext_ivfflat",
        ExactFirstIndexRecommendation::ExactOnly => return None,
    };
    let opclass = opclass(
        &registration.binding_kind,
        &registration.metric,
        recommendation,
    )
    .unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            "the advised exact-first index has no certified operator class",
        )
    });
    let ddl = format!(
        "CREATE INDEX CONCURRENTLY IF NOT EXISTS {} ON {}.{} USING {} ({} {})",
        quote_identifier(index_name),
        quote_identifier(&registration.source_schema_name),
        quote_identifier(&registration.source_table_name),
        access_method,
        quote_identifier(&registration.column_name),
        opclass,
    );
    if ddl.len() > MAX_DDL_BYTES {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "generated exact-first DDL exceeds the certified bound",
        );
    }
    Some(ddl)
}

pub(super) fn opclass(
    binding_kind: &str,
    metric: &str,
    recommendation: ExactFirstIndexRecommendation,
) -> Option<String> {
    let suffix = match metric {
        "l2" => "ops",
        "inner_product" => "ip_ops",
        "cosine" => "cosine_ops",
        "l1" => "l1_ops",
        "hamming" => "hamming_ops",
        "jaccard" => "jaccard_ops",
        _ => return None,
    };
    let kind = match binding_kind {
        "dense" => "vector",
        "half" => "halfvec",
        "sparse" => "sparsevec",
        "int8" => "int8vec",
        "uint8" => "uint8vec",
        "bit" => "bitvec",
        _ => return None,
    };
    if recommendation == ExactFirstIndexRecommendation::IvfFlat
        && !matches!(kind, "vector" | "halfvec")
    {
        return None;
    }
    Some(format!(
        "pgcontext.{kind}_{}_{}",
        recommendation.as_catalog(),
        suffix
    ))
}

fn store_plan(
    registration: &AdvisorRegistration,
    recommendation: &str,
    evidence: &serde_json::Value,
    generated_ddl: Option<&str>,
    digest: &[u8],
) -> i64 {
    Spi::connect_mut(|client| {
        let locked = client
            .select(
                "SELECT exact_first_registration_id
                   FROM pgcontext._exact_first_registrations
                  WHERE exact_first_registration_id = $1
                    AND registration_revision = $2
                  FOR UPDATE",
                Some(1),
                &[
                    registration.registration_id.into(),
                    registration.registration_revision.into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to lock exact-first plan allocation: {error}"),
                )
            });
        if locked.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_T_R_SERIALIZATION_FAILURE,
                "exact-first registration changed during plan allocation",
            );
        }
        let current_state = client
            .select(
                "SELECT EXISTS (
                            SELECT 1
                              FROM pgcontext._exact_first_plans AS current_plan
                             WHERE current_plan.exact_first_registration_id = $1
                               AND current_plan.plan_revision =
                                   registrations.current_plan_revision
                               AND current_plan.plan_sha256 = $2
                        ),
                        EXISTS (
                            SELECT 1
                              FROM pgcontext._exact_first_plans AS current_plan
                              JOIN pgcontext._exact_first_plan_jobs AS plan_jobs
                                USING (exact_first_plan_id)
                             WHERE current_plan.exact_first_registration_id = $1
                               AND current_plan.plan_revision =
                                   registrations.current_plan_revision
                               AND plan_jobs.status IN (
                                   'queued','building','validating','cancel_requested'
                               )
                        )
                   FROM pgcontext._exact_first_registrations AS registrations
                  WHERE registrations.exact_first_registration_id = $1",
                Some(1),
                &[registration.registration_id.into(), digest.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to fence exact-first plan replacement: {error}"),
                )
            });
        let current_state = current_state.first();
        let repeats_current = required(
            current_state.get::<bool>(1).unwrap_or(None),
            "current_plan_digest_matches",
        );
        let current_has_active_job = required(
            current_state.get::<bool>(2).unwrap_or(None),
            "current_plan_has_active_job",
        );
        if current_has_active_job && !repeats_current {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "an active exact-first build must finish or become terminal before replacing its plan",
            );
        }
        let existing = client
            .select(
                "SELECT plan_revision
                   FROM pgcontext._exact_first_plans
                  WHERE exact_first_registration_id = $1 AND plan_sha256 = $2",
                Some(1),
                &[registration.registration_id.into(), digest.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to converge exact-first plan: {error}"),
                )
            });
        let revision = if existing.is_empty() {
            let next = client
                .select(
                    "SELECT COALESCE(max(plan_revision), 0) + 1
                       FROM pgcontext._exact_first_plans
                      WHERE exact_first_registration_id = $1",
                    Some(1),
                    &[registration.registration_id.into()],
                )
                .unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to allocate exact-first plan revision: {error}"),
                    )
                });
            let next = required(next.first().get::<i64>(1).unwrap_or(None), "plan_revision");
            if next > MAX_PLAN_REVISIONS {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                    "exact-first plan history reached the certified retention bound",
                );
            }
            let inserted = client
                .update(
                    "INSERT INTO pgcontext._exact_first_plans (
                         exact_first_registration_id, plan_revision,
                         registration_revision, plan_sha256, apply_policy,
                         recommendation, evidence, generated_ddl
                     ) VALUES ($1,$2,$3,$4,'recommend_only',$5,$6,$7)
                     RETURNING plan_revision",
                    Some(1),
                    &[
                        registration.registration_id.into(),
                        next.into(),
                        registration.registration_revision.into(),
                        digest.into(),
                        recommendation.into(),
                        JsonB(evidence.clone()).into(),
                        nullable_text(generated_ddl),
                    ],
                )
                .unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to store exact-first plan: {error}"),
                    )
                });
            required(
                inserted.first().get::<i64>(1).unwrap_or(None),
                "plan_revision",
            )
        } else {
            required(
                existing.first().get::<i64>(1).unwrap_or(None),
                "plan_revision",
            )
        };
        client
            .update(
                "UPDATE pgcontext._exact_first_registrations
                    SET current_plan_revision = $2, updated_at = pg_catalog.now()
                  WHERE exact_first_registration_id = $1
                    AND registration_revision = $3",
                None,
                &[
                    registration.registration_id.into(),
                    revision.into(),
                    registration.registration_revision.into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to publish exact-first plan pointer: {error}"),
                )
            });
        revision
    })
}

pub(super) fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn nullable_text(value: Option<&str>) -> DatumWithOid<'_> {
    match value {
        Some(value) => value.into(),
        None => None::<String>.into(),
    }
}
