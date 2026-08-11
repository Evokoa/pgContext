//! PostgreSQL adapter for bounded mixed-profile retrieval.

#[cfg(feature = "pg_test")]
use std::cell::Cell;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;
use std::rc::Rc;

use context_core::{
    ConfigurationRevision, PointId, ProfileId, ProfileLifecycle, SourceKey, SourceVersion,
};
use context_query::{
    Candidate, CandidateDiagnostics, CandidatePage, CandidateSource, ExecutionBudget,
    ExecutionOutcome, FilterCandidateBatch, FilterCandidateSource, HydratedCandidate,
    MAX_MULTI_PROFILE_BRANCHES, MAX_MULTI_PROFILE_QUERY_TOTAL_BYTES, MissingProfile,
    MissingProfileReason, MultiProfileBranch, MultiProfileCoverage, MultiProfileDecision,
    MultiProfileObserved, MultiProfileRequest, PortBudget, ProfileName, QueryError, QueryExecutor,
    QueryIr, QueryKind, RecheckPage, Result as QueryResult, SourceReadiness, SourceRechecker,
    build_multi_profile_query, plan_multi_profile,
};
use pgrx::JsonB;
use pgrx::datum::DatumWithOid;
use pgrx::prelude::*;
use serde_json::{Map, Value, json};

use crate::error::{raise_query_error, raise_sql_error};
use crate::table_search::{
    FilterField, FilterPredicatePlan, push_filter_parameter_args, quote_identifier,
    quote_qualified_identifier, resolve_typed_filter_plan,
};

const BRANCH_KEYS: [&str; 5] = ["profile", "configuration_hash", "query", "limit", "weight"];
const CANDIDATE_TRANSIENT_KEY_COPIES: usize = 3;
const PREPARATION_QUERY_COPIES: usize = 4;
const PREPARATION_PROFILE_MAP_RESERVE_BYTES: usize = 4_096;
const PREPARATION_QUERY_NODE_RESERVE_BYTES: usize = 512;
const PREPARATION_FILTER_RESERVE_BYTES: usize = 1024 * 1024;
const REPORT_ROOT_RESERVE_BYTES: usize = 4_096;
const REPORT_BRANCH_RESERVE_BYTES: usize = 2_048;
const REPORT_RESULT_RESERVE_BYTES: usize = 2_048;
const REPORT_CONTRIBUTION_RESERVE_BYTES: usize = 2_048;
const MAX_MULTI_PROFILE_PLAN_DEPTH: usize = 256;
const HNSW_PLAN_JSON_BYTES_PER_INDEX: usize = 4_096;

#[cfg(feature = "pg_test")]
thread_local! {
    static NEXT_COMPARISON_LIMIT: Cell<Option<usize>> = const { Cell::new(None) };
    static NEXT_EXACT_PROFILE: RefCell<Option<String>> = const { RefCell::new(None) };
    static LAST_FILTER_FIELD_COUNT: Cell<usize> = const { Cell::new(0) };
}

fn query_comparison_limit() -> usize {
    #[cfg(feature = "pg_test")]
    if let Some(limit) = NEXT_COMPARISON_LIMIT.with(Cell::take) {
        return limit;
    }
    context_query::DEFAULT_QUERY_COMPARISONS
}

#[cfg(feature = "pg_test")]
pub(crate) fn set_next_comparison_limit_for_test(limit: usize) {
    NEXT_COMPARISON_LIMIT.with(|next| next.set(Some(limit)));
}

#[cfg(feature = "pg_test")]
pub(crate) fn force_next_exact_profile_for_test(profile: &str) {
    NEXT_EXACT_PROFILE.with(|next| *next.borrow_mut() = Some(profile.to_owned()));
}

#[cfg(feature = "pg_test")]
fn take_forced_exact_profile(profile: &ProfileName) -> bool {
    NEXT_EXACT_PROFILE.with(|next| {
        let mut next = next.borrow_mut();
        if next.as_deref() == Some(profile.as_str()) {
            next.take();
            true
        } else {
            false
        }
    })
}

#[cfg(not(feature = "pg_test"))]
const fn take_forced_exact_profile(_profile: &ProfileName) -> bool {
    false
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct IndexPlanIdentity {
    schema: String,
    name: String,
}

#[derive(Clone, Debug)]
struct PreparedProfile {
    registration_revision: i64,
    configuration_hash: u64,
    profile: ProfileName,
    lifecycle: ProfileLifecycle,
    source_schema: String,
    source_table: String,
    source_table_oid: pg_sys::Oid,
    source_key_type_schema: String,
    source_key_type_name: String,
    vector_column: String,
    source_version_column: String,
    embedding_version_column: String,
    representation: String,
    dimensions: i32,
    metric: String,
    index_oid: pg_sys::Oid,
    plan_indexes: Vec<IndexPlanIdentity>,
}

#[derive(Clone, Debug)]
struct CandidateObservation {
    source_key: String,
    source_version: i64,
    approximate_score: f64,
}

#[derive(Clone, Debug)]
struct CandidateIdentity {
    point_id: i64,
    source_key: String,
    source_version: i64,
    approximate_score: f64,
}

#[derive(Clone, Debug, Default)]
struct BranchExecution {
    candidate_count: usize,
    probe_exhausted: bool,
    recheck_count: usize,
    retained_count: usize,
    hnsw_visits: usize,
    exact_fallback: bool,
}

#[derive(Default)]
struct RuntimeState {
    candidates: Vec<CandidateIdentity>,
    branches: BTreeMap<ProfileName, BranchExecution>,
}

/// Executes bounded rank-only fusion over immutable embedding profiles.
///
/// The returned JSON report keeps partial-profile service explicit and retains
/// per-profile native scores only as diagnostics. PostgreSQL applies current
/// ACL, RLS, MVCC, filter, and version predicates during both candidate
/// selection and authoritative recheck.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_multi_model(
    collection: String,
    branches: JsonB,
    filter: Option<JsonB>,
    limit: i32,
    rrf_k: i32,
    unique_candidate_budget: i32,
    require_all_profiles: bool,
) -> JsonB {
    let observation = std::sync::Mutex::new(None);
    PgTryBuilder::new(|| {
        query_multi_model_inner(
            collection,
            branches,
            filter,
            limit,
            rrf_k,
            unique_candidate_budget,
            require_all_profiles,
            &observation,
        )
    })
    .catch_others(|cause| {
        use pgrx::pg_sys::panic::CaughtError;

        let sqlerrcode = match &cause {
            CaughtError::PostgresError(report) | CaughtError::ErrorReport(report) => {
                report.sql_error_code()
            }
            CaughtError::RustPanic { ereport, .. } => ereport.sql_error_code(),
        };
        if let Some(observation) = *observation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            crate::query_stats_async::abort(observation, sqlerrcode as i32);
        }
        cause.rethrow()
    })
    .execute()
}

#[allow(
    clippy::too_many_arguments,
    reason = "the SQL boundary passes the validated request controls plus telemetry state"
)]
fn query_multi_model_inner(
    collection: String,
    branches: JsonB,
    filter: Option<JsonB>,
    limit: i32,
    rrf_k: i32,
    unique_candidate_budget: i32,
    require_all_profiles: bool,
    observation: &std::sync::Mutex<Option<crate::query_stats_async::ObservationToken>>,
) -> JsonB {
    let request = parse_request(
        branches,
        limit,
        rrf_k,
        unique_candidate_budget,
        require_all_profiles,
    );
    let filter_value = filter.as_ref().map(|filter| {
        context_query::validate_filter_json_value(&filter.0)
            .unwrap_or_else(|error| raise_query_error(error));
        filter.0.clone()
    });
    let has_filter = filter_value.is_some();

    let collection_id = crate::lexical_catalog::require_collection_owner_id(&collection);
    let max_elapsed_micros = crate::collection_limits::query_timeout_micros_with_ceiling(
        collection_id,
        context_query::DEFAULT_QUERY_ELAPSED_MICROS,
        context_query::MAX_QUERY_ELAPSED_MICROS,
    );
    let clock = crate::retrieval::PgQueryClock::start();
    let timeout = crate::retrieval::current_statement_timeout::Guard::arm(max_elapsed_micros)
        .unwrap_or_else(|error| raise_query_error(error));
    #[cfg(feature = "pg_test")]
    crate::retrieval::run_query_preparation_delay_probe();
    let source_table_oid = require_source_select(collection_id, &collection);
    let (observed, prepared, attached_index_memory_bytes) =
        prepare_profiles(collection_id, source_table_oid, &request);
    reject_duplicate_source_columns(&prepared);
    for branch in request.branches() {
        if let Some(profile) = prepared.get(branch.profile()) {
            validate_profile_query(profile, branch.query());
        }
    }
    let decision =
        plan_multi_profile(&request, &observed).unwrap_or_else(|error| raise_query_error(error));
    let (selected, coverage) = selected_profiles(decision, &prepared);
    let selected_branches = request
        .branches()
        .iter()
        .filter(|branch| selected.contains_key(branch.profile()))
        .cloned()
        .collect::<Vec<_>>();
    let query = build_multi_profile_query(
        selected_branches,
        filter_value,
        request.rrf_k(),
        request.limit(),
    )
    .unwrap_or_else(|error| raise_query_error(error));
    let base_preparation_memory_bytes = project_preparation_memory(
        &request,
        &prepared,
        attached_index_memory_bytes,
        has_filter,
        0,
    )
    .unwrap_or_else(|error| raise_query_error(error));
    let remaining_filter_memory = context_query::DEFAULT_QUERY_MEMORY_BYTES
        .checked_sub(base_preparation_memory_bytes)
        .unwrap_or_else(|| data_corrupted("multi-profile preparation memory exceeded its limit"));
    let (filter_fields, filter_field_memory_bytes) = load_multi_profile_filter_fields(
        collection_id,
        query.filter_in_subtree(),
        remaining_filter_memory,
    )
    .unwrap_or_else(|error| raise_query_error(error));
    let preparation_memory_bytes =
        admit_preparation_memory(base_preparation_memory_bytes, filter_field_memory_bytes)
            .unwrap_or_else(|error| raise_query_error(error));
    *observation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        crate::query_stats::begin_automatic_query_stat(collection_id, &query, false);

    let state = Rc::new(RefCell::new(RuntimeState::default()));
    let mut candidate_source = MultiProfileCandidateSource {
        collection_id,
        profiles: &selected,
        filter_fields: &filter_fields,
        state: Rc::clone(&state),
    };
    let mut filter_source = MultiProfileFilterSource;
    let filter_port = query
        .has_filter_in_subtree()
        .then_some(&mut filter_source as &mut dyn FilterCandidateSource);
    let mut rechecker = MultiProfileRechecker {
        collection_id,
        profiles: &selected,
        filter_fields: &filter_fields,
        state: Rc::clone(&state),
    };
    let mut telemetry = crate::retrieval::PgTelemetrySink::default();
    let cancellation = crate::retrieval::PgCancellation;
    let max_rechecks = selected
        .keys()
        .filter_map(|profile| {
            request
                .branches()
                .iter()
                .find(|branch| branch.profile() == profile)
        })
        .try_fold(0_usize, |total, branch| total.checked_add(branch.limit()))
        .unwrap_or_else(|| invalid_parameter("multi-model recheck budget overflowed"));
    let execution_memory_bytes = context_query::DEFAULT_QUERY_MEMORY_BYTES
        .checked_sub(preparation_memory_bytes)
        .filter(|remaining| *remaining > 0)
        .ok_or(QueryError::WorkBudgetExceeded {
            budget: "multi_profile_preparation_memory",
            actual: preparation_memory_bytes,
            maximum: context_query::DEFAULT_QUERY_MEMORY_BYTES,
        });
    let budget = execution_memory_bytes.and_then(|execution_memory_bytes| {
        ExecutionBudget::new(
            request.unique_candidate_budget(),
            1,
            max_rechecks.max(1),
            context_core::policy::MAX_QUERY_STAGES,
            1,
            query.max_node_limit(),
        )
        .and_then(|budget| {
            budget.with_resource_limits(
                query_comparison_limit(),
                execution_memory_bytes,
                context_query::DEFAULT_QUERY_HYDRATION_BYTES,
                max_elapsed_micros,
            )
        })
    });
    let outcome = budget.and_then(|budget| {
        QueryExecutor::new(
            &mut candidate_source,
            filter_port,
            &mut rechecker,
            &mut telemetry,
            &cancellation,
        )
        .with_clock(&clock)
        .execute(&query, budget)
    });
    crate::query_stats::record_automatic_query_stat(
        *observation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        telemetry.diagnostics(),
        outcome.as_ref(),
        false,
    );
    let outcome = outcome.unwrap_or_else(|error| raise_query_error(error));
    crate::retrieval::require_complete_outcome(&outcome);
    let report_memory_bytes =
        project_report_memory(&request, &outcome).unwrap_or_else(|error| raise_query_error(error));
    let total_memory_bytes = preparation_memory_bytes
        .checked_add(outcome.usage().memory_bytes())
        .and_then(|bytes| bytes.checked_add(report_memory_bytes))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "multi_profile_total_memory_projection",
        })
        .unwrap_or_else(|error| raise_query_error(error));
    if total_memory_bytes > context_query::DEFAULT_QUERY_MEMORY_BYTES {
        raise_query_error(QueryError::WorkBudgetExceeded {
            budget: "multi_profile_report_memory",
            actual: total_memory_bytes,
            maximum: context_query::DEFAULT_QUERY_MEMORY_BYTES,
        });
    }
    #[cfg(feature = "pg_test")]
    crate::retrieval::run_query_finalization_delay_probe();
    let mut report = build_report(
        &request,
        coverage,
        &selected,
        &state.borrow(),
        &outcome,
        total_memory_bytes,
    );
    crate::retrieval::check_query_interrupt();
    let elapsed_micros = clock.elapsed_micros();
    if elapsed_micros >= max_elapsed_micros {
        raise_query_error(QueryError::WorkBudgetExceeded {
            budget: "multi_profile_elapsed_micros",
            actual: usize::try_from(elapsed_micros).unwrap_or(usize::MAX),
            maximum: usize::try_from(max_elapsed_micros).unwrap_or(usize::MAX),
        });
    }
    report["budget_usage"]["elapsed_micros"] = json!(elapsed_micros);
    timeout.restore();

    JsonB(report)
}

fn parse_request(
    branches: JsonB,
    limit: i32,
    rrf_k: i32,
    unique_candidate_budget: i32,
    require_all_profiles: bool,
) -> MultiProfileRequest {
    let branches = branches
        .0
        .as_array()
        .unwrap_or_else(|| invalid_parameter("branches must be a JSON array"));
    if !(1..=MAX_MULTI_PROFILE_BRANCHES).contains(&branches.len()) {
        invalid_parameter(format!(
            "branches must contain between 1 and {MAX_MULTI_PROFILE_BRANCHES} branches"
        ));
    }
    let aggregate_query_bytes = branches.iter().try_fold(0_usize, |total, branch| {
        let bytes = branch
            .as_object()
            .and_then(|object| object.get("query"))
            .and_then(Value::as_str)
            .map_or(0, str::len);
        total.checked_add(bytes)
    });
    let Some(aggregate_query_bytes) = aggregate_query_bytes else {
        raise_query_error(QueryError::ArithmeticOverflow {
            operation: "multi_profile_query_bytes",
        });
    };
    if aggregate_query_bytes > MAX_MULTI_PROFILE_QUERY_TOTAL_BYTES {
        raise_query_error(QueryError::WorkBudgetExceeded {
            budget: "multi_profile_query_bytes",
            actual: aggregate_query_bytes,
            maximum: MAX_MULTI_PROFILE_QUERY_TOTAL_BYTES,
        });
    }
    validate_request_query_memory(aggregate_query_bytes)
        .unwrap_or_else(|error| raise_query_error(error));
    let parsed = branches.iter().map(parse_branch).collect::<Vec<_>>();
    let limit = positive_usize(limit, "limit");
    let rrf_k = positive_u32(rrf_k, "rrf_k");
    let unique_candidate_budget =
        positive_usize(unique_candidate_budget, "unique_candidate_budget");
    let request = MultiProfileRequest::new(
        parsed,
        rrf_k,
        limit,
        unique_candidate_budget,
        require_all_profiles,
    )
    .unwrap_or_else(|error| raise_query_error(error));
    request_preparation_memory(&request).unwrap_or_else(|error| raise_query_error(error));
    request
}

pub(crate) fn validate_request_query_memory(query_bytes: usize) -> QueryResult<usize> {
    let projected = query_bytes.checked_mul(PREPARATION_QUERY_COPIES).ok_or(
        QueryError::ArithmeticOverflow {
            operation: "multi_profile_request_memory_projection",
        },
    )?;
    if projected >= context_query::DEFAULT_QUERY_MEMORY_BYTES {
        return Err(QueryError::WorkBudgetExceeded {
            budget: "multi_profile_preparation_memory",
            actual: projected,
            maximum: context_query::DEFAULT_QUERY_MEMORY_BYTES,
        });
    }
    Ok(projected)
}

fn project_preparation_memory(
    request: &MultiProfileRequest,
    prepared: &BTreeMap<ProfileName, PreparedProfile>,
    attached_index_memory_bytes: usize,
    has_filter: bool,
    filter_field_memory_bytes: usize,
) -> QueryResult<usize> {
    let mut projected = request_preparation_memory(request)?;
    projected = admit_preparation_memory(
        projected,
        attached_index_memory_bytes
            .checked_mul(2)
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "multi_profile_attached_index_memory_projection",
            })?,
    )?;
    for profile in prepared.values() {
        projected = admit_preparation_memory(
            projected,
            prepared_profile_memory(profile)?.checked_mul(2).ok_or(
                QueryError::ArithmeticOverflow {
                    operation: "multi_profile_profile_memory_projection",
                },
            )?,
        )?;
    }
    if has_filter {
        projected = admit_preparation_memory(
            projected,
            PREPARATION_FILTER_RESERVE_BYTES
                .checked_add(filter_field_memory_bytes)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "multi_profile_filter_memory_projection",
                })?,
        )?;
    }
    Ok(projected)
}

fn request_preparation_memory(request: &MultiProfileRequest) -> QueryResult<usize> {
    let query_bytes = request
        .branches()
        .iter()
        .try_fold(0_usize, |total, branch| {
            total.checked_add(branch.query().len())
        });
    let projected =
        validate_request_query_memory(query_bytes.ok_or(QueryError::ArithmeticOverflow {
            operation: "multi_profile_query_memory_projection",
        })?)?;
    let branch_count = request.branches().len();
    let query_nodes = branch_count
        .checked_mul(2)
        .and_then(|nodes| nodes.checked_add(1))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "multi_profile_query_node_projection",
        })?;
    projected
        .checked_add(
            branch_count
                .checked_mul(
                    size_of::<MultiProfileBranch>()
                        .checked_mul(PREPARATION_QUERY_COPIES)
                        .and_then(|bytes| bytes.checked_add(PREPARATION_PROFILE_MAP_RESERVE_BYTES))
                        .ok_or(QueryError::ArithmeticOverflow {
                            operation: "multi_profile_branch_memory_projection",
                        })?,
                )
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "multi_profile_branch_memory_projection",
                })?,
        )
        .and_then(|bytes| {
            bytes.checked_add(
                query_nodes
                    .checked_mul(size_of::<QueryIr>() + PREPARATION_QUERY_NODE_RESERVE_BYTES)?,
            )
        })
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "multi_profile_preparation_memory_projection",
        })
        .and_then(|projected| admit_preparation_memory(0, projected))
}

fn prepared_profile_memory(profile: &PreparedProfile) -> QueryResult<usize> {
    let dynamic_bytes = [
        profile.profile.as_str(),
        &profile.source_schema,
        &profile.source_table,
        &profile.source_key_type_schema,
        &profile.source_key_type_name,
        &profile.vector_column,
        &profile.source_version_column,
        &profile.embedding_version_column,
        &profile.representation,
        &profile.metric,
    ]
    .into_iter()
    .try_fold(0_usize, |total, value| total.checked_add(value.len()))
    .ok_or(QueryError::ArithmeticOverflow {
        operation: "multi_profile_profile_string_projection",
    })?;
    size_of::<PreparedProfile>()
        .checked_add(dynamic_bytes)
        .and_then(|bytes| bytes.checked_add(PREPARATION_PROFILE_MAP_RESERVE_BYTES))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "multi_profile_profile_memory_projection",
        })
}

fn admit_preparation_memory(current: usize, additional: usize) -> QueryResult<usize> {
    let projected = current
        .checked_add(additional)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "multi_profile_preparation_memory_projection",
        })?;
    if projected >= context_query::DEFAULT_QUERY_MEMORY_BYTES {
        return Err(QueryError::WorkBudgetExceeded {
            budget: "multi_profile_preparation_memory",
            actual: projected,
            maximum: context_query::DEFAULT_QUERY_MEMORY_BYTES,
        });
    }
    Ok(projected)
}

fn project_report_memory(
    request: &MultiProfileRequest,
    outcome: &ExecutionOutcome,
) -> QueryResult<usize> {
    let mut projected = REPORT_ROOT_RESERVE_BYTES
        .checked_add(
            request
                .branches()
                .len()
                .checked_mul(REPORT_BRANCH_RESERVE_BYTES)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "multi_profile_report_branch_projection",
                })?,
        )
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "multi_profile_report_memory_projection",
        })?;
    for point in outcome.points() {
        projected = projected
            .checked_add(REPORT_RESULT_RESERVE_BYTES)
            .and_then(|bytes| bytes.checked_add(point.source_key().as_str().len().checked_mul(2)?))
            .and_then(|bytes| {
                bytes.checked_add(
                    point
                        .contributions()
                        .len()
                        .checked_mul(REPORT_CONTRIBUTION_RESERVE_BYTES)?,
                )
            })
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "multi_profile_report_result_projection",
            })?;
    }
    Ok(projected)
}

fn parse_branch(value: &Value) -> MultiProfileBranch {
    let object = value
        .as_object()
        .unwrap_or_else(|| invalid_parameter("each multi-model branch must be a JSON object"));
    reject_unknown_branch_keys(object);
    let profile = required_string(object, "profile");
    if let Err(reason) = ProfileName::validate(profile) {
        invalid_parameter(format!("invalid profile name: {reason}"));
    }
    let profile = ProfileName::new(profile.to_owned())
        .unwrap_or_else(|_| data_corrupted("validated profile name could not be constructed"));
    let configuration_hash = required_string(object, "configuration_hash");
    if configuration_hash.len() != 16
        || configuration_hash
            .bytes()
            .any(|byte| !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase())
    {
        invalid_parameter("configuration_hash must be 16 lowercase hex digits");
    }
    let configuration_hash = u64::from_str_radix(configuration_hash, 16).unwrap_or_else(|_| {
        invalid_parameter("configuration_hash must be 16 lowercase hex digits")
    });
    let query = required_string(object, "query").to_owned();
    let limit = object
        .get("limit")
        .and_then(Value::as_i64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_else(|| invalid_parameter("branch limit must be a positive integer"));
    let weight = object
        .get("weight")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| invalid_parameter("branch weight must be numeric"));
    MultiProfileBranch::new(profile, configuration_hash, query, limit, weight)
        .unwrap_or_else(|error| raise_query_error(error))
}

fn reject_unknown_branch_keys(object: &Map<String, Value>) {
    let allowed = BRANCH_KEYS.into_iter().collect::<BTreeSet<_>>();
    if object.keys().any(|key| !allowed.contains(key.as_str())) {
        invalid_parameter("unsupported multi-model branch field");
    }
}

fn required_string<'a>(object: &'a Map<String, Value>, key: &str) -> &'a str {
    object
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_else(|| invalid_parameter(format!("multi-model branch {key} must be a string")))
}

fn positive_usize(value: i32, field: &'static str) -> usize {
    usize::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or_else(|| invalid_parameter(format!("{field} must be a positive integer")))
}

fn positive_u32(value: i32, field: &'static str) -> u32 {
    u32::try_from(value)
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or_else(|| invalid_parameter(format!("{field} must be a positive integer")))
}

fn require_source_select(collection_id: i64, collection: &str) -> pg_sys::Oid {
    let row = Spi::connect(|client| {
        let rows = client.select(
            "SELECT source_table_oid,
                    pg_catalog.has_table_privilege(SESSION_USER, source_table_oid, 'SELECT')
               FROM pgcontext._visible_collections
              WHERE collection_id = $1",
            Some(1),
            &[collection_id.into()],
        )?;
        if rows.is_empty() {
            invalid_parameter(format!("collection has no source table: {collection}"));
        }
        let row = rows.first();
        Ok::<_, spi::Error>((
            required(row.get::<pg_sys::Oid>(1)?, "source_table_oid"),
            required(row.get::<bool>(2)?, "source SELECT privilege"),
        ))
    })
    .unwrap_or_else(|error| internal(format!("failed to validate source privileges: {error}")));
    if !row.1 {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!("permission denied for source table of collection {collection}"),
        );
    }
    row.0
}

fn prepare_profiles(
    collection_id: i64,
    source_table_oid: pg_sys::Oid,
    request: &MultiProfileRequest,
) -> (
    Vec<MultiProfileObserved>,
    BTreeMap<ProfileName, PreparedProfile>,
    usize,
) {
    let mut observed = Vec::with_capacity(request.branches().len());
    let mut prepared = BTreeMap::new();
    let mut attached_index_count = 0_usize;
    let mut preparation_memory_bytes =
        request_preparation_memory(request).unwrap_or_else(|error| raise_query_error(error));
    let mut attached_index_memory_bytes = 0_usize;
    for branch in request.branches() {
        let Some((mut profile, configuration_hash, mut binding_ready)) =
            load_profile(collection_id, source_table_oid, branch.profile())
        else {
            continue;
        };
        preparation_memory_bytes = admit_preparation_memory(
            preparation_memory_bytes,
            prepared_profile_memory(&profile)
                .and_then(|bytes| {
                    bytes.checked_mul(2).ok_or(QueryError::ArithmeticOverflow {
                        operation: "multi_profile_profile_memory_projection",
                    })
                })
                .unwrap_or_else(|error| raise_query_error(error)),
        )
        .unwrap_or_else(|error| raise_query_error(error));
        if binding_ready {
            let remaining_indexes = context_core::policy::MAX_MULTI_PROFILE_ATTACHED_INDEXES
                .checked_sub(attached_index_count)
                .ok_or(QueryError::WorkBudgetExceeded {
                    budget: "multi_profile_attached_indexes",
                    actual: attached_index_count,
                    maximum: context_core::policy::MAX_MULTI_PROFILE_ATTACHED_INDEXES,
                })
                .unwrap_or_else(|error| raise_query_error(error));
            if let Some(plan_indexes) = load_hnsw_plan_indexes(profile.index_oid, remaining_indexes)
                .unwrap_or_else(|error| raise_query_error(error))
            {
                attached_index_count = attached_index_count
                    .checked_add(plan_indexes.len())
                    .ok_or(QueryError::ArithmeticOverflow {
                        operation: "multi_profile_attached_index_projection",
                    })
                    .unwrap_or_else(|error| raise_query_error(error));
                let index_slots = plan_indexes
                    .capacity()
                    .checked_mul(size_of::<IndexPlanIdentity>())
                    .ok_or(QueryError::ArithmeticOverflow {
                        operation: "multi_profile_attached_index_memory_projection",
                    })
                    .unwrap_or_else(|error| raise_query_error(error));
                let profile_index_bytes = plan_indexes
                    .iter()
                    .try_fold(index_slots, |total, index| {
                        total
                            .checked_add(index.schema.capacity())
                            .and_then(|bytes| bytes.checked_add(index.name.capacity()))
                            .ok_or(QueryError::ArithmeticOverflow {
                                operation: "multi_profile_attached_index_memory_projection",
                            })
                    })
                    .unwrap_or_else(|error| raise_query_error(error));
                attached_index_memory_bytes = attached_index_memory_bytes
                    .checked_add(profile_index_bytes)
                    .ok_or(QueryError::ArithmeticOverflow {
                        operation: "multi_profile_preparation_memory_projection",
                    })
                    .unwrap_or_else(|error| raise_query_error(error));
                preparation_memory_bytes = admit_preparation_memory(
                    preparation_memory_bytes,
                    profile_index_bytes
                        .checked_mul(2)
                        .ok_or(QueryError::ArithmeticOverflow {
                            operation: "multi_profile_attached_index_memory_projection",
                        })
                        .unwrap_or_else(|error| raise_query_error(error)),
                )
                .unwrap_or_else(|error| raise_query_error(error));
                profile.plan_indexes = plan_indexes;
            } else {
                binding_ready = false;
            }
        }
        let version_bindings_present = !profile.source_version_column.is_empty()
            && !profile.embedding_version_column.is_empty();
        observed.push(MultiProfileObserved::new(
            profile.profile.clone(),
            configuration_hash,
            profile.lifecycle,
            version_bindings_present,
            binding_ready,
        ));
        prepared.insert(profile.profile.clone(), profile);
    }
    (observed, prepared, attached_index_memory_bytes)
}

fn load_profile(
    collection_id: i64,
    source_table_oid: pg_sys::Oid,
    profile_name: &ProfileName,
) -> Option<(PreparedProfile, u64, bool)> {
    Spi::connect(|client| {
        let rows = client.select(
            "SELECT profiles.embedding_profile_id,
                    profiles.lifecycle,
                    profiles.configuration_hash,
                    profiles.source_schema_name,
                    profiles.source_table_name,
                    profiles.source_column_name,
                    coalesce(profiles.source_version_column_name, ''),
                    coalesce(profiles.embedding_version_column_name, ''),
                    profiles.representation,
                    profiles.dimensions,
                    profiles.metric,
                    pg_catalog.to_regclass(pg_catalog.format(
                        '%I.%I', profiles.hnsw_schema_name, profiles.hnsw_index_name
                    )),
                    profiles.hnsw_index_name,
                    coalesce(source_key_type_namespace.nspname, '')::pg_catalog.text,
                    coalesce(source_key_type.typname, '')::pg_catalog.text,
                    EXISTS (
                        SELECT 1
                          FROM pg_catalog.pg_class AS source_table
                          JOIN pg_catalog.pg_namespace AS source_namespace
                            ON source_namespace.oid = source_table.relnamespace
                          JOIN pg_catalog.pg_attribute AS vector_attribute
                            ON vector_attribute.attrelid = source_table.oid
                           AND vector_attribute.attnum = profiles.source_attnum
                           AND vector_attribute.attname = profiles.source_column_name
                           AND vector_attribute.atttypmod = profiles.source_typmod
                           AND NOT vector_attribute.attisdropped
                          JOIN pg_catalog.pg_attribute AS source_key_attribute
                            ON source_key_attribute.attrelid = source_table.oid
                           AND source_key_attribute.attname = 'id'
                           AND source_key_attribute.attnum > 0
                           AND NOT source_key_attribute.attisdropped
                          JOIN pg_catalog.pg_index AS source_key_index
                            ON source_key_index.indrelid = source_table.oid
                           AND source_key_index.indisprimary
                           AND source_key_index.indisvalid
                           AND source_key_index.indisready
                           AND source_key_index.indislive
                           AND source_key_index.indnkeyatts = 1
                           AND source_key_index.indnatts = 1
                           AND source_key_index.indkey[0] = source_key_attribute.attnum
                          JOIN pg_catalog.pg_type AS vector_type
                            ON vector_type.oid = vector_attribute.atttypid
                           AND vector_type.typname = profiles.source_type_name
                          JOIN pg_catalog.pg_namespace AS vector_namespace
                            ON vector_namespace.oid = vector_type.typnamespace
                           AND vector_namespace.nspname = 'pgcontext'
                          JOIN pg_catalog.pg_attribute AS source_version_attribute
                            ON source_version_attribute.attrelid = source_table.oid
                           AND source_version_attribute.attnum = profiles.source_version_attnum
                           AND source_version_attribute.attname = profiles.source_version_column_name
                           AND source_version_attribute.atttypid = 'pg_catalog.int8'::regtype
                           AND NOT source_version_attribute.attisdropped
                          JOIN pg_catalog.pg_attribute AS embedding_version_attribute
                            ON embedding_version_attribute.attrelid = source_table.oid
                           AND embedding_version_attribute.attnum = profiles.embedding_version_attnum
                           AND embedding_version_attribute.attname = profiles.embedding_version_column_name
                           AND embedding_version_attribute.atttypid = 'pg_catalog.int8'::regtype
                           AND NOT embedding_version_attribute.attisdropped
                          JOIN pg_catalog.pg_class AS index_class
                            ON index_class.oid = pg_catalog.to_regclass(pg_catalog.format(
                                '%I.%I', profiles.hnsw_schema_name, profiles.hnsw_index_name
                            ))
                          JOIN pg_catalog.pg_index AS index_catalog
                            ON index_catalog.indexrelid = index_class.oid
                           AND index_catalog.indrelid = source_table.oid
                           AND index_catalog.indkey[0] = profiles.source_attnum
                           AND index_catalog.indnkeyatts = 1
                           AND index_catalog.indnatts = 1
                           AND index_catalog.indexprs IS NULL
                           AND index_catalog.indpred IS NULL
                           AND index_catalog.indisvalid
                           AND index_catalog.indisready
                           AND index_catalog.indislive
                          JOIN pg_catalog.pg_am AS access_method
                            ON access_method.oid = index_class.relam
                           AND access_method.amname = 'pgcontext_hnsw'
                          JOIN pg_catalog.pg_opclass AS opclass
                            ON opclass.oid = index_catalog.indclass[0]
                           AND opclass.opcname = profiles.hnsw_opclass
                          JOIN pg_catalog.pg_namespace AS opclass_namespace
                            ON opclass_namespace.oid = opclass.opcnamespace
                           AND opclass_namespace.nspname = 'pgcontext'
                         WHERE source_table.oid = $3
                           AND source_namespace.nspname = profiles.source_schema_name
                           AND source_table.relname = profiles.source_table_name
                    ) AS binding_ready
               FROM pgcontext._visible_embedding_profiles AS profiles
               LEFT JOIN pg_catalog.pg_attribute AS source_key_attribute
                 ON source_key_attribute.attrelid = $3
                AND source_key_attribute.attname = 'id'
                AND source_key_attribute.attnum > 0
                AND NOT source_key_attribute.attisdropped
               LEFT JOIN pg_catalog.pg_type AS source_key_type
                 ON source_key_type.oid = source_key_attribute.atttypid
               LEFT JOIN pg_catalog.pg_namespace AS source_key_type_namespace
                 ON source_key_type_namespace.oid = source_key_type.typnamespace
              WHERE profiles.collection_id = $1
                AND profiles.profile_name = $2",
            Some(1),
            &[
                collection_id.into(),
                profile_name.as_str().into(),
                source_table_oid.into(),
            ],
        )?;
        if rows.is_empty() {
            return Ok::<_, spi::Error>(None);
        }
        let row = rows.first();
        let lifecycle_name = required(row.get::<String>(2)?, "profile lifecycle");
        let lifecycle = ProfileLifecycle::parse(&lifecycle_name)
            .unwrap_or_else(|error| data_corrupted(error.to_string()));
        let configuration_hash = required(row.get::<String>(3)?, "configuration_hash");
        let configuration_hash = u64::from_str_radix(&configuration_hash, 16)
            .unwrap_or_else(|_| data_corrupted("stored configuration hash is invalid"));
        let index_oid = row.get::<pg_sys::Oid>(12)?.unwrap_or(pg_sys::InvalidOid);
        let _index_name = required(row.get::<String>(13)?, "HNSW index name");
        let source_key_type_schema = required(row.get::<String>(14)?, "source key type schema");
        let source_key_type_name = required(row.get::<String>(15)?, "source key type name");
        let binding_ready = required(row.get::<bool>(16)?, "profile binding readiness")
            && index_oid != pg_sys::InvalidOid;
        Ok(Some((
            PreparedProfile {
                registration_revision: required(
                    row.get::<i64>(1)?,
                    "embedding_profile_id",
                ),
                configuration_hash,
                profile: profile_name.clone(),
                lifecycle,
                source_schema: required(row.get::<String>(4)?, "source schema"),
                source_table: required(row.get::<String>(5)?, "source table"),
                source_table_oid,
                source_key_type_schema,
                source_key_type_name,
                vector_column: required(row.get::<String>(6)?, "source vector column"),
                source_version_column: required(
                    row.get::<String>(7)?,
                    "source version column",
                ),
                embedding_version_column: required(
                    row.get::<String>(8)?,
                    "embedding version column",
                ),
                representation: required(row.get::<String>(9)?, "representation"),
                dimensions: required(row.get::<i32>(10)?, "dimensions"),
                metric: required(row.get::<String>(11)?, "metric"),
                index_oid,
                plan_indexes: Vec::new(),
            },
            configuration_hash,
            binding_ready,
        )))
    })
    .unwrap_or_else(|error| internal(format!("failed to prepare embedding profile: {error}")))
}

fn load_hnsw_plan_indexes(
    index_oid: pg_sys::Oid,
    remaining_indexes: usize,
) -> QueryResult<Option<Vec<IndexPlanIdentity>>> {
    let probe_limit = remaining_indexes
        .checked_add(1)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "multi_profile_attached_index_probe",
        })?;
    let sql_limit = i64::try_from(probe_limit).map_err(|_| QueryError::ArithmeticOverflow {
        operation: "multi_profile_attached_index_probe",
    })?;
    let rows = Spi::connect(|client| {
        let rows = client.select(
            "WITH RECURSIVE index_tree(index_oid) AS (
                 SELECT $1::oid
                 UNION ALL
                 SELECT inheritance.inhrelid
                   FROM pg_catalog.pg_inherits AS inheritance
                   JOIN index_tree AS parent
                     ON parent.index_oid = inheritance.inhparent
             )
             SELECT namespace.nspname::pg_catalog.text,
                    index_class.relname::pg_catalog.text,
                    index_catalog.indisvalid
                        AND index_catalog.indisready
                        AND index_catalog.indislive
                        AND access_method.amname = 'pgcontext_hnsw'
               FROM index_tree
               JOIN pg_catalog.pg_class AS index_class
                 ON index_class.oid = index_tree.index_oid
               JOIN pg_catalog.pg_namespace AS namespace
                 ON namespace.oid = index_class.relnamespace
               JOIN pg_catalog.pg_index AS index_catalog
                 ON index_catalog.indexrelid = index_class.oid
               JOIN pg_catalog.pg_am AS access_method
                 ON access_method.oid = index_class.relam",
            Some(sql_limit),
            &[index_oid.into()],
        )?;
        let mut indexes = Vec::new();
        for row in rows {
            indexes.push((
                IndexPlanIdentity {
                    schema: required(row.get::<String>(1)?, "HNSW index schema"),
                    name: required(row.get::<String>(2)?, "HNSW index name"),
                },
                required(row.get::<bool>(3)?, "HNSW index readiness"),
            ));
        }
        Ok::<_, spi::Error>(indexes)
    })
    .map_err(|_| QueryError::PortFailure {
        stage: "multi_profile_preparation",
        message: "failed to resolve bounded HNSW index hierarchy".to_owned(),
    })?;
    validate_attached_index_count(rows.len(), remaining_indexes)?;
    let all_ready = rows.iter().all(|(_, ready)| *ready);
    let mut indexes = rows.into_iter().map(|(index, _)| index).collect::<Vec<_>>();
    indexes.sort_unstable();
    indexes.dedup();
    Ok((!indexes.is_empty() && all_ready).then_some(indexes))
}

pub(crate) fn validate_attached_index_count(observed: usize, remaining: usize) -> QueryResult<()> {
    if observed > remaining {
        let already_attached =
            context_core::policy::MAX_MULTI_PROFILE_ATTACHED_INDEXES.saturating_sub(remaining);
        return Err(QueryError::WorkBudgetExceeded {
            budget: "multi_profile_attached_indexes",
            actual: already_attached.saturating_add(observed),
            maximum: context_core::policy::MAX_MULTI_PROFILE_ATTACHED_INDEXES,
        });
    }
    Ok(())
}

#[cfg(feature = "pg_test")]
pub(crate) fn project_single_index_profile_capacity_for_test(
    profile_count: usize,
) -> QueryResult<usize> {
    let mut total = 0_usize;
    for _ in 0..profile_count {
        let mut indexes = Vec::new();
        indexes.push(IndexPlanIdentity {
            schema: "s".to_owned(),
            name: "i".to_owned(),
        });
        let bytes = indexes
            .capacity()
            .checked_mul(size_of::<IndexPlanIdentity>())
            .and_then(|slots| slots.checked_add(2))
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "multi_profile_attached_index_memory_projection",
            })?;
        total = admit_preparation_memory(total, bytes)?;
    }
    Ok(total)
}

fn selected_profiles(
    decision: MultiProfileDecision,
    prepared: &BTreeMap<ProfileName, PreparedProfile>,
) -> (BTreeMap<ProfileName, PreparedProfile>, MultiProfileCoverage) {
    match decision {
        MultiProfileDecision::FailClosed { missing } => fail_unavailable(&missing),
        MultiProfileDecision::Serve { branches, coverage } => {
            let selected = branches
                .into_iter()
                .map(|branch| {
                    let profile = prepared.get(branch.profile()).cloned().unwrap_or_else(|| {
                        data_corrupted("profile selection lost a prepared catalog binding")
                    });
                    (branch.profile().clone(), profile)
                })
                .collect();
            (selected, coverage)
        }
    }
}

fn fail_unavailable(missing: &[MissingProfile]) -> ! {
    let first = missing
        .first()
        .unwrap_or_else(|| data_corrupted("profile selection failed without a reason"));
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
        format!(
            "multi-model query requires unavailable profile {}: {}",
            first.profile().as_str(),
            missing_reason_name(first.reason())
        ),
    )
}

fn reject_duplicate_source_columns(selected: &BTreeMap<ProfileName, PreparedProfile>) {
    let mut columns = BTreeSet::new();
    for profile in selected.values() {
        if !columns.insert(profile.vector_column.as_str()) {
            invalid_parameter("multi-model branches must use distinct source columns");
        }
    }
}

fn validate_profile_query(profile: &PreparedProfile, query: &str) {
    let cast = query_cast(profile);
    Spi::get_one_with_args::<String>(&format!("SELECT ({cast})::text"), &[query.into()])
        .unwrap_or_else(|_| {
            invalid_parameter(format!(
                "query does not match profile {}",
                profile.profile.as_str()
            ))
        })
        .unwrap_or_else(|| data_corrupted("profile query validation returned null"));
}

struct MultiProfileFilterSource;

impl FilterCandidateSource for MultiProfileFilterSource {
    fn candidate_limit(
        &mut self,
        _query: &QueryIr,
        _remaining: usize,
        _budget: PortBudget,
    ) -> QueryResult<usize> {
        Ok(1)
    }

    fn filter_candidates(
        &mut self,
        _query: &QueryIr,
        _limit: usize,
        _budget: PortBudget,
    ) -> QueryResult<FilterCandidateBatch> {
        // The profile adapter binds the same typed predicate into both its
        // bounded HNSW admission and authoritative reread. No corpus-sized ID
        // mask is materialized merely to express filter pushdown.
        Ok(FilterCandidateBatch::new(Vec::new(), 0, true))
    }
}

struct MultiProfileCandidateSource<'a> {
    collection_id: i64,
    profiles: &'a BTreeMap<ProfileName, PreparedProfile>,
    filter_fields: &'a [FilterField],
    state: Rc<RefCell<RuntimeState>>,
}

impl CandidateSource for MultiProfileCandidateSource<'_> {
    fn readiness(&mut self, query: &QueryIr, _budget: PortBudget) -> QueryResult<SourceReadiness> {
        let (profile_name, configuration_hash, _) = profile_leaf(query)?;
        let profile = prepared_profile(self.profiles, profile_name)?;
        if configuration_hash != configuration_revision(profile)?.get() {
            return Err(QueryError::PortFailure {
                stage: "multi_profile_readiness",
                message: "prepared profile configuration changed before execution".to_owned(),
            });
        }
        Ok(SourceReadiness::Ready)
    }

    fn candidate_limit(
        &mut self,
        query: &QueryIr,
        remaining: usize,
        _budget: PortBudget,
    ) -> QueryResult<usize> {
        query
            .limit()
            .checked_add(1)
            .map(|limit| limit.min(remaining))
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "multi_profile_probe_limit",
            })
    }

    fn candidates(
        &mut self,
        query: &QueryIr,
        _filter: Option<&FilterCandidateBatch>,
        limit: usize,
        budget: PortBudget,
    ) -> QueryResult<CandidatePage> {
        let (profile_name, _, provider_query) = profile_leaf(query)?;
        let profile = prepared_profile(self.profiles, profile_name)?;
        self.state.borrow_mut().candidates.clear();
        let filter = filter_plan_result(query.filter(), self.filter_fields, 2)?;
        let (observations, candidate_count, hnsw_visits, comparisons, exact_fallback) =
            candidate_observations(profile, provider_query, limit, filter.as_ref(), budget)?;
        let observation_count = observations.len();
        let identities = resolve_candidate_identities(self.collection_id, observations)?;
        if identities.len() != observation_count {
            return Err(QueryError::PortFailure {
                stage: "multi_profile_candidate_mapping",
                message: format!(
                    "profile {} candidate mapping changed during execution",
                    profile.profile.as_str()
                ),
            });
        }
        let configuration = configuration_revision(profile)?;
        let profile_id = profile_id(profile)?;
        let mut candidates = Vec::with_capacity(query.limit().min(identities.len()));
        let mut state = self.state.borrow_mut();
        let adapter = if exact_fallback {
            crate::retrieval::CandidateAdapter::Exact
        } else {
            crate::retrieval::CandidateAdapter::Hnsw
        };
        state
            .candidates
            .reserve(query.limit().min(identities.len()));
        for (rank, identity) in identities.into_iter().take(query.limit()).enumerate() {
            let point_id = point_id(identity.point_id)?;
            let source_version = source_version(identity.source_version)?;
            let rank = u32::try_from(rank).map_err(|_| QueryError::ArithmeticOverflow {
                operation: "multi_profile_candidate_rank",
            })?;
            let candidate = Candidate::new(
                point_id,
                identity.approximate_score,
                crate::retrieval::profile_candidate_provenance(
                    point_id,
                    configuration,
                    profile_id,
                    source_version,
                    adapter,
                    exact_source_authority(profile)?,
                )?,
            )?
            .with_diagnostics(CandidateDiagnostics::new(rank, 1));
            state.candidates.push(identity);
            candidates.push(candidate);
        }
        let retained_slots = state
            .candidates
            .capacity()
            .checked_mul(size_of::<CandidateIdentity>())
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "multi_profile_retained_candidate_memory",
            })?;
        let retained_memory_bytes =
            state
                .candidates
                .iter()
                .try_fold(retained_slots, |total, identity| {
                    total.checked_add(identity.source_key.capacity()).ok_or(
                        QueryError::ArithmeticOverflow {
                            operation: "multi_profile_retained_candidate_memory",
                        },
                    )
                })?;
        state.branches.insert(
            profile.profile.clone(),
            BranchExecution {
                candidate_count,
                probe_exhausted: candidate_count <= query.limit(),
                hnsw_visits,
                exact_fallback,
                ..BranchExecution::default()
            },
        );
        let strategy = if exact_fallback {
            "multi_profile_exact_fallback"
        } else {
            "multi_profile_hnsw"
        };
        Ok(
            CandidatePage::with_scored_count(candidates, comparisons, true)
                .with_candidate_work_count(candidate_count)
                .with_retained_memory_bytes(retained_memory_bytes)
                .with_strategy(strategy),
        )
    }
}

struct MultiProfileRechecker<'a> {
    collection_id: i64,
    profiles: &'a BTreeMap<ProfileName, PreparedProfile>,
    filter_fields: &'a [FilterField],
    state: Rc<RefCell<RuntimeState>>,
}

impl SourceRechecker for MultiProfileRechecker<'_> {
    fn recheck(
        &mut self,
        query: &QueryIr,
        candidates: &[Candidate],
        limit: usize,
        budget: PortBudget,
    ) -> QueryResult<RecheckPage> {
        let (profile_name, _, provider_query) = profile_leaf(query)?;
        let profile = prepared_profile(self.profiles, profile_name)?;
        let mut state = self.state.borrow_mut();
        let cached = std::mem::take(&mut state.candidates);
        let identities = candidates
            .iter()
            .take(limit)
            .zip(cached)
            .map(|(candidate, identity)| {
                if u64::try_from(identity.point_id).ok() != Some(candidate.point_id().get()) {
                    return Err(QueryError::UnexpectedPointId {
                        stage: "multi_profile_recheck_cache",
                        point_id: candidate.point_id(),
                    });
                }
                Ok(identity)
            })
            .collect::<QueryResult<Vec<_>>>()?;
        if identities.len() != candidates.len().min(limit) {
            return Err(QueryError::InvariantViolation {
                operation: "multi_profile_recheck_cache_cardinality",
            });
        }
        drop(state);
        let filter = filter_plan_result(query.filter(), self.filter_fields, 6)?;
        let rows = authoritative_recheck(
            self.collection_id,
            profile,
            provider_query,
            &identities,
            limit,
            filter.as_ref(),
            budget,
        )?;
        let mut state = self.state.borrow_mut();
        let execution =
            state
                .branches
                .get_mut(profile_name)
                .ok_or(QueryError::InvariantViolation {
                    operation: "multi_profile_branch_execution_join",
                })?;
        execution.recheck_count = identities.len();
        execution.retained_count = rows.len();
        Ok(RecheckPage::new(rows, identities.len()))
    }
}

fn profile_leaf(query: &QueryIr) -> QueryResult<(&ProfileName, u64, &str)> {
    match query.kind() {
        QueryKind::ProfileNearest {
            profile,
            configuration_hash,
            query,
        } => Ok((profile, *configuration_hash, query.as_str())),
        _ => Err(QueryError::PortFailure {
            stage: "multi_profile_router",
            message: "non-profile leaf reached the multi-profile adapter".to_owned(),
        }),
    }
}

fn prepared_profile<'a>(
    profiles: &'a BTreeMap<ProfileName, PreparedProfile>,
    profile: &ProfileName,
) -> QueryResult<&'a PreparedProfile> {
    profiles.get(profile).ok_or(QueryError::PortFailure {
        stage: "multi_profile_router",
        message: "selected profile binding disappeared before execution".to_owned(),
    })
}

fn configuration_revision(profile: &PreparedProfile) -> QueryResult<ConfigurationRevision> {
    ConfigurationRevision::new(profile.configuration_hash).ok_or(QueryError::PortFailure {
        stage: "multi_profile_configuration",
        message: "stored immutable profile configuration is zero".to_owned(),
    })
}

fn profile_id(profile: &PreparedProfile) -> QueryResult<ProfileId> {
    u64::try_from(profile.registration_revision)
        .ok()
        .and_then(ProfileId::new)
        .ok_or(QueryError::PortFailure {
            stage: "multi_profile_profile_id",
            message: "stored profile identity is not positive".to_owned(),
        })
}

fn exact_source_authority(profile: &PreparedProfile) -> QueryResult<context_core::SourceAuthority> {
    match profile.representation.as_str() {
        "dense" | "half" | "sparse" => Ok(context_core::SourceAuthority::PostgreSqlRow),
        "bit" | "int8" | "uint8" => Ok(context_core::SourceAuthority::ProviderNative),
        _ => Err(QueryError::PortFailure {
            stage: "multi_profile_source_authority",
            message: "stored embedding profile representation is invalid".to_owned(),
        }),
    }
}

fn source_version(value: i64) -> QueryResult<SourceVersion> {
    u64::try_from(value)
        .ok()
        .and_then(SourceVersion::new)
        .ok_or(QueryError::PortFailure {
            stage: "multi_profile_source_version",
            message: "authoritative source version is not positive".to_owned(),
        })
}

fn point_id(value: i64) -> QueryResult<PointId> {
    PointId::from_i64(value).ok_or(QueryError::PortFailure {
        stage: "multi_profile_point_id",
        message: "candidate point identity is not positive".to_owned(),
    })
}

fn filter_plan_result(
    filter: Option<&context_filter::Filter>,
    fields: &[FilterField],
    placeholder_offset: usize,
) -> QueryResult<Option<FilterPredicatePlan>> {
    filter
        .map(|filter| {
            resolve_typed_filter_plan(fields, filter, placeholder_offset).map_err(|error| {
                QueryError::PortFailure {
                    stage: "multi_profile_filter",
                    message: error.to_string(),
                }
            })
        })
        .transpose()
}

fn load_multi_profile_filter_fields(
    collection_id: i64,
    filter: Option<&context_filter::Filter>,
    remaining_memory_bytes: usize,
) -> QueryResult<(Vec<FilterField>, usize)> {
    let Some(filter) = filter else {
        #[cfg(feature = "pg_test")]
        LAST_FILTER_FIELD_COUNT.with(|count| count.set(0));
        return Ok((Vec::new(), 0));
    };
    let keys = filter
        .field_keys()
        .into_iter()
        .map(|key| key.as_str().to_owned())
        .collect::<Vec<_>>();
    if keys.is_empty() || keys.len() > context_core::policy::MAX_FILTER_NODES {
        return Err(QueryError::InvariantViolation {
            operation: "multi_profile_filter_key_projection",
        });
    }
    let requested_keys = keys.len();
    let admission_bytes = filter_field_admission_memory(requested_keys)?;
    if admission_bytes > remaining_memory_bytes {
        return Err(QueryError::WorkBudgetExceeded {
            budget: "multi_profile_filter_catalog_memory",
            actual: admission_bytes,
            maximum: remaining_memory_bytes,
        });
    }
    let probe_limit = keys
        .len()
        .checked_add(1)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "multi_profile_filter_field_probe",
        })?;
    let row_limit = i64::try_from(probe_limit).map_err(|_| QueryError::ArithmeticOverflow {
        operation: "multi_profile_filter_field_probe",
    })?;
    let fields = Spi::connect(|client| {
        let rows = client
            .select(
                &format!(
                    "SELECT fields.filter_key,
                            fields.column_name,
                            coalesce(pg_catalog.cardinality(fields.jsonb_path), 0),
                            bounds.path_bytes,
                            CASE WHEN coalesce(pg_catalog.cardinality(fields.jsonb_path), 0) <= {}
                                      AND bounds.path_bytes <= {}
                                 THEN fields.jsonb_path
                            END
                       FROM pgcontext._visible_collection_payload_columns AS fields
                       CROSS JOIN LATERAL (
                           SELECT coalesce(
                                      pg_catalog.sum(pg_catalog.octet_length(segment)), 0
                                  )::bigint AS path_bytes
                             FROM pg_catalog.unnest(fields.jsonb_path) AS segment
                       ) AS bounds
                      WHERE fields.collection_id = $1
                        AND fields.filter_key = ANY($2::text[])
                      ORDER BY fields.filter_key",
                    context_core::policy::MAX_FILTER_PATH_DEPTH,
                    context_core::policy::MAX_FILTER_PATH_BYTES
                ),
                Some(row_limit),
                &[collection_id.into(), keys.into()],
            )
            .map_err(|_| QueryError::PortFailure {
                stage: "multi_profile_filter_catalog",
                message: "bounded filter-field lookup failed".to_owned(),
            })?;
        let mut fields = Vec::with_capacity(probe_limit.min(rows.len()));
        for row in rows {
            fields.push(FilterField {
                filter_key: row
                    .get::<String>(1)
                    .map_err(|_| QueryError::PortFailure {
                        stage: "multi_profile_filter_catalog",
                        message: "failed to read filter-field key".to_owned(),
                    })?
                    .ok_or(QueryError::PortFailure {
                        stage: "multi_profile_filter_catalog",
                        message: "filter-field key is null".to_owned(),
                    })?,
                column_name: row
                    .get::<String>(2)
                    .map_err(|_| QueryError::PortFailure {
                        stage: "multi_profile_filter_catalog",
                        message: "failed to read filter-field column".to_owned(),
                    })?
                    .ok_or(QueryError::PortFailure {
                        stage: "multi_profile_filter_catalog",
                        message: "filter-field column is null".to_owned(),
                    })?,
                jsonb_path: {
                    let path_depth = row
                        .get::<i32>(3)
                        .map_err(|_| QueryError::PortFailure {
                            stage: "multi_profile_filter_catalog",
                            message: "failed to read filter-field JSON path depth".to_owned(),
                        })?
                        .and_then(|depth| usize::try_from(depth).ok())
                        .ok_or(QueryError::PortFailure {
                            stage: "multi_profile_filter_catalog",
                            message: "filter-field JSON path depth is invalid".to_owned(),
                        })?;
                    if path_depth > context_core::policy::MAX_FILTER_PATH_DEPTH {
                        return Err(QueryError::WorkBudgetExceeded {
                            budget: "multi_profile_filter_path_depth",
                            actual: path_depth,
                            maximum: context_core::policy::MAX_FILTER_PATH_DEPTH,
                        });
                    }
                    let path_bytes = row
                        .get::<i64>(4)
                        .map_err(|_| QueryError::PortFailure {
                            stage: "multi_profile_filter_catalog",
                            message: "failed to read filter-field JSON path length".to_owned(),
                        })?
                        .and_then(|bytes| usize::try_from(bytes).ok())
                        .ok_or(QueryError::PortFailure {
                            stage: "multi_profile_filter_catalog",
                            message: "filter-field JSON path length is invalid".to_owned(),
                        })?;
                    if path_bytes > context_core::policy::MAX_FILTER_PATH_BYTES {
                        return Err(QueryError::WorkBudgetExceeded {
                            budget: "multi_profile_filter_path_bytes",
                            actual: path_bytes,
                            maximum: context_core::policy::MAX_FILTER_PATH_BYTES,
                        });
                    }
                    row.get::<Vec<String>>(5)
                        .map_err(|_| QueryError::PortFailure {
                            stage: "multi_profile_filter_catalog",
                            message: "failed to read filter-field JSON path".to_owned(),
                        })?
                },
            });
        }
        Ok::<_, QueryError>(fields)
    })?;
    if fields.len() > requested_keys {
        return Err(QueryError::PortContractViolation {
            stage: "multi_profile_filter_catalog",
            requested: requested_keys,
            returned: fields.len(),
        });
    }
    let memory_bytes = fields.iter().try_fold(
        fields
            .capacity()
            .checked_mul(size_of::<FilterField>())
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "multi_profile_filter_field_memory",
            })?,
        |total, field| {
            let path_bytes = field.jsonb_path.as_ref().map_or(0, |path| {
                path.capacity()
                    .saturating_mul(size_of::<String>())
                    .saturating_add(path.iter().map(String::capacity).sum::<usize>())
            });
            total
                .checked_add(field.filter_key.capacity())
                .and_then(|bytes| bytes.checked_add(field.column_name.capacity()))
                .and_then(|bytes| bytes.checked_add(path_bytes))
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "multi_profile_filter_field_memory",
                })
        },
    )?;
    #[cfg(feature = "pg_test")]
    LAST_FILTER_FIELD_COUNT.with(|count| count.set(fields.len()));
    Ok((fields, memory_bytes))
}

pub(crate) fn filter_field_admission_memory(count: usize) -> QueryResult<usize> {
    let per_field = size_of::<FilterField>()
        .checked_add(context_core::policy::MAX_FILTER_KEY_BYTES)
        .and_then(|bytes| bytes.checked_add(context_core::policy::MAX_SQL_IDENTIFIER_BYTES))
        .and_then(|bytes| bytes.checked_add(context_core::policy::MAX_FILTER_PATH_BYTES))
        .and_then(|bytes| {
            bytes.checked_add(
                context_core::policy::MAX_FILTER_PATH_DEPTH.checked_mul(size_of::<String>())?,
            )
        })
        .and_then(|bytes| bytes.checked_add(4 * size_of::<usize>()))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "multi_profile_filter_catalog_memory",
        })?;
    count
        .checked_mul(per_field)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "multi_profile_filter_catalog_memory",
        })
}

#[cfg(feature = "pg_test")]
pub(crate) fn last_filter_field_count_for_test() -> usize {
    LAST_FILTER_FIELD_COUNT.with(Cell::get)
}

fn candidate_observations(
    profile: &PreparedProfile,
    query: &str,
    probe_limit: usize,
    filter: Option<&FilterPredicatePlan>,
    budget: PortBudget,
) -> QueryResult<(Vec<CandidateObservation>, usize, usize, usize, bool)> {
    let table = quote_qualified_identifier(&profile.source_schema, &profile.source_table);
    let vector = quote_identifier(&profile.vector_column);
    let source_version = quote_identifier(&profile.source_version_column);
    let operator = distance_operator(&profile.metric);
    let version = version_predicate(profile);
    let filter_sql = filter
        .map(|plan| format!(" AND {}", plan.sql))
        .unwrap_or_default();
    let response_bytes = candidate_transient_memory_bytes(probe_limit)?;
    let plan_memory_bytes = hnsw_plan_json_memory_bytes(profile.plan_indexes.len())?;
    let admitted_memory_bytes =
        response_bytes
            .checked_add(plan_memory_bytes)
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "multi_profile_candidate_plan_memory_projection",
            })?;
    crate::retrieval::require_port_memory_bytes(
        admitted_memory_bytes,
        budget,
        "multi_profile_candidate_memory",
    )?;
    crate::retrieval::require_port_hydration(
        probe_limit,
        budget,
        "multi_profile_candidate_hydration",
    )?;
    let traversal_comparisons = budget.max_comparisons().checked_sub(probe_limit).ok_or(
        QueryError::WorkBudgetExceeded {
            budget: "multi_profile_candidate_comparisons",
            actual: probe_limit,
            maximum: budget.max_comparisons(),
        },
    )?;
    let sql = format!(
        "SELECT pg_catalog.octet_length(source.id::text) AS source_key_bytes,
                CASE
                    WHEN pg_catalog.octet_length(source.id::text) <= {}
                    THEN source.id::text
                END AS source_key,
                source.{source_version} AS source_version,
                (source.{vector} {operator} {})::double precision AS score
           FROM {table} AS source
          WHERE source.{vector} IS NOT NULL
            AND {version}
            {filter_sql}
          ORDER BY source.{vector} {operator} {}, source.id
          LIMIT $2",
        context_core::policy::MAX_SOURCE_KEY_BYTES,
        query_cast(profile),
        query_cast(profile)
    );
    let sql_limit = i64::try_from(probe_limit).map_err(|_| QueryError::InvalidInput {
        field: "branch_limit",
        reason: "probe limit exceeds bigint".to_owned(),
    })?;
    let parameters = filter.map(|plan| plan.parameters.as_slice()).unwrap_or(&[]);
    let mut args = Vec::<DatumWithOid<'_>>::with_capacity(2 + parameters.len());
    args.push(query.into());
    args.push(sql_limit.into());
    push_filter_parameter_args(&mut args, parameters);
    let (observations, hnsw_visits, mut exact_strategy) = with_hnsw_planner_settings(|| {
        require_hnsw_query_plan(profile, &sql, &args)?;
        let execute_once = || {
            let (observations, work) = crate::hnsw_am::with_hnsw_query_budget_and_work(
                traversal_comparisons,
                budget.max_memory_bytes() - response_bytes,
                || read_candidate_observations(&sql, &args, sql_limit, probe_limit),
            );
            let observations = observations?;
            Ok::<_, QueryError>((
                observations,
                work.comparisons,
                work.exact_strategy || work.comparisons == 0,
            ))
        };
        execute_once()
    })?;
    if take_forced_exact_profile(&profile.profile) {
        exact_strategy = true;
    }
    if exact_strategy {
        let visible_rows = bounded_visible_profile_rows(profile, budget.max_comparisons())?;
        let observations = with_exact_planner_settings(|| {
            read_candidate_observations(&sql, &args, sql_limit, probe_limit)
        })?;
        let candidate_count = observations.len();
        return Ok((observations, candidate_count, 0, visible_rows, true));
    }
    let candidate_count = observations.len();
    let comparisons =
        hnsw_visits
            .checked_add(candidate_count)
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "multi_profile_candidate_comparisons",
            })?;
    if comparisons > budget.max_comparisons() {
        return Err(QueryError::WorkBudgetExceeded {
            budget: "multi_profile_candidate_comparisons",
            actual: comparisons,
            maximum: budget.max_comparisons(),
        });
    }
    Ok((
        observations,
        candidate_count,
        hnsw_visits,
        comparisons,
        false,
    ))
}

pub(crate) fn candidate_transient_memory_bytes(count: usize) -> QueryResult<usize> {
    let key_bytes = context_core::policy::MAX_SOURCE_KEY_BYTES
        .checked_mul(CANDIDATE_TRANSIENT_KEY_COPIES)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "multi_profile_candidate_key_projection",
        })?;
    let per_candidate = size_of::<CandidateObservation>()
        .checked_add(size_of::<CandidateIdentity>())
        .and_then(|bytes| bytes.checked_add(size_of::<Candidate>()))
        .and_then(|bytes| bytes.checked_add(size_of::<String>()))
        .and_then(|bytes| bytes.checked_add(size_of::<(String, i64)>()))
        .and_then(|bytes| bytes.checked_add(key_bytes))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "multi_profile_candidate_memory_projection",
        })?;
    count
        .checked_mul(per_candidate)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "multi_profile_candidate_memory_projection",
        })
}

pub(crate) fn hnsw_plan_json_memory_bytes(index_count: usize) -> QueryResult<usize> {
    index_count
        .max(1)
        .checked_mul(HNSW_PLAN_JSON_BYTES_PER_INDEX)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "multi_profile_hnsw_plan_json_memory",
        })
}

fn bounded_visible_profile_rows(
    profile: &PreparedProfile,
    comparison_budget: usize,
) -> QueryResult<usize> {
    let table = quote_qualified_identifier(&profile.source_schema, &profile.source_table);
    let vector = quote_identifier(&profile.vector_column);
    let version = version_predicate(profile);
    let sentinel = comparison_budget
        .checked_add(1)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "multi_profile_exact_fallback_sentinel",
        })?;
    let sql = format!(
        "SELECT count(*)::bigint
           FROM (
               SELECT 1
                 FROM {table} AS source
                WHERE source.{vector} IS NOT NULL
                  AND {version}
                LIMIT {sentinel}
           ) AS bounded_visible"
    );
    let visible_rows = Spi::get_one::<i64>(&sql)
        .map_err(|_| QueryError::PortFailure {
            stage: "multi_profile_exact_fallback",
            message: "failed to bound the invoker-visible exact corpus".to_owned(),
        })?
        .and_then(|count| usize::try_from(count).ok())
        .ok_or(QueryError::PortFailure {
            stage: "multi_profile_exact_fallback",
            message: "invoker-visible exact corpus count is invalid".to_owned(),
        })?;
    if visible_rows > comparison_budget {
        return Err(QueryError::WorkBudgetExceeded {
            budget: "multi_profile_exact_fallback_comparisons",
            actual: visible_rows,
            maximum: comparison_budget,
        });
    }
    Ok(visible_rows)
}

fn read_candidate_observations(
    sql: &str,
    args: &[DatumWithOid<'_>],
    sql_limit: i64,
    probe_limit: usize,
) -> QueryResult<Vec<CandidateObservation>> {
    Spi::connect(|client| {
        let rows = client
            .select(sql, Some(sql_limit.max(1)), args)
            .map_err(|_| QueryError::PortFailure {
                stage: "multi_profile_candidate_source",
                message: "bounded HNSW profile query failed".to_owned(),
            })?;
        let mut observations = Vec::with_capacity(probe_limit);
        for row in rows {
            let source_key_bytes = row
                .get::<i32>(1)
                .map_err(|_| QueryError::PortFailure {
                    stage: "multi_profile_candidate_source",
                    message: "failed to read candidate source identity length".to_owned(),
                })?
                .and_then(|bytes| usize::try_from(bytes).ok())
                .ok_or(QueryError::PortFailure {
                    stage: "multi_profile_candidate_source",
                    message: "candidate source identity length is invalid".to_owned(),
                })?;
            if source_key_bytes > context_core::policy::MAX_SOURCE_KEY_BYTES {
                return Err(QueryError::WorkBudgetExceeded {
                    budget: "multi_profile_source_key_bytes",
                    actual: source_key_bytes,
                    maximum: context_core::policy::MAX_SOURCE_KEY_BYTES,
                });
            }
            let source_key = row
                .get::<String>(2)
                .map_err(|_| QueryError::PortFailure {
                    stage: "multi_profile_candidate_source",
                    message: "failed to read candidate source identity".to_owned(),
                })?
                .ok_or(QueryError::PortFailure {
                    stage: "multi_profile_candidate_source",
                    message: "candidate source identity is null".to_owned(),
                })?;
            observations.push(CandidateObservation {
                source_key,
                source_version: row
                    .get::<i64>(3)
                    .map_err(|_| QueryError::PortFailure {
                        stage: "multi_profile_candidate_source",
                        message: "failed to read candidate source version".to_owned(),
                    })?
                    .ok_or(QueryError::PortFailure {
                        stage: "multi_profile_candidate_source",
                        message: "candidate source version is null".to_owned(),
                    })?,
                approximate_score: row
                    .get::<f64>(4)
                    .map_err(|_| QueryError::PortFailure {
                        stage: "multi_profile_candidate_source",
                        message: "failed to read candidate score".to_owned(),
                    })?
                    .ok_or(QueryError::PortFailure {
                        stage: "multi_profile_candidate_source",
                        message: "candidate score is null".to_owned(),
                    })?,
            });
        }
        Ok(observations)
    })
}

fn require_hnsw_query_plan(
    profile: &PreparedProfile,
    sql: &str,
    args: &[DatumWithOid<'_>],
) -> QueryResult<()> {
    let explain = format!("EXPLAIN (FORMAT JSON, COSTS OFF, VERBOSE) {sql}");
    let plan = Spi::connect(|client| {
        let rows = client
            .select(&explain, Some(1), args)
            .map_err(|_| QueryError::PortFailure {
                stage: "multi_profile_candidate_source",
                message: "failed to validate the bounded HNSW query plan".to_owned(),
            })?;
        rows.first()
            .get::<pgrx::Json>(1)
            .map_err(|_| QueryError::PortFailure {
                stage: "multi_profile_candidate_source",
                message: "failed to read the bounded HNSW query plan".to_owned(),
            })?
            .ok_or(QueryError::PortFailure {
                stage: "multi_profile_candidate_source",
                message: "bounded HNSW query plan is empty".to_owned(),
            })
    })?;
    if !json_plan_uses_only_attached_indexes(&plan.0, &profile.plan_indexes) {
        return Err(QueryError::PortFailure {
            stage: "multi_profile_candidate_source",
            message: "validated profile query did not plan its attached HNSW index".to_owned(),
        });
    }
    Ok(())
}

fn json_plan_uses_only_attached_indexes(value: &Value, attached: &[IndexPlanIdentity]) -> bool {
    let mut found_index = false;
    let mut work = vec![(value, 0_usize)];
    let mut visited = 0_usize;
    while let Some((value, depth)) = work.pop() {
        if depth > MAX_MULTI_PROFILE_PLAN_DEPTH {
            return false;
        }
        let Some(child_depth) = depth.checked_add(1) else {
            return false;
        };
        match value {
            Value::Object(object) => {
                visited = visited.saturating_add(1);
                if visited > context_core::policy::MAX_MULTI_PROFILE_ATTACHED_INDEXES {
                    return false;
                }
                if object
                    .get("Node Type")
                    .and_then(Value::as_str)
                    .is_some_and(|node_type| {
                        matches!(
                            node_type,
                            "Seq Scan"
                                | "Parallel Seq Scan"
                                | "Bitmap Heap Scan"
                                | "Tid Scan"
                                | "Tid Range Scan"
                                | "Sample Scan"
                        )
                    })
                {
                    return false;
                }
                if let Some(index_name) = object.get("Index Name").and_then(Value::as_str) {
                    let Some(schema) = object.get("Schema").and_then(Value::as_str) else {
                        return false;
                    };
                    if attached
                        .binary_search_by(|identity| {
                            identity
                                .schema
                                .as_str()
                                .cmp(schema)
                                .then_with(|| identity.name.as_str().cmp(index_name))
                        })
                        .is_err()
                    {
                        return false;
                    }
                    found_index = true;
                }
                work.extend(object.values().filter_map(|child| {
                    matches!(child, Value::Object(_) | Value::Array(_))
                        .then_some((child, child_depth))
                }));
            }
            Value::Array(values) => {
                visited = visited.saturating_add(1);
                if visited > context_core::policy::MAX_MULTI_PROFILE_ATTACHED_INDEXES {
                    return false;
                }
                work.extend(values.iter().filter_map(|child| {
                    matches!(child, Value::Object(_) | Value::Array(_))
                        .then_some((child, child_depth))
                }));
            }
            _ => {}
        }
    }
    found_index
}

#[cfg(feature = "pg_test")]
pub(crate) fn validate_hnsw_plan_depth_for_test(depth: usize) -> bool {
    let mut value = json!({
        "Node Type": "Index Scan",
        "Schema": "public",
        "Index Name": "fixture_hnsw"
    });
    for _ in 0..depth {
        value = Value::Array(vec![value]);
    }
    json_plan_uses_only_attached_indexes(
        &value,
        &[IndexPlanIdentity {
            schema: "public".to_owned(),
            name: "fixture_hnsw".to_owned(),
        }],
    )
}

fn with_hnsw_planner_settings<T>(operation: impl FnOnce() -> QueryResult<T>) -> QueryResult<T> {
    const SETTINGS: [(&str, &str); 3] = [
        ("enable_indexscan", "on"),
        ("enable_bitmapscan", "off"),
        ("enable_seqscan", "off"),
    ];
    with_planner_settings(&SETTINGS, operation)
}

fn with_exact_planner_settings<T>(operation: impl FnOnce() -> QueryResult<T>) -> QueryResult<T> {
    const SETTINGS: [(&str, &str); 3] = [
        ("enable_indexscan", "off"),
        ("enable_bitmapscan", "off"),
        ("enable_seqscan", "on"),
    ];
    with_planner_settings(&SETTINGS, operation)
}

fn with_planner_settings<T>(
    settings: &[(&str, &str)],
    operation: impl FnOnce() -> QueryResult<T>,
) -> QueryResult<T> {
    let mut previous = Vec::with_capacity(settings.len());
    for &(setting, value) in settings {
        let observed = Spi::get_one_with_args::<String>(
            "SELECT pg_catalog.current_setting($1)",
            &[setting.into()],
        )
        .map_err(|_| QueryError::PortFailure {
            stage: "multi_profile_candidate_source",
            message: "failed to read planner settings".to_owned(),
        })?
        .ok_or(QueryError::PortFailure {
            stage: "multi_profile_candidate_source",
            message: "planner setting is unavailable".to_owned(),
        })?;
        set_local_setting(setting, value)?;
        previous.push((setting, observed));
    }
    let outcome = operation();
    let restore = previous
        .into_iter()
        .rev()
        .try_for_each(|(setting, value)| set_local_setting(setting, &value));
    match (outcome, restore) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

fn set_local_setting(setting: &str, value: &str) -> QueryResult<()> {
    Spi::get_one_with_args::<String>(
        "SELECT pg_catalog.set_config($1, $2, true)",
        &[setting.into(), value.into()],
    )
    .map(|_| ())
    .map_err(|_| QueryError::PortFailure {
        stage: "multi_profile_candidate_source",
        message: "failed to scope planner settings".to_owned(),
    })
}

fn resolve_candidate_identities(
    collection_id: i64,
    observations: Vec<CandidateObservation>,
) -> QueryResult<Vec<CandidateIdentity>> {
    if observations.is_empty() {
        return Ok(Vec::new());
    }
    let source_keys = observations
        .iter()
        .map(|observation| observation.source_key.clone())
        .collect::<Vec<_>>();
    let row_limit = i64::try_from(source_keys.len()).map_err(|_| QueryError::PortFailure {
        stage: "multi_profile_candidate_mapping",
        message: "candidate mapping limit exceeds bigint".to_owned(),
    })?;
    let mut point_ids = Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT points.point_id, points.source_key
               FROM pgcontext._visible_collection_points AS points
              WHERE points.collection_id = $1
                AND points.deleted_at IS NULL
                AND points.source_key = ANY($2::text[])",
                Some(row_limit),
                &[collection_id.into(), source_keys.into()],
            )
            .map_err(|_| QueryError::PortFailure {
                stage: "multi_profile_candidate_mapping",
                message: "bounded point-identity lookup failed".to_owned(),
            })?;
        rows.into_iter()
            .map(|row| {
                let point_id = row
                    .get::<i64>(1)
                    .map_err(|_| QueryError::PortFailure {
                        stage: "multi_profile_candidate_mapping",
                        message: "failed to read point identity".to_owned(),
                    })?
                    .ok_or(QueryError::PortFailure {
                        stage: "multi_profile_candidate_mapping",
                        message: "point identity is null".to_owned(),
                    })?;
                let source_key = row
                    .get::<String>(2)
                    .map_err(|_| QueryError::PortFailure {
                        stage: "multi_profile_candidate_mapping",
                        message: "failed to read point source identity".to_owned(),
                    })?
                    .ok_or(QueryError::PortFailure {
                        stage: "multi_profile_candidate_mapping",
                        message: "point source identity is null".to_owned(),
                    })?;
                Ok((source_key, point_id))
            })
            .collect::<QueryResult<Vec<_>>>()
    })
    .map_err(|_| QueryError::PortFailure {
        stage: "multi_profile_candidate_mapping",
        message: "bounded point-identity lookup failed".to_owned(),
    })?;
    point_ids.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Ok(observations
        .into_iter()
        .filter_map(|observation| {
            point_ids
                .binary_search_by(|(source_key, _)| source_key.cmp(&observation.source_key))
                .ok()
                .map(|index| CandidateIdentity {
                    point_id: point_ids[index].1,
                    source_key: observation.source_key,
                    source_version: observation.source_version,
                    approximate_score: observation.approximate_score,
                })
        })
        .collect::<Vec<_>>())
}

fn authoritative_recheck(
    collection_id: i64,
    profile: &PreparedProfile,
    query: &str,
    candidates: &[CandidateIdentity],
    limit: usize,
    filter: Option<&FilterPredicatePlan>,
    budget: PortBudget,
) -> QueryResult<Vec<HydratedCandidate>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let recheck_memory_bytes = candidates.iter().try_fold(0_usize, |total, candidate| {
        total.checked_add(
            size_of::<HydratedCandidate>()
                .checked_add(size_of::<String>())?
                .checked_add(candidate.source_key.len().checked_mul(2)?)?
                .checked_add(size_of::<i64>().checked_mul(2)?)?,
        )
    });
    let recheck_memory_bytes = recheck_memory_bytes.ok_or(QueryError::ArithmeticOverflow {
        operation: "multi_profile_recheck_memory_projection",
    })?;
    crate::retrieval::require_port_memory_bytes(
        recheck_memory_bytes,
        budget,
        "multi_profile_recheck_memory",
    )?;
    crate::retrieval::require_port_hydration(
        candidates.len(),
        budget,
        "multi_profile_recheck_hydration",
    )?;
    if candidates.len() > budget.max_comparisons() {
        return Err(QueryError::WorkBudgetExceeded {
            budget: "multi_profile_recheck_comparisons",
            actual: candidates.len(),
            maximum: budget.max_comparisons(),
        });
    }
    let table = quote_qualified_identifier(&profile.source_schema, &profile.source_table);
    let vector = quote_identifier(&profile.vector_column);
    let source_version = quote_identifier(&profile.source_version_column);
    let query_cast = query_cast(profile);
    let operator = distance_operator(&profile.metric);
    let version = version_predicate(profile);
    let source_key_type = quote_qualified_identifier(
        &profile.source_key_type_schema,
        &profile.source_key_type_name,
    );
    let filter_sql = filter
        .map(|plan| format!(" AND {}", plan.sql))
        .unwrap_or_default();
    let sql = format!(
        "SELECT points.point_id,
                points.source_key,
                (source.{vector} {operator} {query_cast})::double precision AS score,
                source.{source_version}
           FROM pg_catalog.unnest($3::text[])
                WITH ORDINALITY AS requested_keys(source_key, ordinality)
           JOIN pg_catalog.unnest($4::bigint[])
                WITH ORDINALITY AS requested_versions(expected_version, ordinality)
             ON requested_versions.ordinality = requested_keys.ordinality
           JOIN {table} AS source
             ON source.id = requested_keys.source_key::{source_key_type}
           JOIN pgcontext._visible_collection_points AS points
             ON points.source_key = requested_keys.source_key
            AND points.point_id = ANY($5::bigint[])
          WHERE points.collection_id = $2
            AND points.deleted_at IS NULL
            AND source.{vector} IS NOT NULL
            AND source.{source_version} = requested_versions.expected_version
            AND {version}
            {filter_sql}
          ORDER BY score ASC, points.point_id ASC
          LIMIT $6"
    );
    let sql_limit = i64::try_from(limit)
        .unwrap_or_else(|_| invalid_parameter("branch recheck limit exceeds bigint"));
    let parameters = filter.map(|plan| plan.parameters.as_slice()).unwrap_or(&[]);
    let mut args = Vec::<DatumWithOid<'_>>::with_capacity(6 + parameters.len());
    args.push(query.into());
    args.push(collection_id.into());
    args.push(
        candidates
            .iter()
            .map(|candidate| candidate.source_key.clone())
            .collect::<Vec<_>>()
            .into(),
    );
    args.push(
        candidates
            .iter()
            .map(|candidate| candidate.source_version)
            .collect::<Vec<_>>()
            .into(),
    );
    args.push(
        candidates
            .iter()
            .map(|candidate| candidate.point_id)
            .collect::<Vec<_>>()
            .into(),
    );
    args.push(sql_limit.into());
    push_filter_parameter_args(&mut args, parameters);
    Spi::connect(|client| {
        let rows =
            client
                .select(&sql, Some(sql_limit), &args)
                .map_err(|_| QueryError::PortFailure {
                    stage: "multi_profile_source_recheck",
                    message: "bounded authoritative profile reread failed".to_owned(),
                })?;
        rows.into_iter()
            .map(|row| {
                let point_id = point_id(
                    row.get::<i64>(1)
                        .map_err(|_| QueryError::PortFailure {
                            stage: "multi_profile_source_recheck",
                            message: "failed to read rechecked point identity".to_owned(),
                        })?
                        .ok_or(QueryError::PortFailure {
                            stage: "multi_profile_source_recheck",
                            message: "rechecked point identity is null".to_owned(),
                        })?,
                )?;
                let source_key = row
                    .get::<String>(2)
                    .map_err(|_| QueryError::PortFailure {
                        stage: "multi_profile_source_recheck",
                        message: "failed to read rechecked source identity".to_owned(),
                    })?
                    .ok_or(QueryError::PortFailure {
                        stage: "multi_profile_source_recheck",
                        message: "rechecked source identity is null".to_owned(),
                    })?;
                let source_key = SourceKey::new(source_key).map_err(QueryError::from)?;
                let score = row
                    .get::<f64>(3)
                    .map_err(|_| QueryError::PortFailure {
                        stage: "multi_profile_source_recheck",
                        message: "failed to read authoritative score".to_owned(),
                    })?
                    .ok_or(QueryError::PortFailure {
                        stage: "multi_profile_source_recheck",
                        message: "authoritative score is null".to_owned(),
                    })?;
                HydratedCandidate::new(point_id, source_key, score)
            })
            .collect::<QueryResult<Vec<_>>>()
    })
    .map_err(|_| QueryError::PortFailure {
        stage: "multi_profile_source_recheck",
        message: "bounded authoritative profile reread failed".to_owned(),
    })
}

fn build_report(
    request: &MultiProfileRequest,
    coverage: MultiProfileCoverage,
    selected: &BTreeMap<ProfileName, PreparedProfile>,
    state: &RuntimeState,
    outcome: &ExecutionOutcome,
    total_memory_bytes: usize,
) -> Value {
    let missing = match coverage {
        MultiProfileCoverage::Complete => Vec::new(),
        MultiProfileCoverage::Partial { missing } => missing,
    };
    let completion = if missing.is_empty() {
        "complete"
    } else {
        "degraded"
    };
    let missing_by_name = missing
        .iter()
        .map(|entry| (entry.profile().as_str(), entry.reason()))
        .collect::<BTreeMap<_, _>>();
    let branch_reports = request
        .branches()
        .iter()
        .map(|branch| {
            if let (Some(profile), Some(executed)) = (
                selected.get(branch.profile()),
                state.branches.get(branch.profile()),
            ) {
                let strategy = if executed.exact_fallback {
                    "exact_fallback_with_authoritative_recheck"
                } else {
                    "hnsw_with_authoritative_recheck"
                };
                json!({
                    "profile": branch.profile().as_str(),
                    "registration_revision": profile.registration_revision,
                    "lifecycle": profile.lifecycle.stable_name(),
                    "status": "served",
                    "strategy": strategy,
                    "candidate_count": executed.candidate_count,
                    "recheck_count": executed.recheck_count,
                    "retained_results": executed.retained_count,
                    "probe_exhausted": executed.probe_exhausted,
                    "hnsw_visits": executed.hnsw_visits,
                    "index_oid": profile.index_oid.to_u32(),
                    "source_table_oid": profile.source_table_oid.to_u32(),
                })
            } else {
                json!({
                    "profile": branch.profile().as_str(),
                    "status": "skipped",
                    "reason": missing_reason_name(
                        missing_by_name
                            .get(branch.profile().as_str())
                            .copied()
                            .unwrap_or_else(|| data_corrupted(
                                "unselected profile has no explicit readiness reason"
                            ))
                    ),
                    "candidate_count": 0,
                    "recheck_count": 0,
                    "retained_results": 0,
                })
            }
        })
        .collect::<Vec<_>>();

    let profile_by_id = selected
        .values()
        .map(|profile| {
            (
                profile_id(profile).unwrap_or_else(|error| raise_query_error(error)),
                profile,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let weight_by_name = request
        .branches()
        .iter()
        .map(|branch| (branch.profile(), branch.weight()))
        .collect::<BTreeMap<_, _>>();
    let results =
        outcome
            .points()
            .iter()
            .map(|point| {
                let contributions = point
                .contributions()
                .iter()
                .map(|contribution| {
                    let provenance = contribution.provenance();
                    let profile_id = provenance.profile().unwrap_or_else(|| {
                        data_corrupted("multi-profile contribution lost its profile identity")
                    });
                    let profile = profile_by_id.get(&profile_id).copied().unwrap_or_else(|| {
                        data_corrupted("multi-profile contribution references an unknown profile")
                    });
                    let source_version = provenance.source_version().unwrap_or_else(|| {
                        data_corrupted("multi-profile contribution lost its source version")
                    });
                    let weight = weight_by_name.get(&profile.profile).copied().unwrap_or_else(|| {
                        data_corrupted("multi-profile contribution lost its branch weight")
                    });
                    json!({
                        "profile": profile.profile.as_str(),
                        "registration_revision": profile.registration_revision,
                        "source_version": source_version.get(),
                        "source_kind": provenance.source().stable_name(),
                        "source_authority": source_authority_name(provenance.authority()),
                        "rank": contribution.source_rank().saturating_add(1),
                        "raw_score": contribution.source_score(),
                        "weight": weight,
                        "rrf_contribution": contribution.fusion_contribution().unwrap_or_else(|| {
                            data_corrupted("multi-profile contribution lost its fusion score")
                        }),
                    })
                })
                .collect::<Vec<_>>();
                json!({
                    "point_id": point.point_id().get(),
                    "source_key": point.source_key().as_str(),
                    "fused_score": point.score(),
                    "contributions": contributions,
                })
            })
            .collect::<Vec<_>>();

    json!({
        "completion": completion,
        "missing_profiles": missing
            .iter()
            .map(|entry| json!({
                "profile": entry.profile().as_str(),
                "reason": missing_reason_name(entry.reason()),
            }))
            .collect::<Vec<_>>(),
        "branches": branch_reports,
        "results": results,
        "budget_usage": {
            "candidates": outcome.usage().candidates(),
            "rechecks": outcome.usage().rechecks(),
            "comparisons": outcome.usage().comparisons(),
            "memory_bytes": total_memory_bytes,
            "hydration_bytes": outcome.usage().hydration_bytes(),
            "stages": outcome.usage().stages(),
            "elapsed_micros": outcome.usage().elapsed_micros(),
        },
    })
}

fn source_authority_name(authority: context_core::SourceAuthority) -> &'static str {
    match authority {
        context_core::SourceAuthority::PostgreSqlRow => "postgresql_row",
        context_core::SourceAuthority::ProviderNative => "provider_native",
        context_core::SourceAuthority::DerivedArtifact => "derived_artifact",
    }
}

fn query_cast(profile: &PreparedProfile) -> String {
    let type_name = match profile.representation.as_str() {
        "dense" => "vector",
        "half" => "halfvec",
        "sparse" => "sparsevec",
        "bit" => "bitvec",
        "int8" => "int8vec",
        "uint8" => "uint8vec",
        _ => data_corrupted("stored embedding profile representation is invalid"),
    };
    format!("$1::pgcontext.{type_name}({})", profile.dimensions)
}

fn distance_operator(metric: &str) -> &'static str {
    match metric {
        "l2" => "OPERATOR(pgcontext.<->)",
        "inner_product" => "OPERATOR(pgcontext.<#>)",
        "cosine" => "OPERATOR(pgcontext.<=>)",
        "l1" => "OPERATOR(pgcontext.<+>)",
        "hamming" => "OPERATOR(pgcontext.<~>)",
        "jaccard" => "OPERATOR(pgcontext.<%>)",
        _ => data_corrupted("stored embedding profile metric is invalid"),
    }
}

fn version_predicate(profile: &PreparedProfile) -> String {
    let source = quote_identifier(&profile.source_version_column);
    let embedding = quote_identifier(&profile.embedding_version_column);
    format!(
        "source.{source} IS NOT NULL
         AND source.{embedding} IS NOT NULL
         AND source.{source} = source.{embedding}"
    )
}

fn missing_reason_name(reason: MissingProfileReason) -> &'static str {
    match reason {
        MissingProfileReason::NotRegistered => "not_registered",
        MissingProfileReason::ConfigurationChanged => "configuration_changed",
        MissingProfileReason::LifecycleNotServing => "lifecycle_not_serving",
        MissingProfileReason::VersionBindingsMissing => "version_bindings_missing",
        MissingProfileReason::NotReady => "not_ready",
    }
}

fn required<T>(value: Option<T>, label: &str) -> T {
    value.unwrap_or_else(|| data_corrupted(format!("multi-model {label} is null")))
}

fn invalid_parameter(message: impl Into<String>) -> ! {
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
        message.into(),
    )
}

fn data_corrupted(message: impl Into<String>) -> ! {
    raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, message.into())
}

fn internal(message: impl Into<String>) -> ! {
    raise_sql_error(PgSqlErrorCode::ERRCODE_INTERNAL_ERROR, message.into())
}
