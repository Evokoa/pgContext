//! PostgreSQL ports for named sparse exact/ANN execution.

use context_core::{CollectionName, SourceKey, SparseVector};
use context_query::{
    Candidate, CandidateBranch, CandidatePage, CandidateSource, ExecutionBudget, ExecutionOutcome,
    HydratedCandidate, QueryError, QueryExecutor, QueryIr, QueryKind, Result, SourceReadiness,
    SourceRechecker, StageDiagnostic, TelemetrySink,
};
use pgrx::prelude::*;
use serde_json::Value;

use super::{
    PgCancellation, outcome_rows, port_failure, require_complete_outcome, spi_column, spi_point_id,
    sql_limit,
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
    ) -> Result<CandidatePage> {
        let page = match self.strategy {
            SparseCandidateStrategy::Exact => exact_sparse_candidates(
                collection_id,
                &self.registered_vector,
                query,
                filter,
                limit,
            )?,
            SparseCandidateStrategy::Hnsw(index_oid) => hnsw_sparse_candidates(
                collection_id,
                &self.registered_vector,
                index_oid,
                query,
                filter,
                limit,
            )?,
        };
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
    ) -> Result<Vec<HydratedCandidate>> {
        SpiSparseSourceRechecker {
            collection_id,
            registered_vector: &self.registered_vector,
            filter_fields,
        }
        .recheck(query, candidates, limit)
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
    let has_filter = filter.is_some();
    let query = QueryIr::sparse_nearest(
        registered_vector.vector_name.clone(),
        query_vector,
        context_query::ScoreOrder::LowerIsBetter,
        filter,
        limit,
    )
    .unwrap_or_else(|error| crate::error::raise_query_error(error));
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
    let mut telemetry = SparseTelemetry::default();
    let cancellation = PgCancellation;
    let outcome = QueryExecutor::new(
        &mut candidate_source,
        filter_port,
        &mut rechecker,
        &mut telemetry,
        &cancellation,
    )
    .execute(&query, budget)
    .unwrap_or_else(|error| crate::error::raise_query_error(error));
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
    fn readiness(&mut self, query: &QueryIr) -> Result<SourceReadiness> {
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
    ) -> Result<CandidatePage> {
        match self.strategy {
            SparseCandidateStrategy::Exact => exact_sparse_candidates(
                self.collection_id,
                self.registered_vector,
                query,
                filter,
                limit,
            ),
            SparseCandidateStrategy::Hnsw(index_oid) => hnsw_sparse_candidates(
                self.collection_id,
                self.registered_vector,
                index_oid,
                query,
                filter,
                limit,
            ),
        }
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
    ) -> Result<Vec<HydratedCandidate>> {
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
        let sql = format!(
            "SELECT points.point_id,
                    points.source_key,
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
            rows.into_iter()
                .map(|row| {
                    HydratedCandidate::new(
                        spi_point_id(&row, 1, "sparse_source_rechecker")?,
                        SourceKey::new(spi_column::<String>(&row, 2, "sparse_source_rechecker")?)?,
                        f64::from(spi_column::<f32>(&row, 3, "sparse_source_rechecker")?),
                    )
                })
                .collect()
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
    ) -> Result<context_query::FilterCandidateBatch> {
        let filter = query.filter().ok_or_else(|| QueryError::PortFailure {
            stage: "sparse_filter_candidate_source",
            message: "sparse filter adapter called without a query filter".to_owned(),
        })?;
        let plan = resolve_typed_filter_plan(self.filter_fields, filter, 2)
            .map_err(|error| port_failure("sparse_filter_candidate_source", error))?;
        let table_name = quote_qualified_identifier(
            &self.registered_vector.schema_name,
            &self.registered_vector.table_name,
        );
        let probe_limit = limit.saturating_add(1);
        let sql_limit = sql_limit(probe_limit, "sparse_filter_candidate_source")?;
        let sql = format!(
            "SELECT points.point_id
               FROM pgcontext._visible_collection_points AS points
               JOIN {table_name} AS source ON source.id::text = points.source_key
              WHERE points.collection_id = $1
                AND points.deleted_at IS NULL
                AND {}
              ORDER BY points.point_id
              LIMIT $2",
            plan.sql
        );
        let mut args =
            Vec::<pgrx::datum::DatumWithOid<'_>>::with_capacity(2 + plan.parameters.len());
        args.push(self.collection_id.into());
        args.push(sql_limit.into());
        push_filter_parameter_args(&mut args, &plan.parameters);

        Spi::connect(|client| {
            let rows = client
                .select(&sql, Some(sql_limit), &args)
                .map_err(|error| port_failure("sparse_filter_candidate_source", error))?;
            let mut point_ids = rows
                .into_iter()
                .map(|row| spi_point_id(&row, 1, "sparse_filter_candidate_source"))
                .collect::<Result<Vec<_>>>()?;
            let exhausted = point_ids.len() <= limit;
            point_ids.truncate(limit);
            Ok(context_query::FilterCandidateBatch::new(
                point_ids, exhausted,
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
    let sparse_query = SparseVec::from_sparse(sparse_query(query)?.clone());
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let vector_column = quote_identifier(&registered_vector.vector_column_name);
    let distance_function = sparse_distance_function(registered_vector.metric);
    let sql_limit = sql_limit(limit, "sparse_candidate_source")?;
    let point_ids = filter
        .map(|filter| super::sql_point_ids(filter.point_ids().iter().copied()))
        .transpose()?;
    if point_ids.as_ref().is_some_and(Vec::is_empty) {
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
    point_ids: Option<Vec<i64>>,
) -> Result<CandidatePage> {
    let filter_sql = if point_ids.is_some() {
        " AND points.point_id = ANY($3::bigint[])"
    } else {
        ""
    };
    let limit_placeholder = if point_ids.is_some() { 4 } else { 3 };
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
                   {filter_sql}
           ) AS scored
          ORDER BY score ASC, point_id ASC
          LIMIT ${limit_placeholder}"
    );
    let mut args = Vec::<pgrx::datum::DatumWithOid<'_>>::with_capacity(4);
    args.push(query.into());
    args.push(collection_id.into());
    if let Some(point_ids) = point_ids {
        args.push(point_ids.into());
    }
    args.push(sql_limit.into());
    Spi::connect(|client| {
        let rows = client
            .select(&sql, Some(sql_limit), &args)
            .map_err(|error| port_failure("sparse_candidate_source", error))?;
        let mut scored_count = 0_usize;
        let mut candidates = Vec::new();
        for row in rows {
            scored_count = usize::try_from(spi_column::<i64>(&row, 3, "sparse_candidate_source")?)
                .map_err(|_| QueryError::PortFailure {
                    stage: "sparse_candidate_source",
                    message: "negative sparse scored count".to_owned(),
                })?;
            candidates.push(Candidate::new(
                spi_point_id(&row, 1, "sparse_candidate_source")?,
                f64::from(spi_column::<f32>(&row, 2, "sparse_candidate_source")?),
                CandidateBranch::Sparse,
            )?);
        }
        Ok(CandidatePage::with_scored_count(
            candidates,
            scored_count,
            true,
        ))
    })
}

fn hnsw_sparse_candidates(
    collection_id: i64,
    registered_vector: &RegisteredSparseVector,
    index_oid: pg_sys::Oid,
    query: &QueryIr,
    filter: Option<&context_query::FilterCandidateBatch>,
    limit: usize,
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
    let candidates = crate::hnsw_am::with_hnsw_candidate_helper_capability(index_oid, || {
        Spi::connect(|client| {
            let rows = client
                .select(&sql, Some(sql_limit), &args)
                .map_err(|error| port_failure("sparse_candidate_source", error))?;
            rows.into_iter()
                .map(|row| {
                    Candidate::new(
                        spi_point_id(&row, 1, "sparse_candidate_source")?,
                        spi_column::<f64>(&row, 2, "sparse_candidate_source")?,
                        CandidateBranch::Sparse,
                    )
                })
                .collect::<Result<Vec<_>>>()
        })
    })?;
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

fn sparse_query(query: &QueryIr) -> Result<&SparseVector> {
    match query.kind() {
        QueryKind::SparseNearest { vector, .. } => Ok(vector),
        _ => Err(QueryError::PortFailure {
            stage: "sparse_candidate_source",
            message: "sparse PostgreSQL adapter requires a sparse-nearest query".to_owned(),
        }),
    }
}

#[derive(Default)]
struct SparseTelemetry {
    _diagnostics: Vec<StageDiagnostic>,
}

impl TelemetrySink for SparseTelemetry {
    fn record(&mut self, diagnostic: &StageDiagnostic) -> Result<()> {
        self._diagnostics.push(diagnostic.clone());
        Ok(())
    }
}
