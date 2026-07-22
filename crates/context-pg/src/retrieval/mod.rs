//! PostgreSQL adapters for the transport-neutral query executor.

mod sparse;
pub(crate) use sparse::{SparseCandidateStrategy, run_sparse_query};

use context_core::{CollectionName, DenseVector, PointId, SearchLimit, SourceKey};
use context_query::{
    Cancellation, Candidate, CandidateBranch, CandidatePage, CandidateSource, Completion,
    ExecutionBudget, ExecutionOutcome, ExecutionState, FilterCandidateBatch, FilterCandidateSource,
    HydratedCandidate, QueryError, QueryExecutor, QueryIr, QueryKind, Result, SourceReadiness,
    SourceRechecker, StageDiagnostic, TelemetrySink,
};
use pgrx::datum::DatumWithOid;
use pgrx::prelude::*;

use crate::error::{raise_query_error, raise_sql_error};
use crate::table_search::{
    FilterField, SearchVector, distance_function, load_filter_fields,
    load_mmap_artifact_candidates, mmap_delta_candidates, push_filter_parameter_args,
    quote_identifier, quote_qualified_identifier, require_collection_owner,
    require_table_select_privilege, resolve_collection, resolve_registered_vector,
    resolve_registered_vector_by_name, resolve_typed_filter_plan, validate_search_drift,
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

type RecheckCache = Rc<RefCell<BTreeMap<PointId, HydratedCandidate>>>;
type DenseVectorMap = BTreeMap<Option<String>, SearchVector>;
type SparseSourceCache = Rc<RefCell<BTreeMap<String, sparse::CompositeSparseSource>>>;
type LateInteractionCache =
    Rc<RefCell<Option<crate::hybrid_query::late_interaction_ann::CompositeLateInteractionSource>>>;
type QuantizedArtifactCache = Rc<RefCell<BTreeMap<Option<String>, String>>>;

#[derive(Debug, Clone)]
struct SourceTable {
    schema_name: String,
    table_name: String,
    table_oid: pg_sys::Oid,
}

impl From<&SearchVector> for SourceTable {
    fn from(vector: &SearchVector) -> Self {
        Self {
            schema_name: vector.schema_name.clone(),
            table_name: vector.table_name.clone(),
            table_oid: vector.table_oid,
        }
    }
}

struct PgCandidateRouter<'a> {
    collection_name: &'a str,
    collection_id: i64,
    registered_vectors: &'a DenseVectorMap,
    source_table: &'a SourceTable,
    adapter: CandidateAdapter,
    cache: RecheckCache,
    sparse_sources: SparseSourceCache,
    late_interaction: LateInteractionCache,
    quantized_artifacts: QuantizedArtifactCache,
}

impl CandidateSource for PgCandidateRouter<'_> {
    fn readiness(&mut self, query: &QueryIr) -> Result<SourceReadiness> {
        match query.kind() {
            QueryKind::Nearest { .. } => match self.adapter {
                CandidateAdapter::Exact => SpiExactCandidateSource::new(
                    self.collection_id,
                    registered_vector_for_query(self.registered_vectors, query)?,
                )
                .readiness(query),
                CandidateAdapter::Hnsw => {
                    let registered_vector =
                        registered_vector_for_query(self.registered_vectors, query)?;
                    if uses_quantized_mmap(query, registered_vector) {
                        let Some(artifact_name) = resolve_quantized_artifact(self.collection_id)?
                        else {
                            return Ok(SourceReadiness::NotReady {
                                reason: context_query::ReadinessReason::GenerationMissing,
                            });
                        };
                        self.quantized_artifacts
                            .borrow_mut()
                            .insert(dense_vector_key(query)?, artifact_name);
                        Ok(SourceReadiness::Ready)
                    } else {
                        SpiHnswCandidateSource::new(self.collection_id, registered_vector)
                            .readiness(query)
                    }
                }
            },
            QueryKind::SparseNearest { vector_name, .. } => {
                let collection_name = CollectionName::new(self.collection_name.to_owned())?;
                let source = sparse::CompositeSparseSource::prepare(
                    &collection_name,
                    self.collection_id,
                    query,
                )?;
                let readiness = source.readiness();
                self.sparse_sources
                    .borrow_mut()
                    .insert(vector_name.as_str().to_owned(), source);
                Ok(readiness)
            }
            QueryKind::LateInteraction { .. } => {
                let collection_name = CollectionName::new(self.collection_name.to_owned())?;
                let source = crate::hybrid_query::late_interaction_ann::CompositeLateInteractionSource::prepare(
                    &collection_name,
                    query,
                )?;
                let readiness = source.readiness();
                self.late_interaction.replace(Some(source));
                Ok(readiness)
            }
            QueryKind::FullText { .. }
            | QueryKind::Recommend { .. }
            | QueryKind::Discover { .. }
            | QueryKind::Lookup { .. } => Ok(SourceReadiness::Exact),
            _ => Err(QueryError::PortFailure {
                stage: "candidate_router",
                message: "composite node reached a leaf candidate adapter".to_owned(),
            }),
        }
    }

    fn candidate_limit(&mut self, query: &QueryIr, remaining: usize) -> Result<usize> {
        if let QueryKind::SparseNearest { vector_name, .. } = query.kind() {
            return self
                .sparse_sources
                .borrow()
                .get(vector_name.as_str())
                .map(|source| source.candidate_limit(query, remaining))
                .ok_or_else(|| QueryError::PortFailure {
                    stage: "sparse_candidate_source",
                    message: "sparse source was not prepared during readiness".to_owned(),
                });
        }
        if matches!(query.kind(), QueryKind::LateInteraction { .. }) {
            return self
                .late_interaction
                .borrow()
                .as_ref()
                .map(|source| source.candidate_limit(remaining))
                .ok_or_else(|| QueryError::PortFailure {
                    stage: "late_interaction_candidate_source",
                    message: "late-interaction source was not prepared during readiness".to_owned(),
                });
        }
        Ok(leaf_candidate_limit(query, self.adapter)?.min(remaining))
    }

    fn candidates(
        &mut self,
        query: &QueryIr,
        filter: Option<&FilterCandidateBatch>,
        limit: usize,
    ) -> Result<CandidatePage> {
        if matches!(query.kind(), QueryKind::Nearest { .. }) {
            let registered_vector = registered_vector_for_query(self.registered_vectors, query)?;
            return match self.adapter {
                CandidateAdapter::Exact => {
                    SpiExactCandidateSource::new(self.collection_id, registered_vector)
                        .candidates(query, filter, limit)
                        .map(|page| page.with_strategy("dense_exact"))
                }
                CandidateAdapter::Hnsw => {
                    if uses_quantized_mmap(query, registered_vector) {
                        let artifact_name = self
                            .quantized_artifacts
                            .borrow()
                            .get(&dense_vector_key(query)?)
                            .cloned()
                            .ok_or_else(|| QueryError::PortFailure {
                                stage: "quantized_hnsw_candidate_source",
                                message: "quantized artifact was not prepared during readiness"
                                    .to_owned(),
                            })?;
                        return quantized_mmap_candidates(
                            self.collection_name,
                            self.collection_id,
                            registered_vector,
                            query,
                            &artifact_name,
                            limit,
                        );
                    }
                    SpiHnswCandidateSource::new(self.collection_id, registered_vector)
                        .candidates(query, filter, limit)
                        .map(|page| page.with_strategy("dense_hnsw").with_expansion_count(1))
                }
            };
        }
        if let QueryKind::SparseNearest { vector_name, .. } = query.kind() {
            return self
                .sparse_sources
                .borrow()
                .get(vector_name.as_str())
                .ok_or_else(|| QueryError::PortFailure {
                    stage: "sparse_candidate_source",
                    message: "sparse source was not prepared during readiness".to_owned(),
                })?
                .candidates(self.collection_id, query, filter, limit);
        }
        if matches!(query.kind(), QueryKind::LateInteraction { .. }) {
            if filter.is_some() {
                return Err(QueryError::PortFailure {
                    stage: "late_interaction_candidate_source",
                    message: "late-interaction leaves do not accept filters".to_owned(),
                });
            }
            return self
                .late_interaction
                .borrow()
                .as_ref()
                .ok_or_else(|| QueryError::PortFailure {
                    stage: "late_interaction_candidate_source",
                    message: "late-interaction source was not prepared during readiness".to_owned(),
                })?
                .candidates(limit);
        }
        if filter.is_some() {
            return Err(QueryError::PortFailure {
                stage: "candidate_router",
                message: "this named source does not accept a filter batch".to_owned(),
            });
        }
        let rows = advanced_source_rows(
            self.collection_name,
            self.collection_id,
            self.source_table,
            query,
            limit,
        )?;
        let branch = match query.kind() {
            QueryKind::FullText { .. } => CandidateBranch::FullText,
            QueryKind::Recommend { .. } | QueryKind::Discover { .. } => CandidateBranch::DenseExact,
            QueryKind::Lookup { .. } => CandidateBranch::UserProvided,
            _ => unreachable!("advanced source rows only accepts executable named leaves"),
        };
        let mut cache = self.cache.borrow_mut();
        cache.clear();
        let mut candidates = Vec::with_capacity(rows.len());
        for row in rows {
            candidates.push(Candidate::new(row.point_id(), row.score(), branch)?);
            cache.insert(row.point_id(), row);
        }
        let strategy = match query.kind() {
            QueryKind::FullText { .. } => "postgres_full_text",
            QueryKind::Recommend { .. } => "exact_recommend",
            QueryKind::Discover { .. } => "exact_discover",
            QueryKind::Lookup { .. } => "exact_lookup",
            _ => unreachable!("advanced source rows only accepts executable named leaves"),
        };
        Ok(CandidatePage::new(candidates, true)
            .with_strategy(strategy)
            .with_expansion_count(0))
    }
}

struct PgRecheckerRouter<'a> {
    collection_id: i64,
    registered_vectors: &'a DenseVectorMap,
    filter_fields: &'a [FilterField],
    cache: RecheckCache,
    sparse_sources: SparseSourceCache,
    late_interaction: LateInteractionCache,
}

impl SourceRechecker for PgRecheckerRouter<'_> {
    fn recheck(
        &mut self,
        query: &QueryIr,
        candidates: &[Candidate],
        limit: usize,
    ) -> Result<Vec<HydratedCandidate>> {
        if matches!(query.kind(), QueryKind::Nearest { .. }) {
            return SpiSourceRechecker {
                collection_id: self.collection_id,
                registered_vector: registered_vector_for_query(self.registered_vectors, query)?,
                filter_fields: self.filter_fields,
            }
            .recheck(query, candidates, limit);
        }
        if let QueryKind::SparseNearest { vector_name, .. } = query.kind() {
            return self
                .sparse_sources
                .borrow()
                .get(vector_name.as_str())
                .ok_or_else(|| QueryError::PortFailure {
                    stage: "sparse_source_rechecker",
                    message: "sparse source was not prepared during readiness".to_owned(),
                })?
                .recheck(
                    self.collection_id,
                    self.filter_fields,
                    query,
                    candidates,
                    limit,
                );
        }
        if matches!(query.kind(), QueryKind::LateInteraction { .. }) {
            return self
                .late_interaction
                .borrow()
                .as_ref()
                .ok_or_else(|| QueryError::PortFailure {
                    stage: "late_interaction_source_rechecker",
                    message: "late-interaction source was not prepared during readiness".to_owned(),
                })?
                .recheck(candidates, limit);
        }
        let cache = self.cache.borrow();
        candidates
            .iter()
            .take(limit)
            .map(|candidate| {
                cache
                    .get(&candidate.point_id())
                    .cloned()
                    .ok_or(QueryError::UnexpectedPointId {
                        stage: "named_source_rechecker",
                        point_id: candidate.point_id(),
                    })
            })
            .collect()
    }
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
    source_table: &'a SourceTable,
    filter_fields: &'a [FilterField],
    adapter: CandidateAdapter,
}

impl FilterCandidateSource for SpiFilterCandidateSource<'_> {
    fn candidate_limit(&mut self, _query: &QueryIr, remaining: usize) -> Result<usize> {
        Ok(match self.adapter {
            CandidateAdapter::Exact => remaining,
            CandidateAdapter::Hnsw => crate::settings::hnsw_mask_candidate_limit_from_guc()
                .max(1)
                .min(remaining),
        })
    }

    fn filter_candidates(&mut self, query: &QueryIr, limit: usize) -> Result<FilterCandidateBatch> {
        let filter = query.filter().ok_or_else(|| QueryError::PortFailure {
            stage: "filter_candidate_source",
            message: "filter adapter called without a query filter".to_owned(),
        })?;
        let plan = resolve_typed_filter_plan(self.filter_fields, filter, 2)
            .map_err(|error| port_failure("filter_candidate_source", error))?;
        let table_name = quote_qualified_identifier(
            &self.source_table.schema_name,
            &self.source_table.table_name,
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
    let adapter = effective_candidate_adapter(&query, adapter);
    let collection = resolve_collection(collection_name);
    require_collection_owner(&collection, collection_name);
    let source_table = resolve_source_table(collection.collection_id)
        .unwrap_or_else(|error| raise_query_error(error));
    require_source_table_select_privilege(&source_table);
    let registered_vectors =
        resolve_query_vectors(collection_name, collection.collection_id, &query);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        collection_name,
        query.limit(),
    );
    let projected_candidate_limit =
        projected_candidate_limit(&query, adapter).unwrap_or_else(|error| raise_query_error(error));
    if adapter == CandidateAdapter::Hnsw {
        crate::collection_limits::enforce_candidate_budget(
            collection.collection_id,
            collection_name,
            projected_candidate_limit,
        );
    }
    let filter_fields = if query.has_filter_in_subtree() {
        load_filter_fields(collection.collection_id)
    } else {
        Vec::new()
    };

    let mut telemetry = PgTelemetrySink;
    let outcome = execute_prepared_query_with_vectors(
        collection_name.as_str(),
        collection.collection_id,
        &registered_vectors,
        &source_table,
        &filter_fields,
        &query,
        adapter,
        &mut telemetry,
    )
    .unwrap_or_else(|error| raise_query_error(error));
    require_complete_outcome(&outcome);
    outcome_rows(&outcome).unwrap_or_else(|error| raise_query_error(error))
}

#[cfg(feature = "pg_test")]
fn execute_prepared_query(
    collection_name: &str,
    collection_id: i64,
    registered_vector: &SearchVector,
    filter_fields: &[FilterField],
    query: &QueryIr,
    adapter: CandidateAdapter,
    telemetry: &mut PgTelemetrySink,
) -> Result<ExecutionOutcome> {
    let registered_vectors = BTreeMap::from([(None, registered_vector.clone())]);
    let source_table = SourceTable::from(registered_vector);
    execute_prepared_query_with_vectors(
        collection_name,
        collection_id,
        &registered_vectors,
        &source_table,
        filter_fields,
        query,
        adapter,
        telemetry,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the PostgreSQL adapter composition requires resolved catalog and port inputs"
)]
fn execute_prepared_query_with_vectors(
    collection_name: &str,
    collection_id: i64,
    registered_vectors: &DenseVectorMap,
    source_table: &SourceTable,
    filter_fields: &[FilterField],
    query: &QueryIr,
    adapter: CandidateAdapter,
    telemetry: &mut PgTelemetrySink,
) -> Result<ExecutionOutcome> {
    let adapter = effective_candidate_adapter(query, adapter);
    let candidate_limit = projected_candidate_limit(query, adapter)?;
    let filter_candidate_limit = projected_filter_candidate_limit(query, adapter)?;
    let budget = ExecutionBudget::new(
        candidate_limit,
        filter_candidate_limit,
        candidate_limit,
        context_core::policy::MAX_QUERY_STAGES,
        context_core::policy::MAX_QUERY_EXPANSIONS,
        query.max_node_limit(),
    )?;
    let cache = Rc::new(RefCell::new(BTreeMap::new()));
    let sparse_sources = Rc::new(RefCell::new(BTreeMap::new()));
    let late_interaction = Rc::new(RefCell::new(None));
    let quantized_artifacts = Rc::new(RefCell::new(BTreeMap::new()));
    let mut candidates = PgCandidateRouter {
        collection_name,
        collection_id,
        registered_vectors,
        source_table,
        adapter,
        cache: Rc::clone(&cache),
        sparse_sources: Rc::clone(&sparse_sources),
        late_interaction: Rc::clone(&late_interaction),
        quantized_artifacts,
    };
    let mut filter = SpiFilterCandidateSource {
        collection_id,
        source_table,
        filter_fields,
        adapter,
    };
    let filter_port = query
        .has_filter_in_subtree()
        .then_some(&mut filter as &mut dyn FilterCandidateSource);
    let mut rechecker = PgRecheckerRouter {
        collection_id,
        registered_vectors,
        filter_fields,
        cache,
        sparse_sources,
        late_interaction,
    };
    let cancellation = PgCancellation;
    QueryExecutor::new(
        &mut candidates,
        filter_port,
        &mut rechecker,
        telemetry,
        &cancellation,
    )
    .execute(query, budget)
}

fn resolve_source_table(collection_id: i64) -> Result<SourceTable> {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT source_class.oid,
                        collections.source_schema_name,
                        collections.source_table_name
                   FROM pgcontext._visible_collections AS collections
                   JOIN pg_catalog.pg_namespace AS source_namespace
                     ON source_namespace.nspname = collections.source_schema_name
                   JOIN pg_catalog.pg_class AS source_class
                     ON source_class.relnamespace = source_namespace.oid
                    AND source_class.relname = collections.source_table_name
                    AND source_class.relkind IN ('r', 'p')
                  WHERE collections.collection_id = $1",
                Some(1),
                &[collection_id.into()],
            )
            .map_err(|error| port_failure("source_table_resolver", error))?;
        let Some(row) = rows.into_iter().next() else {
            return Err(QueryError::PortFailure {
                stage: "source_table_resolver",
                message: "collection source table is unavailable or has drifted".to_owned(),
            });
        };
        Ok(SourceTable {
            table_oid: spi_column::<pg_sys::Oid>(&row, 1, "source_table_resolver")?,
            schema_name: spi_column::<String>(&row, 2, "source_table_resolver")?,
            table_name: spi_column::<String>(&row, 3, "source_table_resolver")?,
        })
    })
}

fn require_source_table_select_privilege(source_table: &SourceTable) {
    let has_select = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.has_table_privilege(SESSION_USER, $1, 'SELECT')",
        &[source_table.table_oid.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check source table privileges: {error}"),
        )
    })
    .unwrap_or(false);
    if !has_select {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!(
                "permission denied for source table: {}.{}",
                source_table.schema_name, source_table.table_name
            ),
        );
    }
}

fn resolve_query_vectors(
    collection_name: &CollectionName,
    collection_id: i64,
    query: &QueryIr,
) -> DenseVectorMap {
    let mut names = BTreeSet::new();
    collect_dense_vector_names(query, &mut names);
    if names.is_empty() {
        return BTreeMap::new();
    }

    names
        .into_iter()
        .map(|name| {
            let mut registered_vector = match name.as_deref() {
                Some(name) => {
                    let vector_name = context_core::VectorName::new(name.to_owned())
                        .unwrap_or_else(|error| crate::error::raise_core_error(error));
                    resolve_registered_vector_by_name(collection_name, collection_id, &vector_name)
                }
                None => resolve_registered_vector(collection_name, collection_id),
            };
            validate_search_drift(collection_id, &mut registered_vector);
            require_table_select_privilege(&registered_vector);
            (name, registered_vector)
        })
        .collect()
}

fn collect_dense_vector_names(query: &QueryIr, names: &mut BTreeSet<Option<String>>) {
    match query.kind() {
        QueryKind::Nearest { vector_name, .. } => {
            names.insert(
                vector_name
                    .as_ref()
                    .map(|vector_name| vector_name.as_str().to_owned()),
            );
        }
        QueryKind::Prefetch { branches } => {
            for branch in branches {
                collect_dense_vector_names(branch, names);
            }
        }
        QueryKind::Weighted { query, .. }
        | QueryKind::ScoreThreshold { query, .. }
        | QueryKind::Formula { query, .. }
        | QueryKind::Rerank { query } => collect_dense_vector_names(query, names),
        QueryKind::SparseNearest { .. }
        | QueryKind::FullText { .. }
        | QueryKind::LateInteraction { .. }
        | QueryKind::Recommend { .. }
        | QueryKind::Discover { .. }
        | QueryKind::Lookup { .. } => {}
    }
}

fn registered_vector_for_query<'a>(
    registered_vectors: &'a DenseVectorMap,
    query: &QueryIr,
) -> Result<&'a SearchVector> {
    let key = dense_vector_key(query)?;
    registered_vectors
        .get(&key)
        .ok_or_else(|| QueryError::PortFailure {
            stage: "dense_vector_router",
            message: "resolved dense vector binding is unavailable".to_owned(),
        })
}

fn dense_vector_key(query: &QueryIr) -> Result<Option<String>> {
    let QueryKind::Nearest { vector_name, .. } = query.kind() else {
        return Err(QueryError::PortFailure {
            stage: "dense_vector_router",
            message: "dense vector routing requires a nearest query".to_owned(),
        });
    };
    Ok(vector_name
        .as_ref()
        .map(|vector_name| vector_name.as_str().to_owned()))
}

fn vector_uses_quantization(registered_vector: &SearchVector) -> bool {
    registered_vector
        .quantization_options
        .as_object()
        .is_some_and(|options| !options.is_empty())
}

fn uses_quantized_mmap(query: &QueryIr, registered_vector: &SearchVector) -> bool {
    vector_uses_quantization(registered_vector)
        && query.filter().is_none()
        && dense_vector_key(query).is_ok_and(|key| key.is_none())
}

fn resolve_quantized_artifact(collection_id: i64) -> Result<Option<String>> {
    let names = Spi::connect(|client| {
        client
            .select(
                "SELECT artifacts.artifact_name
                   FROM pgcontext._visible_artifact_segments AS artifacts
                  WHERE artifacts.collection_id = $1
                    AND artifacts.artifact_kind = 'mmap'
                    AND artifacts.segment_kind = 'hnsw_graph'
                    AND artifacts.lifecycle_state = 'file_materialized'
                    AND artifacts.config_revision =
                        pgcontext.current_vector_config_revision($1)
                  ORDER BY artifacts.generation DESC, artifacts.artifact_id DESC",
                None,
                &[collection_id.into()],
            )
            .map_err(|error| port_failure("quantized_hnsw_readiness", error))?
            .map(|row| spi_column::<String>(&row, 1, "quantized_hnsw_readiness"))
            .collect::<Result<Vec<_>>>()
    })?;
    let distinct = names.into_iter().collect::<BTreeSet<_>>();
    if distinct.len() > 1 {
        return Err(QueryError::PortFailure {
            stage: "quantized_hnsw_readiness",
            message: "multiple serving-ready mapped artifacts match the quantized vector"
                .to_owned(),
        });
    }
    Ok(distinct.into_iter().next())
}

fn quantized_mmap_candidates(
    collection_name: &str,
    collection_id: i64,
    registered_vector: &SearchVector,
    query: &QueryIr,
    artifact_name: &str,
    limit: usize,
) -> Result<CandidatePage> {
    let query_vector = Vector::from_dense(nearest_vector(query)?.clone());
    let candidate_limit = SearchLimit::new(limit).map_err(QueryError::from)?;
    let result_limit = SearchLimit::new(query.limit()).map_err(QueryError::from)?;
    let max_mapped_bytes =
        i64::try_from(crate::settings::hnsw_mmap_serving_budget_bytes_from_guc()).map_err(
            |_| QueryError::PortFailure {
                stage: "quantized_hnsw_candidate_source",
                message: "mapped serving byte budget exceeds PostgreSQL bigint".to_owned(),
            },
        )?;
    let (generation_high_water, mut rows) = load_mmap_artifact_candidates(
        collection_name,
        artifact_name,
        &query_vector,
        max_mapped_bytes,
        candidate_limit,
        result_limit,
    );
    rows.extend(mmap_delta_candidates(
        collection_id,
        registered_vector,
        &query_vector,
        generation_high_water,
        limit,
    ));
    rows.sort_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    rows.dedup_by_key(|(point_id, _)| *point_id);
    rows.truncate(limit);
    let scored_count = rows.len();
    let candidates = rows
        .into_iter()
        .map(|(point_id, score)| {
            Candidate::new(
                PointId::from_i64(point_id).ok_or_else(|| QueryError::PortFailure {
                    stage: "quantized_hnsw_candidate_source",
                    message: format!("invalid PostgreSQL point ID {point_id}"),
                })?,
                f64::from(score),
                CandidateBranch::DenseAnn,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(
        CandidatePage::with_scored_count(candidates, scored_count, true)
            .with_strategy("quantized_mmap_hnsw")
            .with_expansion_count(1),
    )
}

fn advanced_source_rows(
    collection_name: &str,
    collection_id: i64,
    source_table: &SourceTable,
    query: &QueryIr,
    limit: usize,
) -> Result<Vec<HydratedCandidate>> {
    let sql_limit = i32::try_from(limit).map_err(|_| QueryError::PortFailure {
        stage: "named_candidate_source",
        message: format!("candidate limit {limit} exceeds PostgreSQL integer"),
    })?;
    let rows = match query.kind() {
        QueryKind::FullText { text_column, query } => {
            full_text_source_rows(collection_id, source_table, text_column, query, limit)?
        }
        QueryKind::Recommend { positive, negative } => {
            crate::table_search::recommend::recommend_collection_from_points(
                collection_name.to_owned(),
                sql_point_ids(positive.iter().copied())?,
                sql_point_ids(negative.iter().copied())?,
                sql_limit,
            )
            .map(|(point_id, source_key, score)| (point_id, source_key, f64::from(score)))
            .collect()
        }
        QueryKind::Discover { context } => crate::table_search::recommend::discover_collection(
            collection_name.to_owned(),
            sql_point_ids(context.iter().copied())?,
            sql_limit,
        )
        .map(|(point_id, source_key, score)| (point_id, source_key, f64::from(score)))
        .collect(),
        QueryKind::Lookup { point_ids } => lookup_source_rows(collection_id, point_ids, limit)?,
        _ => {
            return Err(QueryError::PortFailure {
                stage: "named_candidate_source",
                message: "query kind has no named PostgreSQL adapter".to_owned(),
            });
        }
    };
    rows.into_iter()
        .map(|(point_id, source_key, score)| {
            HydratedCandidate::new(
                PointId::from_i64(point_id).ok_or_else(|| QueryError::PortFailure {
                    stage: "named_candidate_source",
                    message: format!("invalid PostgreSQL point ID {point_id}"),
                })?,
                SourceKey::new(source_key)?,
                score,
            )
        })
        .collect()
}

fn full_text_source_rows(
    collection_id: i64,
    source_table: &SourceTable,
    text_column: &str,
    text_query: &str,
    limit: usize,
) -> Result<Vec<(i64, String, f64)>> {
    let table_name =
        quote_qualified_identifier(&source_table.schema_name, &source_table.table_name);
    let text_column = quote_identifier(text_column);
    let sql_limit = sql_limit(limit, "full_text_candidate_source")?;
    let sql = format!(
        "WITH query AS (SELECT pg_catalog.plainto_tsquery('simple', $2) AS tsquery)
         SELECT points.point_id,
                points.source_key,
                pg_catalog.ts_rank_cd(
                    pg_catalog.to_tsvector('simple', coalesce(source.{text_column}::text, '')),
                    query.tsquery
                )::double precision
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
           CROSS JOIN query
          WHERE points.collection_id = $1
            AND points.deleted_at IS NULL
            AND pg_catalog.to_tsvector('simple', coalesce(source.{text_column}::text, '')) @@ query.tsquery
          ORDER BY 3 DESC, points.point_id ASC
          LIMIT $3"
    );
    Spi::connect(|client| {
        client
            .select(
                &sql,
                Some(sql_limit),
                &[collection_id.into(), text_query.into(), sql_limit.into()],
            )
            .map_err(|error| port_failure("full_text_candidate_source", error))?
            .map(|row| {
                Ok((
                    spi_column::<i64>(&row, 1, "full_text_candidate_source")?,
                    spi_column::<String>(&row, 2, "full_text_candidate_source")?,
                    spi_column::<f64>(&row, 3, "full_text_candidate_source")?,
                ))
            })
            .collect()
    })
}

fn lookup_source_rows(
    collection_id: i64,
    point_ids: &[PointId],
    limit: usize,
) -> Result<Vec<(i64, String, f64)>> {
    let sql_ids = sql_point_ids(point_ids.iter().copied())?;
    let rows = Spi::connect(|client| {
        client
            .select(
                "SELECT point_id, source_key
                   FROM pgcontext._visible_collection_points
                  WHERE collection_id = $1
                    AND deleted_at IS NULL
                    AND point_id = ANY($2::bigint[])",
                None,
                &[collection_id.into(), sql_ids.into()],
            )
            .map_err(|error| port_failure("lookup_candidate_source", error))?
            .map(|row| {
                Ok((
                    spi_column::<i64>(&row, 1, "lookup_candidate_source")?,
                    spi_column::<String>(&row, 2, "lookup_candidate_source")?,
                ))
            })
            .collect::<Result<BTreeMap<_, _>>>()
    })?;
    point_ids
        .iter()
        .take(limit)
        .enumerate()
        .filter_map(|(position, point_id)| {
            let point_id = i64::try_from(point_id.get()).ok()?;
            let source_key = rows.get(&point_id)?.clone();
            let position = u32::try_from(position).ok()?;
            Some(Ok((point_id, source_key, -f64::from(position))))
        })
        .collect()
}

fn effective_candidate_adapter(query: &QueryIr, adapter: CandidateAdapter) -> CandidateAdapter {
    if adapter == CandidateAdapter::Hnsw
        && query.has_filter_in_subtree()
        && crate::settings::hnsw_mask_candidate_limit_from_guc() == 0
    {
        CandidateAdapter::Exact
    } else {
        adapter
    }
}

fn projected_candidate_limit(query: &QueryIr, adapter: CandidateAdapter) -> Result<usize> {
    match query.kind() {
        QueryKind::Prefetch { branches } => branches.iter().try_fold(0_usize, |total, branch| {
            total
                .checked_add(projected_candidate_limit(branch, adapter)?)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "composite_candidate_projection",
                })
        }),
        QueryKind::Weighted { query, .. }
        | QueryKind::ScoreThreshold { query, .. }
        | QueryKind::Formula { query, .. }
        | QueryKind::Rerank { query } => projected_candidate_limit(query, adapter),
        _ => leaf_candidate_limit(query, adapter),
    }
}

fn projected_filter_candidate_limit(query: &QueryIr, adapter: CandidateAdapter) -> Result<usize> {
    if adapter == CandidateAdapter::Exact {
        return Ok(context_core::policy::MAX_HNSW_CANDIDATE_MASK_POINTS);
    }
    let per_leaf = crate::settings::hnsw_mask_candidate_limit_from_guc();
    let filtered_leaves = filtered_leaf_count(query);
    let projected = per_leaf
        .checked_mul(filtered_leaves)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "composite_filter_candidate_projection",
        })?
        .max(1);
    if projected > context_core::policy::MAX_HNSW_CANDIDATE_MASK_POINTS {
        return Err(QueryError::WorkBudgetExceeded {
            budget: "filter_candidates",
            actual: projected,
            maximum: context_core::policy::MAX_HNSW_CANDIDATE_MASK_POINTS,
        });
    }
    Ok(projected)
}

fn filtered_leaf_count(query: &QueryIr) -> usize {
    match query.kind() {
        QueryKind::Prefetch { branches } => branches.iter().map(filtered_leaf_count).sum(),
        QueryKind::Weighted { query, .. }
        | QueryKind::ScoreThreshold { query, .. }
        | QueryKind::Formula { query, .. }
        | QueryKind::Rerank { query } => filtered_leaf_count(query),
        _ => usize::from(query.filter().is_some()),
    }
}

fn leaf_candidate_limit(query: &QueryIr, adapter: CandidateAdapter) -> Result<usize> {
    let limit = match query.kind() {
        QueryKind::Nearest { .. } | QueryKind::SparseNearest { .. }
            if adapter == CandidateAdapter::Hnsw =>
        {
            crate::settings::hnsw_candidate_budget_from_guc().max(query.limit().saturating_add(1))
        }
        QueryKind::LateInteraction {
            vectors,
            candidates_per_query,
        } => vectors
            .len()
            .checked_mul(candidates_per_query.get())
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "late_interaction_candidate_projection",
            })?,
        _ => query.limit(),
    };
    if limit > context_core::policy::MAX_RECALL_CHECK_POINT_IDS {
        return Err(QueryError::WorkBudgetExceeded {
            budget: "candidates",
            actual: limit,
            maximum: context_core::policy::MAX_RECALL_CHECK_POINT_IDS,
        });
    }
    Ok(limit)
}

fn require_complete_outcome(outcome: &ExecutionOutcome) {
    match outcome.state() {
        ExecutionState::Ready => {}
        ExecutionState::RebuildRequired { reason } => raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!("query source requires rebuild: {reason:?}"),
        ),
        ExecutionState::NotReady { reason } => raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            format!("query source is not ready: {reason:?}"),
        ),
    }
    match outcome.completion() {
        Completion::Complete => {}
        Completion::Cancelled => raise_sql_error(
            PgSqlErrorCode::ERRCODE_QUERY_CANCELED,
            "query execution was cancelled",
        ),
        Completion::BudgetExhausted => raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "query execution exhausted its work budget",
        ),
    }
}

fn candidate_rows(
    collection_id: i64,
    registered_vector: &SearchVector,
    query: &QueryIr,
    filter: Option<&FilterCandidateBatch>,
    limit: usize,
    adapter: CandidateAdapter,
) -> Result<CandidatePage> {
    match adapter {
        CandidateAdapter::Exact => {
            exact_candidate_rows(collection_id, registered_vector, query, filter, limit)
        }
        CandidateAdapter::Hnsw => {
            hnsw_candidate_rows(collection_id, registered_vector, query, filter, limit)
        }
    }
}

fn exact_candidate_rows(
    collection_id: i64,
    registered_vector: &SearchVector,
    query: &QueryIr,
    filter: Option<&FilterCandidateBatch>,
    limit: usize,
) -> Result<CandidatePage> {
    let query_vector = sql_vector(query)?;
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let vector_column = quote_identifier(&registered_vector.vector_column_name);
    let score_expression = format!(
        "pgcontext.{}(source.{vector_column}, $1)",
        distance_function(registered_vector.metric)
    );
    let sql_limit = sql_limit(limit, "candidate_source")?;
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
        let mut candidates = Vec::new();
        for row in rows {
            let point_id = spi_point_id(&row, 1, "candidate_source")?;
            let score = f64::from(spi_column::<f32>(&row, 2, "candidate_source")?);
            candidates.push(Candidate::new(
                point_id,
                score,
                CandidateBranch::DenseExact,
            )?);
        }
        Ok(CandidatePage::new(candidates, true))
    })
}

fn hnsw_candidate_rows(
    collection_id: i64,
    registered_vector: &SearchVector,
    query: &QueryIr,
    filter: Option<&FilterCandidateBatch>,
    limit: usize,
) -> Result<CandidatePage> {
    let query_vector = sql_vector(query)?;
    let index_oid = registered_vector
        .hnsw_index_oid
        .ok_or_else(|| QueryError::PortFailure {
            stage: "candidate_source",
            message: "registered vector has no attached HNSW index".to_owned(),
        })?;
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let sql_limit = sql_limit(limit, "candidate_source")?;
    let hnsw_limit = i32::try_from(limit).map_err(|_| QueryError::PortFailure {
        stage: "candidate_source",
        message: format!("HNSW candidate limit {limit} exceeds PostgreSQL integer"),
    })?;
    let point_ids = filter
        .map(|filter| sql_point_ids(filter.point_ids().iter().copied()))
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
                  CROSS JOIN LATERAL pgcontext._hnsw_masked_candidates(
                        $5,
                        $1,
                        candidate_mask.heap_tids,
                        $4
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
            "WITH ann_candidates AS MATERIALIZED (
                 SELECT ann.heap_tid, ann.score::float8 AS score
                   FROM pgcontext._hnsw_candidates($4, $1, $3) AS ann
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
    let mut args = Vec::<DatumWithOid<'_>>::with_capacity(5);
    args.push(query_vector.into());
    args.push(collection_id.into());
    if let Some(point_ids) = point_ids {
        args.push(point_ids.into());
    }
    args.push(hnsw_limit.into());
    args.push(index_oid.into());

    crate::hnsw_am::with_hnsw_candidate_helper_capability(index_oid, || {
        Spi::connect(|client| {
            let rows = client
                .select(&sql, Some(sql_limit), &args)
                .map_err(|error| port_failure("candidate_source", error))?;
            let mut candidates = Vec::new();
            for row in rows {
                let point_id = spi_point_id(&row, 1, "candidate_source")?;
                let score = spi_column::<f64>(&row, 2, "candidate_source")?;
                candidates.push(Candidate::new(point_id, score, CandidateBranch::DenseAnn)?);
            }
            Ok(CandidatePage::new(candidates, true))
        })
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
        collection_name.as_str(),
        collection.collection_id,
        &registered_vector,
        &[],
        &query,
        CandidateAdapter::Exact,
        &mut PgTelemetrySink::default(),
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
    pub(crate) exact_complete: bool,
    pub(crate) hnsw_complete: bool,
    pub(crate) hnsw_work_candidates: i64,
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
        collection_name.as_str(),
        collection.collection_id,
        &registered_vector,
        &filter_fields,
        &query,
        CandidateAdapter::Exact,
        &mut PgTelemetrySink::default(),
    )
    .unwrap_or_else(|error| raise_query_error(error));
    let hnsw = execute_prepared_query(
        collection_name.as_str(),
        collection.collection_id,
        &registered_vector,
        &filter_fields,
        &query,
        CandidateAdapter::Hnsw,
        &mut PgTelemetrySink::default(),
    )
    .unwrap_or_else(|error| raise_query_error(error));
    let exact_usage = exact.usage();
    let hnsw_usage = hnsw.usage();
    let hnsw_work_candidates =
        Spi::get_one::<i64>("SELECT candidates FROM pgcontext.hnsw_last_scan_work()")
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to read HNSW adapter work: {error}"),
                )
            })
            .unwrap_or_default();
    AdapterConformanceSnapshot {
        exact_rows: outcome_rows(&exact).unwrap_or_else(|error| raise_query_error(error)),
        hnsw_rows: outcome_rows(&hnsw).unwrap_or_else(|error| raise_query_error(error)),
        filter_candidates: exact_usage.filter_candidates(),
        exact_candidates: exact_usage.candidates(),
        hnsw_candidates: hnsw_usage.candidates(),
        exact_rechecks: exact_usage.rechecks(),
        hnsw_rechecks: hnsw_usage.rechecks(),
        exact_complete: exact.state() == &ExecutionState::Ready
            && exact.completion() == Completion::Complete,
        hnsw_complete: hnsw.state() == &ExecutionState::Ready
            && hnsw.completion() == Completion::Complete,
        hnsw_work_candidates,
    }
}

#[cfg(feature = "pg_test")]
pub(crate) fn dense_metric_adapter_snapshot_for_test(
    collection: String,
) -> AdapterConformanceSnapshot {
    let collection_name = crate::table_search::collection_name_from_sql(collection);
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector(&collection_name, collection.collection_id);
    validate_search_drift(collection.collection_id, &mut registered_vector);
    require_table_select_privilege(&registered_vector);
    let query = QueryIr::nearest(
        None,
        vec![1.0, 0.0],
        context_query::ScoreOrder::LowerIsBetter,
        None,
        3,
    )
    .unwrap_or_else(|error| raise_query_error(error));
    let exact = execute_prepared_query(
        collection_name.as_str(),
        collection.collection_id,
        &registered_vector,
        &[],
        &query,
        CandidateAdapter::Exact,
        &mut PgTelemetrySink::default(),
    )
    .unwrap_or_else(|error| raise_query_error(error));
    let hnsw = execute_prepared_query(
        collection_name.as_str(),
        collection.collection_id,
        &registered_vector,
        &[],
        &query,
        CandidateAdapter::Hnsw,
        &mut PgTelemetrySink::default(),
    )
    .unwrap_or_else(|error| raise_query_error(error));
    let exact_usage = exact.usage();
    let hnsw_usage = hnsw.usage();
    let hnsw_work_candidates =
        Spi::get_one::<i64>("SELECT candidates FROM pgcontext.hnsw_last_scan_work()")
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to read HNSW metric adapter work: {error}"),
                )
            })
            .unwrap_or_default();
    AdapterConformanceSnapshot {
        exact_rows: outcome_rows(&exact).unwrap_or_else(|error| raise_query_error(error)),
        hnsw_rows: outcome_rows(&hnsw).unwrap_or_else(|error| raise_query_error(error)),
        filter_candidates: 0,
        exact_candidates: exact_usage.candidates(),
        hnsw_candidates: hnsw_usage.candidates(),
        exact_rechecks: exact_usage.rechecks(),
        hnsw_rechecks: hnsw_usage.rechecks(),
        exact_complete: exact.state() == &ExecutionState::Ready
            && exact.completion() == Completion::Complete,
        hnsw_complete: hnsw.state() == &ExecutionState::Ready
            && hnsw.completion() == Completion::Complete,
        hnsw_work_candidates,
    }
}

#[cfg(feature = "pg_test")]
pub(crate) fn run_hnsw_for_test(collection: String) -> Vec<(i64, String, f32)> {
    let collection_name = crate::table_search::collection_name_from_sql(collection);
    let query = QueryIr::nearest(
        None,
        vec![1.0, 0.0],
        context_query::ScoreOrder::LowerIsBetter,
        None,
        1,
    )
    .unwrap_or_else(|error| raise_query_error(error));
    run_query(&collection_name, query, CandidateAdapter::Hnsw)
}
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
