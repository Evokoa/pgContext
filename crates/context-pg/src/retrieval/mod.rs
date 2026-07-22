//! PostgreSQL adapters for the transport-neutral query executor.

use context_core::{CollectionName, DenseVector, DistanceMetric, PointId, SourceKey};
use context_query::{
    Cancellation, Candidate, CandidateBranch, CandidatePage, CandidateSource, ExecutionBudget,
    ExecutionOutcome, FilterCandidateBatch, FilterCandidateSource, HydratedCandidate, QueryError,
    QueryExecutor, QueryIr, QueryKind, Result, SourceReadiness, SourceRechecker, StageDiagnostic,
    TelemetrySink,
};
use pgrx::datum::DatumWithOid;
use pgrx::prelude::*;

use crate::error::raise_query_error;
use crate::table_search::{
    FilterField, SearchVector, distance_function, load_filter_fields, push_filter_parameter_args,
    quote_identifier, quote_qualified_identifier, require_collection_owner,
    require_table_select_privilege, resolve_collection, resolve_registered_vector,
    resolve_typed_filter_plan, validate_search_drift,
};
use crate::vector::Vector;

/// Selects the PostgreSQL candidate-generation adapter for one execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CandidateAdapter {
    /// Exhaustive exact distance ordering.
    Exact,
    /// Attached PostgreSQL HNSW index ordering.
    Hnsw,
}

/// Exact SPI candidate generation over the caller-visible source rows.
pub(crate) struct SpiExactCandidateSource<'a> {
    collection_id: i64,
    registered_vector: &'a SearchVector,
}

impl<'a> SpiExactCandidateSource<'a> {
    fn new(collection_id: i64, registered_vector: &'a SearchVector) -> Self {
        Self {
            collection_id,
            registered_vector,
        }
    }
}

impl CandidateSource for SpiExactCandidateSource<'_> {
    fn readiness(&mut self, query: &QueryIr) -> Result<SourceReadiness> {
        nearest_vector(query)?;
        Ok(SourceReadiness::Ready)
    }

    fn candidates(
        &mut self,
        query: &QueryIr,
        filter: Option<&FilterCandidateBatch>,
        limit: usize,
    ) -> Result<CandidatePage> {
        candidate_rows(
            self.collection_id,
            self.registered_vector,
            query,
            filter,
            limit,
            CandidateAdapter::Exact,
        )
    }
}

/// HNSW-backed SPI candidate generation over an attached dense-vector index.
pub(crate) struct SpiHnswCandidateSource<'a> {
    collection_id: i64,
    registered_vector: &'a SearchVector,
}

impl<'a> SpiHnswCandidateSource<'a> {
    fn new(collection_id: i64, registered_vector: &'a SearchVector) -> Self {
        Self {
            collection_id,
            registered_vector,
        }
    }
}

impl CandidateSource for SpiHnswCandidateSource<'_> {
    fn readiness(&mut self, query: &QueryIr) -> Result<SourceReadiness> {
        nearest_vector(query)?;
        Ok(if self.registered_vector.hnsw_index_oid.is_some() {
            SourceReadiness::Ready
        } else {
            SourceReadiness::NotReady {
                reason: context_query::ReadinessReason::GenerationMissing,
            }
        })
    }

    fn candidates(
        &mut self,
        query: &QueryIr,
        filter: Option<&FilterCandidateBatch>,
        limit: usize,
    ) -> Result<CandidatePage> {
        candidate_rows(
            self.collection_id,
            self.registered_vector,
            query,
            filter,
            limit,
            CandidateAdapter::Hnsw,
        )
    }
}

/// Authoritative source-row hydration and exact score recheck.
pub(crate) struct SpiSourceRechecker<'a> {
    collection_id: i64,
    registered_vector: &'a SearchVector,
    filter_fields: &'a [FilterField],
}

impl SourceRechecker for SpiSourceRechecker<'_> {
    fn recheck(
        &mut self,
        query: &QueryIr,
        candidates: &[Candidate],
        limit: usize,
    ) -> Result<Vec<HydratedCandidate>> {
        let query_vector = sql_vector(query)?;
        let point_ids = sql_point_ids(candidates.iter().map(Candidate::point_id))?;
        let table_name = quote_qualified_identifier(
            &self.registered_vector.schema_name,
            &self.registered_vector.table_name,
        );
        let vector_column = quote_identifier(&self.registered_vector.vector_column_name);
        let distance_function = distance_function(self.registered_vector.metric);
        let filter_plan = query
            .filter()
            .map(|filter| resolve_typed_filter_plan(self.filter_fields, filter, 4))
            .transpose()
            .map_err(|error| port_failure("source_rechecker", error))?;
        let filter_sql = filter_plan
            .as_ref()
            .map(|plan| format!(" AND {}", plan.sql))
            .unwrap_or_default();
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
        let sql_limit = sql_limit(limit, "source_rechecker")?;
        let parameters = filter_plan
            .as_ref()
            .map(|plan| plan.parameters.as_slice())
            .unwrap_or(&[]);
        let mut args = Vec::<DatumWithOid<'_>>::with_capacity(4 + parameters.len());
        args.push(query_vector.into());
        args.push(self.collection_id.into());
        args.push(point_ids.into());
        args.push(sql_limit.into());
        push_filter_parameter_args(&mut args, parameters);

        Spi::connect(|client| {
            let rows = client
                .select(&sql, Some(sql_limit), &args)
                .map_err(|error| port_failure("source_rechecker", error))?;
            let mut output = Vec::new();
            for row in rows {
                let point_id = spi_point_id(&row, 1, "source_rechecker")?;
                let source_key = spi_column::<String>(&row, 2, "source_rechecker")?;
                let score = spi_column::<f32>(&row, 3, "source_rechecker")?;
                output.push(HydratedCandidate::new(
                    point_id,
                    SourceKey::new(source_key)?,
                    f64::from(score),
                )?);
            }
            Ok(output)
        })
    }
}

/// SPI filter adapter that resolves only registered public filter fields.
pub(crate) struct SpiFilterCandidateSource<'a> {
    collection_id: i64,
    registered_vector: &'a SearchVector,
    filter_fields: &'a [FilterField],
}

impl FilterCandidateSource for SpiFilterCandidateSource<'_> {
    fn filter_candidates(&mut self, query: &QueryIr, limit: usize) -> Result<FilterCandidateBatch> {
        let filter = query.filter().ok_or_else(|| QueryError::PortFailure {
            stage: "filter_candidate_source",
            message: "filter adapter called without a query filter".to_owned(),
        })?;
        let plan = resolve_typed_filter_plan(self.filter_fields, filter, 2)
            .map_err(|error| port_failure("filter_candidate_source", error))?;
        let table_name = quote_qualified_identifier(
            &self.registered_vector.schema_name,
            &self.registered_vector.table_name,
        );
        let probe_limit = limit.saturating_add(1);
        let sql_limit = sql_limit(probe_limit, "filter_candidate_source")?;
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
        let mut args = Vec::<DatumWithOid<'_>>::with_capacity(2 + plan.parameters.len());
        args.push(self.collection_id.into());
        args.push(sql_limit.into());
        push_filter_parameter_args(&mut args, &plan.parameters);

        Spi::connect(|client| {
            let rows = client
                .select(&sql, Some(sql_limit), &args)
                .map_err(|error| port_failure("filter_candidate_source", error))?;
            let mut point_ids = rows
                .into_iter()
                .map(|row| spi_point_id(&row, 1, "filter_candidate_source"))
                .collect::<Result<Vec<_>>>()?;
            let exhausted = point_ids.len() <= limit;
            point_ids.truncate(limit);
            Ok(FilterCandidateBatch::new(point_ids, exhausted))
        })
    }
}

/// Stage-I seam; Stage B intentionally records no persistent telemetry.
#[derive(Default)]
pub(crate) struct PgTelemetrySink;

impl TelemetrySink for PgTelemetrySink {
    fn record(&mut self, _diagnostic: &StageDiagnostic) -> Result<()> {
        Ok(())
    }
}

/// PostgreSQL cooperative cancellation bridge.
pub(crate) struct PgCancellation;

#[allow(
    unsafe_code,
    reason = "pgrx's interrupt checkpoint macro enters PostgreSQL's audited FFI boundary"
)]
impl Cancellation for PgCancellation {
    fn check_interrupt(&self) -> Result<()> {
        // SAFETY: pgrx expands this to PostgreSQL's standard backend interrupt
        // checkpoint; no pointer or borrowed PostgreSQL memory escapes.
        pg_sys::check_for_interrupts!();
        Ok(())
    }

    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Resolves PostgreSQL security/catalog state and executes one query IR.
pub(crate) fn run_query(
    collection_name: &CollectionName,
    query: QueryIr,
    adapter: CandidateAdapter,
) -> Vec<(i64, String, f32)> {
    let collection = resolve_collection(collection_name);
    require_collection_owner(&collection, collection_name);
    let mut registered_vector =
        resolve_registered_vector(collection_name, collection.collection_id);
    validate_search_drift(collection.collection_id, &mut registered_vector);
    require_table_select_privilege(&registered_vector);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        collection_name,
        query.limit(),
    );
    let filter_fields = query
        .filter()
        .map(|_| load_filter_fields(collection.collection_id))
        .unwrap_or_default();

    let outcome = execute_prepared_query(
        collection.collection_id,
        &registered_vector,
        &filter_fields,
        &query,
        adapter,
    )
    .unwrap_or_else(|error| raise_query_error(error));
    outcome_rows(&outcome).unwrap_or_else(|error| raise_query_error(error))
}

fn execute_prepared_query(
    collection_id: i64,
    registered_vector: &SearchVector,
    filter_fields: &[FilterField],
    query: &QueryIr,
    adapter: CandidateAdapter,
) -> Result<ExecutionOutcome> {
    let candidate_limit = match adapter {
        CandidateAdapter::Exact => query.limit(),
        CandidateAdapter::Hnsw => {
            crate::settings::hnsw_candidate_budget_from_guc().max(query.limit())
        }
    };
    let budget = ExecutionBudget::new(
        candidate_limit,
        context_core::policy::MAX_RECALL_CHECK_POINT_IDS,
        candidate_limit,
        3,
        1,
        query.limit(),
    )?;
    let mut exact;
    let mut hnsw;
    let candidates: &mut dyn CandidateSource = match adapter {
        CandidateAdapter::Exact => {
            exact = SpiExactCandidateSource::new(collection_id, registered_vector);
            &mut exact
        }
        CandidateAdapter::Hnsw => {
            hnsw = SpiHnswCandidateSource::new(collection_id, registered_vector);
            &mut hnsw
        }
    };
    let mut filter = SpiFilterCandidateSource {
        collection_id,
        registered_vector,
        filter_fields,
    };
    let filter_port = query
        .filter()
        .map(|_| &mut filter as &mut dyn FilterCandidateSource);
    let mut rechecker = SpiSourceRechecker {
        collection_id,
        registered_vector,
        filter_fields,
    };
    let mut telemetry = PgTelemetrySink;
    let cancellation = PgCancellation;
    QueryExecutor::new(
        candidates,
        filter_port,
        &mut rechecker,
        &mut telemetry,
        &cancellation,
    )
    .execute(query, budget)
}

fn candidate_rows(
    collection_id: i64,
    registered_vector: &SearchVector,
    query: &QueryIr,
    filter: Option<&FilterCandidateBatch>,
    limit: usize,
    adapter: CandidateAdapter,
) -> Result<CandidatePage> {
    let query_vector = sql_vector(query)?;
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let vector_column = quote_identifier(&registered_vector.vector_column_name);
    let score_expression = match adapter {
        CandidateAdapter::Exact => format!(
            "pgcontext.{}(source.{vector_column}, $1)",
            distance_function(registered_vector.metric)
        ),
        CandidateAdapter::Hnsw => format!(
            "source.{vector_column} OPERATOR(pgcontext.{}) $1",
            distance_operator(registered_vector.metric)?
        ),
    };
    let probe_limit = limit.saturating_add(1);
    let sql_limit = sql_limit(probe_limit, "candidate_source")?;
    let (filter_sql, point_ids) = match filter {
        Some(filter) => (
            " AND points.point_id = ANY($3::bigint[])",
            Some(sql_point_ids(filter.point_ids().iter().copied())?),
        ),
        None => ("", None),
    };
    let limit_placeholder = if point_ids.is_some() { 4 } else { 3 };
    let sql = format!(
        "SELECT points.point_id, {score_expression} AS score
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $2
            AND points.deleted_at IS NULL
            {filter_sql}
          ORDER BY score ASC, points.point_id ASC
          LIMIT ${limit_placeholder}"
    );
    let mut args = Vec::<DatumWithOid<'_>>::with_capacity(4);
    args.push(query_vector.into());
    args.push(collection_id.into());
    if let Some(point_ids) = point_ids {
        args.push(point_ids.into());
    }
    args.push(sql_limit.into());

    Spi::connect(|client| {
        let rows = client
            .select(&sql, Some(sql_limit), &args)
            .map_err(|error| port_failure("candidate_source", error))?;
        let branch = match adapter {
            CandidateAdapter::Exact => CandidateBranch::DenseExact,
            CandidateAdapter::Hnsw => CandidateBranch::DenseAnn,
        };
        let mut candidates = Vec::new();
        for row in rows {
            let point_id = spi_point_id(&row, 1, "candidate_source")?;
            let score = match adapter {
                CandidateAdapter::Exact => {
                    f64::from(spi_column::<f32>(&row, 2, "candidate_source")?)
                }
                CandidateAdapter::Hnsw => spi_column::<f64>(&row, 2, "candidate_source")?,
            };
            candidates.push(Candidate::new(point_id, score, branch)?);
        }
        let exhausted = candidates.len() <= limit;
        candidates.truncate(limit);
        Ok(CandidatePage::new(candidates, exhausted))
    })
}

fn nearest_vector(query: &QueryIr) -> Result<&DenseVector> {
    match query.kind() {
        QueryKind::Nearest { vector, .. } => Ok(vector),
        _ => Err(QueryError::PortFailure {
            stage: "candidate_source",
            message: "dense PostgreSQL adapter requires a nearest query".to_owned(),
        }),
    }
}

fn sql_vector(query: &QueryIr) -> Result<Vector> {
    Ok(Vector::from_dense(nearest_vector(query)?.clone()))
}

fn distance_operator(metric: DistanceMetric) -> Result<&'static str> {
    match metric {
        DistanceMetric::L2 => Ok("<->"),
        DistanceMetric::InnerProduct | DistanceMetric::NegativeInnerProduct => Ok("<#>"),
        DistanceMetric::Cosine => Ok("<=>"),
        DistanceMetric::L1 => Ok("<+>"),
        DistanceMetric::Hamming | DistanceMetric::Jaccard => Err(QueryError::PortFailure {
            stage: "candidate_source",
            message: "bit metrics cannot serve dense vector queries".to_owned(),
        }),
    }
}

fn sql_point_ids(point_ids: impl IntoIterator<Item = PointId>) -> Result<Vec<i64>> {
    point_ids
        .into_iter()
        .map(|point_id| {
            i64::try_from(point_id.get()).map_err(|_| QueryError::PortFailure {
                stage: "postgres_identity",
                message: format!("point ID {} exceeds PostgreSQL bigint", point_id.get()),
            })
        })
        .collect()
}

fn sql_limit(limit: usize, stage: &'static str) -> Result<i64> {
    i64::try_from(limit).map_err(|_| QueryError::PortFailure {
        stage,
        message: format!("work limit {limit} exceeds PostgreSQL bigint"),
    })
}

fn spi_point_id(
    row: &spi::SpiHeapTupleData<'_>,
    index: usize,
    stage: &'static str,
) -> Result<PointId> {
    let point_id = spi_column::<i64>(row, index, stage)?;
    PointId::from_i64(point_id).ok_or_else(|| QueryError::PortFailure {
        stage,
        message: format!("negative PostgreSQL point ID: {point_id}"),
    })
}

fn spi_column<T>(row: &spi::SpiHeapTupleData<'_>, index: usize, stage: &'static str) -> Result<T>
where
    T: FromDatum + IntoDatum,
{
    row.get::<T>(index)
        .map_err(|error| port_failure(stage, error))?
        .ok_or_else(|| QueryError::PortFailure {
            stage,
            message: format!("SPI column {index} is null"),
        })
}

fn port_failure(stage: &'static str, error: impl std::fmt::Display) -> QueryError {
    QueryError::PortFailure {
        stage,
        message: error.to_string(),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "PostgreSQL adapters widened original float4 scores to f64 DTOs"
)]
fn outcome_rows(outcome: &ExecutionOutcome) -> Result<Vec<(i64, String, f32)>> {
    outcome
        .points()
        .iter()
        .map(|point| {
            let point_id =
                i64::try_from(point.point_id().get()).map_err(|_| QueryError::PortFailure {
                    stage: "postgres_identity",
                    message: format!(
                        "point ID {} exceeds PostgreSQL bigint",
                        point.point_id().get()
                    ),
                })?;
            Ok((
                point_id,
                point.source_key().as_str().to_owned(),
                point.score() as f32,
            ))
        })
        .collect()
}

#[cfg(feature = "pg_test")]
pub(crate) fn differential_exact_rows_for_test(
    collection: String,
    vector: Vector,
    limit: i32,
) -> (Vec<(i64, String, f32)>, Vec<(i64, String, f32)>) {
    let collection_name = crate::table_search::collection_name_from_sql(collection);
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector(&collection_name, collection.collection_id);
    validate_search_drift(collection.collection_id, &mut registered_vector);
    require_table_select_privilege(&registered_vector);
    let limit = crate::table_search::search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    let legacy = crate::table_search::search_registered_table(
        collection.collection_id,
        &registered_vector,
        vector.clone(),
        limit,
    );
    let query = QueryIr::nearest(
        None,
        vector.as_slice().to_vec(),
        context_query::ScoreOrder::LowerIsBetter,
        None,
        limit.get(),
    )
    .unwrap_or_else(|error| raise_query_error(error));
    let outcome = execute_prepared_query(
        collection.collection_id,
        &registered_vector,
        &[],
        &query,
        CandidateAdapter::Exact,
    )
    .unwrap_or_else(|error| raise_query_error(error));
    let executor = outcome_rows(&outcome).unwrap_or_else(|error| raise_query_error(error));
    (legacy, executor)
}

#[cfg(feature = "pg_test")]
pub(crate) struct AdapterConformanceSnapshot {
    pub(crate) exact_rows: Vec<(i64, String, f32)>,
    pub(crate) hnsw_rows: Vec<(i64, String, f32)>,
    pub(crate) filter_candidates: usize,
    pub(crate) exact_candidates: usize,
    pub(crate) hnsw_candidates: usize,
    pub(crate) exact_rechecks: usize,
    pub(crate) hnsw_rechecks: usize,
}

#[cfg(feature = "pg_test")]
pub(crate) fn adapter_conformance_snapshot_for_test(
    collection: String,
) -> AdapterConformanceSnapshot {
    let collection_name = crate::table_search::collection_name_from_sql(collection);
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector(&collection_name, collection.collection_id);
    validate_search_drift(collection.collection_id, &mut registered_vector);
    require_table_select_privilege(&registered_vector);
    let filter_fields = load_filter_fields(collection.collection_id);
    let query = QueryIr::nearest(
        None,
        vec![0.0, 0.0],
        context_query::ScoreOrder::LowerIsBetter,
        Some(serde_json::json!({
            "must": [{"key": "tenant_id", "match": "acme"}]
        })),
        2,
    )
    .unwrap_or_else(|error| raise_query_error(error));
    let exact = execute_prepared_query(
        collection.collection_id,
        &registered_vector,
        &filter_fields,
        &query,
        CandidateAdapter::Exact,
    )
    .unwrap_or_else(|error| raise_query_error(error));
    let hnsw = execute_prepared_query(
        collection.collection_id,
        &registered_vector,
        &filter_fields,
        &query,
        CandidateAdapter::Hnsw,
    )
    .unwrap_or_else(|error| raise_query_error(error));
    let exact_usage = exact.usage();
    let hnsw_usage = hnsw.usage();
    AdapterConformanceSnapshot {
        exact_rows: outcome_rows(&exact).unwrap_or_else(|error| raise_query_error(error)),
        hnsw_rows: outcome_rows(&hnsw).unwrap_or_else(|error| raise_query_error(error)),
        filter_candidates: exact_usage.filter_candidates(),
        exact_candidates: exact_usage.candidates(),
        hnsw_candidates: hnsw_usage.candidates(),
        exact_rechecks: exact_usage.rechecks(),
        hnsw_rechecks: hnsw_usage.rechecks(),
    }
}
