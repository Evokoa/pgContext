//! PostgreSQL ports for named sparse exact/ANN execution.

use core::mem::size_of;

use context_core::{
    CollectionName, PointId, ScoreOrder, SourceAuthority, SourceKey, SparseEntry, SparseVector,
};
use context_query::{
    Candidate, CandidateBranch, CandidateDiagnostics, CandidatePage, CandidateSource,
    CandidateSourceKind, ExecutionBudget, ExecutionOutcome, HydratedCandidate, PortBudget,
    QueryError, QueryExecutor, QueryIr, QueryKind, RecheckPage, Result, SourceReadiness,
    SourceRechecker,
};
use pgrx::prelude::*;
use serde_json::Value;

use super::{
    PgCancellation, candidate_provenance, outcome_rows, port_failure, require_complete_outcome,
    spi_column, spi_point_id, sql_limit,
};
use crate::sparse_search::{
    RegisteredSparseVector, require_sparse_query_dimensions, require_sparse_table_select_privilege,
    resolve_registered_sparse_vector, resolve_sparse_hnsw_index, sparse_distance_function,
    validate_sparse_vector_drift,
};
use crate::table_search::{
    FilterField, load_filter_fields, push_filter_parameter_args, quote_identifier,
    quote_qualified_identifier, resolve_typed_filter_plan,
};
use crate::vector_variants::SparseVec;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SparseCandidateStrategy {
    Exact,
    Hnsw(pg_sys::Oid),
}

pub(crate) struct SparseExecution {
    pub(crate) rows: Vec<(i64, String, f32)>,
    pub(crate) outcome: ExecutionOutcome,
    pub(crate) strategy: SparseCandidateStrategy,
}

#[derive(Debug, Clone)]
pub(super) struct CompositeSparseSource {
    registered_vector: RegisteredSparseVector,
    strategy: SparseCandidateStrategy,
}

impl CompositeSparseSource {
    pub(super) fn prepare(
        collection_name: &CollectionName,
        collection_id: i64,
        query: &QueryIr,
    ) -> Result<Self> {
        let QueryKind::SparseNearest {
            vector_name,
            vector,
        } = query.kind()
        else {
            return Err(QueryError::PortFailure {
                stage: "sparse_candidate_source",
                message: "sparse adapter requires a sparse-nearest query".to_owned(),
            });
        };
        let mut registered_vector =
            resolve_registered_sparse_vector(collection_name, collection_id, vector_name.as_str());
        validate_sparse_vector_drift(collection_id, &mut registered_vector);
        require_sparse_table_select_privilege(&registered_vector);
        require_sparse_query_dimensions(&registered_vector, vector);

        let mask_limit = crate::settings::hnsw_mask_candidate_limit_from_guc();
        let strategy = resolve_sparse_hnsw_index(&registered_vector).map_or(
            SparseCandidateStrategy::Exact,
            |index_oid| {
                let visible_size = if mask_limit == 0 || query.filter().is_some() {
                    None
                } else {
                    visible_sparse_mask_size(collection_id, &registered_vector, mask_limit).ok()
                };
                if mask_limit == 0
                    || visible_size.is_some_and(|size| size == 0 || size > mask_limit)
                {
                    SparseCandidateStrategy::Exact
                } else {
                    SparseCandidateStrategy::Hnsw(index_oid)
                }
            },
        );
        Ok(Self {
            registered_vector,
            strategy,
        })
    }

    pub(super) const fn readiness(&self) -> SourceReadiness {
        match self.strategy {
            SparseCandidateStrategy::Exact => SourceReadiness::Exact,
            SparseCandidateStrategy::Hnsw(_) => SourceReadiness::Ready,
        }
    }

    pub(super) fn candidate_limit(&self, query: &QueryIr, remaining: usize) -> usize {
        match self.strategy {
            SparseCandidateStrategy::Exact => query.limit().min(remaining),
            SparseCandidateStrategy::Hnsw(_) => crate::settings::hnsw_candidate_budget_from_guc()
                .max(query.limit())
                .min(remaining),
        }
    }

    pub(super) fn candidates(
        &self,
        collection_id: i64,
        query: &QueryIr,
        filter: Option<&context_query::FilterCandidateBatch>,
        limit: usize,
        budget: PortBudget,
    ) -> Result<CandidatePage> {
        let page = sparse_candidates_with_budget(
            collection_id,
            &self.registered_vector,
            self.strategy,
            query,
            filter,
            limit,
            budget,
        )?;
        Ok(page
            .with_strategy(match self.strategy {
                SparseCandidateStrategy::Exact => "named_sparse_exact",
                SparseCandidateStrategy::Hnsw(_) => "named_sparse_hnsw",
            })
            .with_expansion_count(usize::from(matches!(
                self.strategy,
                SparseCandidateStrategy::Hnsw(_)
            ))))
    }

    pub(super) fn recheck(
        &self,
        collection_id: i64,
        filter_fields: &[FilterField],
        query: &QueryIr,
        candidates: &[Candidate],
        limit: usize,
        budget: PortBudget,
    ) -> Result<Vec<HydratedCandidate>> {
        SpiSparseSourceRechecker {
            collection_id,
            registered_vector: &self.registered_vector,
            filter_fields,
        }
        .recheck(query, candidates, limit, budget)
        .map(RecheckPage::into_rows)
    }
}

pub(crate) fn run_sparse_query(
    collection_name: &CollectionName,
    collection_id: i64,
    registered_vector: &RegisteredSparseVector,
    query_vector: SparseVector,
    filter: Option<Value>,
    limit: usize,
) -> SparseExecution {
    let observation = std::sync::Mutex::new(None);
    PgTryBuilder::new(|| {
        run_sparse_query_inner(
            collection_name,
            collection_id,
            registered_vector,
            query_vector,
            filter,
            limit,
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
        let observation = *observation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(observation) = observation {
            crate::query_stats_async::abort(observation, sqlerrcode as i32);
        }
        cause.rethrow()
    })
    .execute()
}

#[allow(
    clippy::too_many_arguments,
    reason = "the sparse PostgreSQL adapter carries resolved catalog and query inputs"
)]
fn run_sparse_query_inner(
    collection_name: &CollectionName,
    collection_id: i64,
    registered_vector: &RegisteredSparseVector,
    query_vector: SparseVector,
    filter: Option<Value>,
    limit: usize,
    observation: &std::sync::Mutex<Option<crate::query_stats_async::ObservationToken>>,
) -> SparseExecution {
    let has_filter = filter.is_some();
    let query = QueryIr::sparse_nearest(
        registered_vector.vector_name.clone(),
        query_vector,
        ScoreOrder::LowerIsBetter,
        filter,
        limit,
    )
    .unwrap_or_else(|error| crate::error::raise_query_error(error));
    *observation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        crate::query_stats::begin_automatic_query_stat(collection_id, &query, false);
    let mask_limit = crate::settings::hnsw_mask_candidate_limit_from_guc();
    let strategy = resolve_sparse_hnsw_index(registered_vector).map_or(
        SparseCandidateStrategy::Exact,
        |index_oid| {
            let visible_size = if mask_limit == 0 || has_filter {
                None
            } else {
                Some(
                    visible_sparse_mask_size(collection_id, registered_vector, mask_limit)
                        .unwrap_or_else(|error| crate::error::raise_query_error(error)),
                )
            };
            if mask_limit == 0 || visible_size.is_some_and(|size| size == 0 || size > mask_limit) {
                SparseCandidateStrategy::Exact
            } else {
                SparseCandidateStrategy::Hnsw(index_oid)
            }
        },
    );
    let candidate_limit = match strategy {
        SparseCandidateStrategy::Exact => limit,
        SparseCandidateStrategy::Hnsw(_) => {
            crate::settings::hnsw_candidate_budget_from_guc().max(limit)
        }
    };
    if matches!(strategy, SparseCandidateStrategy::Hnsw(_)) {
        crate::collection_limits::enforce_candidate_budget(
            collection_id,
            collection_name,
            candidate_limit,
        );
    }
    let filter_candidate_limit = match strategy {
        SparseCandidateStrategy::Exact => context_core::policy::MAX_RECALL_CHECK_POINT_IDS,
        SparseCandidateStrategy::Hnsw(_) => crate::settings::hnsw_mask_candidate_limit_from_guc(),
    };
    let budget = ExecutionBudget::new(
        candidate_limit,
        filter_candidate_limit,
        candidate_limit,
        3,
        1,
        limit,
    )
    .unwrap_or_else(|error| crate::error::raise_query_error(error));
    let mut candidate_source = SpiSparseAnnCandidateSource {
        collection_id,
        registered_vector,
        strategy,
    };
    let filter_fields = query
        .filter()
        .map(|_| load_filter_fields(collection_id))
        .unwrap_or_default();
    let mut rechecker = SpiSparseSourceRechecker {
        collection_id,
        registered_vector,
        filter_fields: &filter_fields,
    };
    let mut filter_source = SpiSparseFilterCandidateSource {
        collection_id,
        registered_vector,
        filter_fields: &filter_fields,
    };
    let filter_port = query
        .filter()
        .map(|_| &mut filter_source as &mut dyn context_query::FilterCandidateSource);
    let mut telemetry = super::PgTelemetrySink::default();
    let cancellation = PgCancellation;
    let outcome = QueryExecutor::new(
        &mut candidate_source,
        filter_port,
        &mut rechecker,
        &mut telemetry,
        &cancellation,
    )
    .execute(&query, budget);
    crate::query_stats::record_automatic_query_stat(
        *observation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        &telemetry.diagnostics,
        outcome.as_ref(),
        false,
    );
    let outcome = outcome.unwrap_or_else(|error| crate::error::raise_query_error(error));
    require_complete_outcome(&outcome);
    let rows =
        outcome_rows(&outcome).unwrap_or_else(|error| crate::error::raise_query_error(error));
    SparseExecution {
        rows,
        outcome,
        strategy,
    }
}

struct SpiSparseAnnCandidateSource<'a> {
    collection_id: i64,
    registered_vector: &'a RegisteredSparseVector,
    strategy: SparseCandidateStrategy,
}

impl CandidateSource for SpiSparseAnnCandidateSource<'_> {
    fn readiness(&mut self, query: &QueryIr, _budget: PortBudget) -> Result<SourceReadiness> {
        sparse_query(query)?;
        Ok(match self.strategy {
            SparseCandidateStrategy::Exact => SourceReadiness::Exact,
            SparseCandidateStrategy::Hnsw(_) => SourceReadiness::Ready,
        })
    }

    fn candidates(
        &mut self,
        query: &QueryIr,
        filter: Option<&context_query::FilterCandidateBatch>,
        limit: usize,
        budget: PortBudget,
    ) -> Result<CandidatePage> {
        let page = sparse_candidates_with_budget(
            self.collection_id,
            self.registered_vector,
            self.strategy,
            query,
            filter,
            limit,
            budget,
        )?;
        Ok(page
            .with_strategy(match self.strategy {
                SparseCandidateStrategy::Exact => "named_sparse_exact",
                SparseCandidateStrategy::Hnsw(_) => "named_sparse_hnsw",
            })
            .with_expansion_count(usize::from(matches!(
                self.strategy,
                SparseCandidateStrategy::Hnsw(_)
            ))))
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the sparse candidate gate mirrors the candidate port contract"
)]
fn sparse_candidates_with_budget(
    collection_id: i64,
    registered_vector: &RegisteredSparseVector,
    strategy: SparseCandidateStrategy,
    query: &QueryIr,
    filter: Option<&context_query::FilterCandidateBatch>,
    limit: usize,
    budget: PortBudget,
) -> Result<CandidatePage> {
    let sparse_query_vector = sparse_query(query)?;
    let filter_ids = filter.map_or(0, |filter| filter.point_ids().len());
    let query_memory = match strategy {
        SparseCandidateStrategy::Exact => sparse_query_copy_bytes(sparse_query_vector, 2, 0)?,
        SparseCandidateStrategy::Hnsw(_) => sparse_query_copy_bytes(sparse_query_vector, 3, 3)?,
    };
    let retained_memory = sparse_candidate_peak_bytes(limit, filter_ids, query_memory)?;
    require_sparse_memory(
        retained_memory,
        budget.max_memory_bytes(),
        "candidate_memory",
    )?;
    let traversal_memory = budget.max_memory_bytes() - retained_memory;
    match strategy {
        SparseCandidateStrategy::Exact => {
            if filter.is_none() {
                return exact_sparse_candidates_with_admission(
                    collection_id,
                    registered_vector,
                    query,
                    limit,
                    budget.max_comparisons(),
                );
            }
            let projected = filter.map_or(0, |filter| filter.point_ids().len());
            super::with_candidate_comparison_budget(projected, budget, || {
                exact_sparse_candidates(collection_id, registered_vector, query, filter, limit)
            })
        }
        SparseCandidateStrategy::Hnsw(index_oid) => hnsw_sparse_candidates(
            collection_id,
            registered_vector,
            index_oid,
            query,
            filter,
            limit,
            budget.max_comparisons(),
            traversal_memory,
        ),
    }
}

struct SpiSparseSourceRechecker<'a> {
    collection_id: i64,
    registered_vector: &'a RegisteredSparseVector,
    filter_fields: &'a [FilterField],
}

impl SourceRechecker for SpiSparseSourceRechecker<'_> {
    fn recheck(
        &mut self,
        query: &QueryIr,
        candidates: &[Candidate],
        limit: usize,
        budget: PortBudget,
    ) -> Result<RecheckPage> {
        let output_count = candidates.len().min(limit);
        let query_memory = sparse_query_copy_bytes(sparse_query(query)?, 2, 0)?;
        let projected_memory =
            sparse_recheck_peak_bytes(candidates.len(), output_count, query_memory)?;
        require_sparse_memory(
            projected_memory,
            budget.max_memory_bytes(),
            "source_recheck_memory",
        )?;
        super::require_port_hydration(output_count, budget, "source_recheck_hydration")?;
        if candidates.len() > budget.max_comparisons() {
            return Err(QueryError::WorkBudgetExceeded {
                budget: "source_recheck_comparisons",
                actual: candidates.len(),
                maximum: budget.max_comparisons(),
            });
        }
        let query_sql = SparseVec::from_sparse(sparse_query(query)?.clone());
        let point_ids = super::sql_point_ids(candidates.iter().map(Candidate::point_id))?;
        let table_name = quote_qualified_identifier(
            &self.registered_vector.schema_name,
            &self.registered_vector.table_name,
        );
        let vector_column = quote_identifier(&self.registered_vector.vector_column_name);
        let distance_function = sparse_distance_function(self.registered_vector.metric);
        let filter_plan = query
            .filter()
            .map(|filter| resolve_typed_filter_plan(self.filter_fields, filter, 4))
            .transpose()
            .map_err(|error| port_failure("sparse_source_rechecker", error))?;
        let filter_sql = filter_plan
            .as_ref()
            .map(|plan| format!(" AND {}", plan.sql))
            .unwrap_or_default();
        let sql_limit = sql_limit(limit, "sparse_source_rechecker")?;
        let max_source_key_bytes = context_core::policy::MAX_SOURCE_KEY_BYTES;
        let sql = format!(
            "SELECT points.point_id,
                    CASE WHEN pg_catalog.octet_length(points.source_key) <= {max_source_key_bytes}
                         THEN points.source_key
                    END AS source_key,
                    pgcontext.{distance_function}(source.{vector_column}, $1) AS score
               FROM pgcontext._visible_collection_points AS points
               JOIN {table_name} AS source ON source.id::text = points.source_key
              WHERE points.collection_id = $2
                AND points.deleted_at IS NULL
                AND points.point_id = ANY($3::bigint[])
                {filter_sql}
              ORDER BY score ASC, points.point_id ASC
              LIMIT $4"
        );
        let parameters = filter_plan
            .as_ref()
            .map(|plan| plan.parameters.as_slice())
            .unwrap_or(&[]);
        let mut args = Vec::<pgrx::datum::DatumWithOid<'_>>::with_capacity(4 + parameters.len());
        args.push(query_sql.into());
        args.push(self.collection_id.into());
        args.push(point_ids.into());
        args.push(sql_limit.into());
        push_filter_parameter_args(&mut args, parameters);
        Spi::connect(|client| {
            let rows = client
                .select(&sql, Some(sql_limit), &args)
                .map_err(|error| port_failure("sparse_source_rechecker", error))?;
            let mut output = Vec::with_capacity(output_count);
            for row in rows {
                output.push(HydratedCandidate::new(
                    spi_point_id(&row, 1, "sparse_source_rechecker")?,
                    SourceKey::new(spi_column::<String>(&row, 2, "sparse_source_rechecker")?)?,
                    f64::from(spi_column::<f32>(&row, 3, "sparse_source_rechecker")?),
                )?);
            }
            Ok(RecheckPage::new(output, candidates.len()))
        })
    }
}

struct SpiSparseFilterCandidateSource<'a> {
    collection_id: i64,
    registered_vector: &'a RegisteredSparseVector,
    filter_fields: &'a [FilterField],
}

impl context_query::FilterCandidateSource for SpiSparseFilterCandidateSource<'_> {
    fn filter_candidates(
        &mut self,
        query: &QueryIr,
        limit: usize,
        budget: PortBudget,
    ) -> Result<context_query::FilterCandidateBatch> {
        let result_probe_limit = limit.checked_add(1).ok_or(QueryError::ArithmeticOverflow {
            operation: "sparse_filter_result_probe",
        })?;
        let admitted_probe_count = result_probe_limit.min(budget.max_comparisons());
        super::require_port_memory_for::<PointId>(
            admitted_probe_count,
            budget,
            "filter_candidate_memory",
        )?;
        let filter = query.filter().ok_or_else(|| QueryError::PortFailure {
            stage: "sparse_filter_candidate_source",
            message: "sparse filter adapter called without a query filter".to_owned(),
        })?;
        let plan = resolve_typed_filter_plan(self.filter_fields, filter, 4)
            .map_err(|error| port_failure("sparse_filter_candidate_source", error))?;
        let table_name = quote_qualified_identifier(
            &self.registered_vector.schema_name,
            &self.registered_vector.table_name,
        );
        let sql_result_limit = sql_limit(result_probe_limit, "sparse_filter_candidate_source")?;
        let comparison_probe =
            budget
                .max_comparisons()
                .checked_add(1)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "sparse_filter_comparison_probe",
                })?;
        let sql_comparison_probe = sql_limit(comparison_probe, "sparse_filter_candidate_source")?;
        let sql_max_comparisons =
            sql_limit(budget.max_comparisons(), "sparse_filter_candidate_source")?;
        let sql = format!(
            "WITH visible AS MATERIALIZED (
                 SELECT points.point_id, source.ctid AS heap_tid
                   FROM pgcontext._visible_collection_points AS points
                   JOIN {table_name} AS source ON source.id::text = points.source_key
                  WHERE points.collection_id = $1
                    AND points.deleted_at IS NULL
                  LIMIT $3
             ),
             admission AS MATERIALIZED (
                 SELECT count(*)::bigint AS evaluated_count FROM visible
             ),
             filtered AS MATERIALIZED (
                 SELECT visible.point_id
                   FROM visible
                   JOIN {table_name} AS source ON source.ctid = visible.heap_tid
                  CROSS JOIN admission
                  WHERE admission.evaluated_count <= $4
                    AND {}
                  ORDER BY visible.point_id
                  LIMIT $2
             )
             SELECT filtered.point_id, admission.evaluated_count
               FROM admission
               LEFT JOIN filtered ON true
              ORDER BY filtered.point_id NULLS LAST",
            plan.sql
        );
        let mut args =
            Vec::<pgrx::datum::DatumWithOid<'_>>::with_capacity(4 + plan.parameters.len());
        args.push(self.collection_id.into());
        args.push(sql_result_limit.into());
        args.push(sql_comparison_probe.into());
        args.push(sql_max_comparisons.into());
        push_filter_parameter_args(&mut args, &plan.parameters);

        Spi::connect(|client| {
            let rows = client
                .select(&sql, Some(sql_result_limit), &args)
                .map_err(|error| port_failure("sparse_filter_candidate_source", error))?;
            let mut point_ids = Vec::with_capacity(admitted_probe_count);
            let mut evaluated_count = None;
            for row in rows {
                evaluated_count = Some(
                    usize::try_from(spi_column::<i64>(
                        &row,
                        2,
                        "sparse_filter_candidate_source",
                    )?)
                    .map_err(|_| QueryError::PortFailure {
                        stage: "sparse_filter_candidate_source",
                        message: "visible sparse filter row count exceeds usize".to_owned(),
                    })?,
                );
                if let Some(point_id) = super::spi_optional_result_column::<i64>(
                    &row,
                    1,
                    "sparse_filter_candidate_source",
                )? {
                    point_ids.push(PointId::from_i64(point_id).ok_or_else(|| {
                        QueryError::PortFailure {
                            stage: "sparse_filter_candidate_source",
                            message: format!("negative PostgreSQL point ID: {point_id}"),
                        }
                    })?);
                }
            }
            let evaluated_count = evaluated_count.ok_or_else(|| QueryError::PortFailure {
                stage: "sparse_filter_candidate_source",
                message: "sparse filter admission returned no row".to_owned(),
            })?;
            if evaluated_count > budget.max_comparisons() {
                return Err(QueryError::WorkBudgetExceeded {
                    budget: "filter_comparisons",
                    actual: evaluated_count,
                    maximum: budget.max_comparisons(),
                });
            }
            let exhausted = point_ids.len() <= limit;
            point_ids.truncate(limit);
            Ok(context_query::FilterCandidateBatch::new(
                point_ids,
                evaluated_count,
                exhausted,
            ))
        })
    }
}

fn exact_sparse_candidates(
    collection_id: i64,
    registered_vector: &RegisteredSparseVector,
    query: &QueryIr,
    filter: Option<&context_query::FilterCandidateBatch>,
    limit: usize,
) -> Result<CandidatePage> {
    let filter = filter.ok_or_else(|| QueryError::PortFailure {
        stage: "sparse_candidate_source",
        message: "filtered sparse exact path requires a candidate mask".to_owned(),
    })?;
    let sparse_query = SparseVec::from_sparse(sparse_query(query)?.clone());
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let vector_column = quote_identifier(&registered_vector.vector_column_name);
    let distance_function = sparse_distance_function(registered_vector.metric);
    let sql_limit = sql_limit(limit, "sparse_candidate_source")?;
    let point_ids = super::sql_point_ids(filter.point_ids().iter().copied())?;
    if point_ids.is_empty() {
        return Ok(CandidatePage::with_scored_count(Vec::new(), 0, true));
    }
    exact_sparse_candidates_with_filter(
        collection_id,
        sparse_query,
        table_name,
        vector_column,
        distance_function,
        sql_limit,
        point_ids,
    )
}

fn exact_sparse_candidates_with_admission(
    collection_id: i64,
    registered_vector: &RegisteredSparseVector,
    query: &QueryIr,
    limit: usize,
    max_comparisons: usize,
) -> Result<CandidatePage> {
    let query = SparseVec::from_sparse(sparse_query(query)?.clone());
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let vector_column = quote_identifier(&registered_vector.vector_column_name);
    let distance_function = sparse_distance_function(registered_vector.metric);
    let result_limit = sql_limit(limit, "sparse_candidate_source")?;
    let probe_limit = max_comparisons
        .checked_add(1)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "sparse_exact_admission_probe",
        })?;
    let probe_limit = sql_limit(probe_limit, "sparse_candidate_source")?;
    let sql_max_comparisons = sql_limit(max_comparisons, "sparse_candidate_source")?;
    let sql = format!(
        "WITH visible AS MATERIALIZED (
             SELECT points.point_id,
                    source.{vector_column} AS sparse_vector
               FROM pgcontext._visible_collection_points AS points
               JOIN {table_name} AS source ON source.id::text = points.source_key
              WHERE points.collection_id = $2
                AND points.deleted_at IS NULL
              LIMIT $4
         ),
         admission AS MATERIALIZED (
             SELECT count(*)::bigint AS visible_count
               FROM visible
         ),
         scored AS MATERIALIZED (
             SELECT visible.point_id,
                    pgcontext.{distance_function}(visible.sparse_vector, $1) AS score
               FROM visible
              CROSS JOIN admission
              WHERE admission.visible_count <= $5
              ORDER BY score ASC, visible.point_id ASC
              LIMIT $3
         )
         SELECT scored.point_id,
                scored.score,
                admission.visible_count
           FROM admission
           LEFT JOIN scored ON true
          ORDER BY scored.score ASC NULLS LAST, scored.point_id ASC"
    );
    Spi::connect(|client| {
        let mut rows = client
            .select(
                &sql,
                Some(result_limit.max(1)),
                &[
                    query.into(),
                    collection_id.into(),
                    result_limit.into(),
                    probe_limit.into(),
                    sql_max_comparisons.into(),
                ],
            )
            .map_err(|error| port_failure("sparse_candidate_source", error))?;
        let first = rows.next().ok_or_else(|| QueryError::PortFailure {
            stage: "sparse_candidate_source",
            message: "sparse exact admission returned no row".to_owned(),
        })?;
        let scored_count = admitted_exact_sparse_count(
            spi_column::<i64>(&first, 3, "sparse_candidate_source")?,
            max_comparisons,
        )?;
        let mut candidates = Vec::with_capacity(limit.min(scored_count));
        for row in core::iter::once(first).chain(rows) {
            let point_id =
                super::spi_optional_result_column::<i64>(&row, 1, "sparse_candidate_source")?;
            let score =
                super::spi_optional_result_column::<f32>(&row, 2, "sparse_candidate_source")?;
            let (Some(point_id), Some(score)) = (point_id, score) else {
                continue;
            };
            let point_id = PointId::from_i64(point_id).ok_or_else(|| QueryError::PortFailure {
                stage: "sparse_candidate_source",
                message: "sparse exact query returned an invalid point ID".to_owned(),
            })?;
            let score = f64::from(score);
            candidates.push(
                Candidate::new(
                    point_id,
                    score,
                    candidate_provenance(
                        point_id,
                        CandidateBranch::Sparse,
                        CandidateSourceKind::Sparse,
                        ScoreOrder::LowerIsBetter,
                        SourceAuthority::PostgreSqlRow,
                    )?,
                )?
                .with_exact_score(score)?
                .with_diagnostics(CandidateDiagnostics::new(
                    u32::try_from(candidates.len()).unwrap_or(u32::MAX),
                    1,
                )),
            );
        }
        Ok(CandidatePage::with_scored_count(
            candidates,
            scored_count,
            true,
        ))
    })
}

fn admitted_exact_sparse_count(reported_count: i64, maximum: usize) -> Result<usize> {
    let actual = usize::try_from(reported_count).map_err(|_| QueryError::PortFailure {
        stage: "sparse_candidate_source",
        message: "negative sparse exact admission count".to_owned(),
    })?;
    if actual > maximum {
        return Err(QueryError::WorkBudgetExceeded {
            budget: "candidate_comparisons",
            actual,
            maximum,
        });
    }
    Ok(actual)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the exact sparse SQL inputs mirror the candidate port contract"
)]
fn exact_sparse_candidates_with_filter(
    collection_id: i64,
    query: SparseVec,
    table_name: String,
    vector_column: String,
    distance_function: &'static str,
    sql_limit: i64,
    point_ids: Vec<i64>,
) -> Result<CandidatePage> {
    let sql = format!(
        "SELECT point_id, score, total_count
           FROM (
                SELECT points.point_id,
                       pgcontext.{distance_function}(source.{vector_column}, $1) AS score,
                       count(*) OVER ()::bigint AS total_count
                  FROM pgcontext._visible_collection_points AS points
                  JOIN {table_name} AS source ON source.id::text = points.source_key
                 WHERE points.collection_id = $2
                   AND points.deleted_at IS NULL
                   AND points.point_id = ANY($3::bigint[])
           ) AS scored
          ORDER BY score ASC, point_id ASC
          LIMIT $4"
    );
    let mut args = Vec::<pgrx::datum::DatumWithOid<'_>>::with_capacity(4);
    args.push(query.into());
    args.push(collection_id.into());
    args.push(point_ids.into());
    args.push(sql_limit.into());
    Spi::connect(|client| {
        let rows = client
            .select(&sql, Some(sql_limit), &args)
            .map_err(|error| port_failure("sparse_candidate_source", error))?;
        let mut scored_count = 0_usize;
        let mut candidates = Vec::with_capacity(usize::try_from(sql_limit).unwrap_or_default());
        for (rank, row) in rows.into_iter().enumerate() {
            scored_count = usize::try_from(spi_column::<i64>(&row, 3, "sparse_candidate_source")?)
                .map_err(|_| QueryError::PortFailure {
                    stage: "sparse_candidate_source",
                    message: "negative sparse scored count".to_owned(),
                })?;
            let point_id = spi_point_id(&row, 1, "sparse_candidate_source")?;
            let score = f64::from(spi_column::<f32>(&row, 2, "sparse_candidate_source")?);
            candidates.push(
                Candidate::new(
                    point_id,
                    score,
                    candidate_provenance(
                        point_id,
                        CandidateBranch::Sparse,
                        CandidateSourceKind::Sparse,
                        ScoreOrder::LowerIsBetter,
                        SourceAuthority::PostgreSqlRow,
                    )?,
                )?
                .with_exact_score(score)?
                .with_diagnostics(CandidateDiagnostics::new(
                    u32::try_from(rank).unwrap_or(u32::MAX),
                    1,
                )),
            );
        }
        Ok(CandidatePage::with_scored_count(
            candidates,
            scored_count,
            true,
        ))
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "the sparse HNSW adapter keeps query work and memory ceilings explicit"
)]
fn hnsw_sparse_candidates(
    collection_id: i64,
    registered_vector: &RegisteredSparseVector,
    index_oid: pg_sys::Oid,
    query: &QueryIr,
    filter: Option<&context_query::FilterCandidateBatch>,
    limit: usize,
    max_comparisons: usize,
    max_memory_bytes: usize,
) -> Result<CandidatePage> {
    let query = SparseVec::from_sparse(sparse_query(query)?.clone());
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let sql_limit = sql_limit(limit, "sparse_candidate_source")?;
    let hnsw_limit = i32::try_from(limit).map_err(|_| QueryError::PortFailure {
        stage: "sparse_candidate_source",
        message: format!("sparse HNSW candidate limit {limit} exceeds PostgreSQL integer"),
    })?;
    let point_ids = filter
        .map(|filter| super::sql_point_ids(filter.point_ids().iter().copied()))
        .transpose()?;
    let sql = if point_ids.is_some() {
        format!(
            "WITH candidate_mask AS MATERIALIZED (
                 SELECT array_agg(source.ctid ORDER BY source.ctid) AS heap_tids
                   FROM pgcontext._visible_collection_points AS points
                   JOIN {table_name} AS source ON source.id::text = points.source_key
                  WHERE points.collection_id = $2
                    AND points.deleted_at IS NULL
                    AND points.point_id = ANY($3::bigint[])
             ),
             ann_candidates AS MATERIALIZED (
                 SELECT ann.heap_tid, ann.score::float8 AS score
                   FROM candidate_mask
                  CROSS JOIN LATERAL pgcontext._hnsw_sparse_masked_candidates(
                        $5, $1, candidate_mask.heap_tids, $4
                    ) AS ann
             )
             SELECT points.point_id, ann.score
               FROM ann_candidates AS ann
               JOIN {table_name} AS source ON source.ctid::text = ann.heap_tid
               JOIN pgcontext._visible_collection_points AS points
                 ON points.source_key = source.id::text
              WHERE points.collection_id = $2
                AND points.deleted_at IS NULL
              ORDER BY ann.score ASC, points.point_id ASC
              LIMIT $4"
        )
    } else {
        format!(
            "WITH candidate_mask AS MATERIALIZED (
                 SELECT array_agg(source.ctid ORDER BY source.ctid) AS heap_tids
                   FROM pgcontext._visible_collection_points AS points
                   JOIN {table_name} AS source ON source.id::text = points.source_key
                  WHERE points.collection_id = $2
                    AND points.deleted_at IS NULL
             ),
             ann_candidates AS MATERIALIZED (
                 SELECT ann.heap_tid, ann.score::float8 AS score
                   FROM candidate_mask
                  CROSS JOIN LATERAL pgcontext._hnsw_sparse_masked_candidates(
                        $4, $1, candidate_mask.heap_tids, $3
                    ) AS ann
             )
             SELECT points.point_id, ann.score
               FROM ann_candidates AS ann
               JOIN {table_name} AS source ON source.ctid::text = ann.heap_tid
               JOIN pgcontext._visible_collection_points AS points
                 ON points.source_key = source.id::text
              WHERE points.collection_id = $2
                AND points.deleted_at IS NULL
              ORDER BY ann.score ASC, points.point_id ASC
              LIMIT $3"
        )
    };
    let mut args = Vec::<pgrx::datum::DatumWithOid<'_>>::with_capacity(5);
    args.push(query.into());
    args.push(collection_id.into());
    if let Some(point_ids) = point_ids {
        args.push(point_ids.into());
    }
    args.push(hnsw_limit.into());
    args.push(index_oid.into());
    let candidates = crate::hnsw_am::with_hnsw_candidate_helper_budget(
        index_oid,
        max_comparisons,
        max_memory_bytes,
        || {
            Spi::connect(|client| {
                let rows = client
                    .select(&sql, Some(sql_limit), &args)
                    .map_err(|error| port_failure("sparse_candidate_source", error))?;
                let mut candidates = Vec::with_capacity(limit);
                for (rank, row) in rows.into_iter().enumerate() {
                    let point_id = spi_point_id(&row, 1, "sparse_candidate_source")?;
                    candidates.push(
                        Candidate::new(
                            point_id,
                            spi_column::<f64>(&row, 2, "sparse_candidate_source")?,
                            candidate_provenance(
                                point_id,
                                CandidateBranch::Sparse,
                                CandidateSourceKind::Hnsw,
                                ScoreOrder::LowerIsBetter,
                                SourceAuthority::DerivedArtifact,
                            )?,
                        )?
                        .with_diagnostics(CandidateDiagnostics::new(
                            u32::try_from(rank).unwrap_or(u32::MAX),
                            1,
                        )),
                    );
                }
                Ok::<_, QueryError>(candidates)
            })
        },
    )?;
    let scored_count =
        Spi::get_one::<i64>("SELECT node_reads FROM pgcontext.hnsw_last_scan_work()")
            .map_err(|error| port_failure("sparse_candidate_source", error))?
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(candidates.len());
    Ok(CandidatePage::with_scored_count(
        candidates,
        scored_count,
        true,
    ))
}

fn visible_sparse_mask_size(
    collection_id: i64,
    registered_vector: &RegisteredSparseVector,
    mask_limit: usize,
) -> Result<usize> {
    if mask_limit == 0 {
        return Ok(0);
    }
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let probe_limit = sql_limit(
        mask_limit.saturating_add(1),
        "sparse_visibility_candidate_source",
    )?;
    let sql = format!(
        "SELECT count(*)::bigint
           FROM (
                SELECT 1
                  FROM pgcontext._visible_collection_points AS points
                  JOIN {table_name} AS source ON source.id::text = points.source_key
                 WHERE points.collection_id = $1
                   AND points.deleted_at IS NULL
                 LIMIT $2
           ) AS visible_points"
    );
    Spi::connect(|client| {
        let rows = client
            .select(&sql, Some(1), &[collection_id.into(), probe_limit.into()])
            .map_err(|error| port_failure("sparse_visibility_candidate_source", error))?;
        let row = rows
            .into_iter()
            .next()
            .ok_or_else(|| QueryError::PortFailure {
                stage: "sparse_visibility_candidate_source",
                message: "sparse visibility count returned no row".to_owned(),
            })?;
        let count = spi_column::<i64>(&row, 1, "sparse_visibility_candidate_source")?;
        let count = usize::try_from(count).map_err(|_| QueryError::PortFailure {
            stage: "sparse_visibility_candidate_source",
            message: "negative sparse visibility count".to_owned(),
        })?;
        Ok(count)
    })
}

fn sparse_query_copy_bytes(
    query: &SparseVector,
    sparse_entry_copies: usize,
    dense_value_copies: usize,
) -> Result<usize> {
    sparse_query_shape_bytes(
        query.dimensions(),
        query.non_zero_count(),
        sparse_entry_copies,
        dense_value_copies,
    )
}

fn sparse_query_shape_bytes(
    dimensions: usize,
    non_zero_count: usize,
    sparse_entry_copies: usize,
    dense_value_copies: usize,
) -> Result<usize> {
    let sparse_bytes = non_zero_count
        .checked_mul(size_of::<SparseEntry>())
        .and_then(|bytes| bytes.checked_mul(sparse_entry_copies))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "sparse_query_entry_memory_projection",
        })?;
    let dense_bytes = dimensions
        .checked_mul(size_of::<f32>())
        .and_then(|bytes| bytes.checked_mul(dense_value_copies))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "sparse_query_dense_memory_projection",
        })?;
    sparse_bytes
        .checked_add(dense_bytes)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "sparse_query_memory_projection",
        })
}

fn sparse_candidate_peak_bytes(
    limit: usize,
    filter_ids: usize,
    query_memory: usize,
) -> Result<usize> {
    limit
        .checked_mul(size_of::<Candidate>())
        .and_then(|bytes| {
            filter_ids
                .checked_mul(size_of::<i64>())
                .and_then(|filter_bytes| bytes.checked_add(filter_bytes))
        })
        .and_then(|bytes| bytes.checked_add(query_memory))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "sparse_candidate_memory_projection",
        })
}

fn sparse_recheck_peak_bytes(
    candidate_count: usize,
    output_count: usize,
    query_memory: usize,
) -> Result<usize> {
    let point_id_bytes = candidate_count.checked_mul(size_of::<i64>());
    let output_bytes = output_count.checked_mul(size_of::<HydratedCandidate>());
    let key_bytes = output_count
        .checked_mul(context_core::policy::MAX_SOURCE_KEY_BYTES)
        .and_then(|bytes| bytes.checked_mul(2));
    point_id_bytes
        .and_then(|bytes| output_bytes.and_then(|output| bytes.checked_add(output)))
        .and_then(|bytes| key_bytes.and_then(|keys| bytes.checked_add(keys)))
        .and_then(|bytes| bytes.checked_add(query_memory))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "sparse_recheck_memory_projection",
        })
}

fn require_sparse_memory(actual: usize, maximum: usize, budget: &'static str) -> Result<()> {
    if actual > maximum {
        return Err(QueryError::WorkBudgetExceeded {
            budget,
            actual,
            maximum,
        });
    }
    Ok(())
}

fn sparse_query(query: &QueryIr) -> Result<&SparseVector> {
    match query.kind() {
        QueryKind::SparseNearest { vector, .. } => Ok(vector),
        _ => Err(QueryError::PortFailure {
            stage: "sparse_candidate_source",
            message: "sparse PostgreSQL adapter requires a sparse-nearest query".to_owned(),
        }),
    }
}

#[cfg(test)]
mod exact_sparse_admission_tests {
    use super::*;

    #[test]
    fn exact_sparse_admission_accepts_the_inclusive_comparison_boundary() {
        assert!(matches!(admitted_exact_sparse_count(4, 4), Ok(4)));
    }

    #[test]
    fn exact_sparse_admission_rejects_the_bounded_probe_without_scored_results() {
        assert!(matches!(
            admitted_exact_sparse_count(5, 4),
            Err(QueryError::WorkBudgetExceeded {
                budget: "candidate_comparisons",
                actual: 5,
                maximum: 4,
            })
        ));
    }

    #[test]
    fn maximum_sparse_hnsw_query_is_admitted_only_at_exact_boundary() {
        let dimensions = context_core::policy::MAX_VECTOR_DIMENSIONS;
        let query_bytes = sparse_query_shape_bytes(dimensions, dimensions, 3, 3)
            .unwrap_or_else(|error| unreachable!("policy sparse shape must fit usize: {error}"));
        let projected = sparse_candidate_peak_bytes(1, 0, query_bytes)
            .unwrap_or_else(|error| unreachable!("policy sparse peak must fit usize: {error}"));

        assert!(require_sparse_memory(projected, projected, "candidate_memory").is_ok());
        assert!(require_sparse_memory(projected, projected - 1, "candidate_memory").is_err());
    }

    #[test]
    fn sparse_hnsw_candidate_tuple_materialization_has_an_exact_memory_boundary() {
        let limit = 17;
        let query_bytes = sparse_query_shape_bytes(64, 8, 3, 3)
            .unwrap_or_else(|error| unreachable!("test sparse shape must fit usize: {error}"));
        let projected = sparse_candidate_peak_bytes(limit, 0, query_bytes)
            .unwrap_or_else(|error| unreachable!("test sparse peak must fit usize: {error}"));

        assert!(require_sparse_memory(projected, projected, "candidate_memory").is_ok());
        assert!(require_sparse_memory(projected, projected - 1, "candidate_memory").is_err());
    }

    #[test]
    fn sparse_recheck_tuple_materialization_has_an_exact_memory_boundary() {
        let candidate_count = 23;
        let output_count = 11;
        let query_bytes = sparse_query_shape_bytes(64, 8, 2, 0)
            .unwrap_or_else(|error| unreachable!("test sparse shape must fit usize: {error}"));
        let projected = sparse_recheck_peak_bytes(candidate_count, output_count, query_bytes)
            .unwrap_or_else(|error| unreachable!("test sparse peak must fit usize: {error}"));

        assert!(require_sparse_memory(projected, projected, "source_recheck_memory").is_ok());
        assert!(require_sparse_memory(projected, projected - 1, "source_recheck_memory").is_err());
    }
}
