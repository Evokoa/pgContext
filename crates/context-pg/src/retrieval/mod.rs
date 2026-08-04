//! PostgreSQL adapters for the transport-neutral query executor.

mod lexical;
mod sparse;
pub(crate) use lexical::{LexicalStrategy, lexical_headline_rows};
pub(crate) use sparse::{SparseCandidateStrategy, run_sparse_query};

use context_core::{
    CollectionName, ConfigurationRevision, DenseVector, GenerationId, OccurrenceId, PointId,
    ScoreOrder, SearchLimit, SourceAuthority, SourceKey,
};
use context_index::HnswComparisonBudget;
use context_query::{
    Cancellation, Candidate, CandidateBranch, CandidateDiagnostics, CandidatePage,
    CandidateProvenance, CandidateSource, CandidateSourceKind, Completion, ExecutionBudget,
    ExecutionOutcome, ExecutionState, FilterCandidateBatch, FilterCandidateSource,
    HydratedCandidate, PortBudget, QueryClock, QueryError, QueryExecutor, QueryIr, QueryKind,
    RecheckPage, Result, SourceReadiness, SourceRechecker, StageDiagnostic, TelemetrySink,
};
use core::mem::size_of;
use pgrx::datum::DatumWithOid;
use pgrx::prelude::*;
use std::time::Instant;

use crate::error::{raise_query_error, raise_sql_error};
use crate::table_search::{
    FilterField, SearchVector, distance_function, load_filter_fields,
    load_mmap_artifact_candidates_with_runtime_budget,
    mmap_delta_candidates_with_comparison_budget, push_filter_parameter_args, quote_identifier,
    quote_qualified_identifier, require_collection_owner, require_table_select_privilege,
    resolve_collection, resolve_registered_vector, resolve_registered_vector_by_name,
    resolve_typed_filter_plan, take_last_mmap_candidate_visits, take_last_mmap_delta_visits,
    validate_search_drift,
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

pub(super) fn candidate_provenance(
    point_id: PointId,
    branch: CandidateBranch,
    source: CandidateSourceKind,
    score_order: ScoreOrder,
    authority: SourceAuthority,
) -> Result<CandidateProvenance> {
    let occurrence_id = candidate_occurrence_id(point_id, branch, source, None, None)?;
    Ok(CandidateProvenance::new(
        occurrence_id,
        branch,
        source,
        score_order,
        authority,
    ))
}

fn artifact_candidate_provenance(
    point_id: PointId,
    branch: CandidateBranch,
    source: CandidateSourceKind,
    score_order: ScoreOrder,
    authority: SourceAuthority,
    generation: GenerationId,
    configuration: ConfigurationRevision,
) -> Result<CandidateProvenance> {
    let occurrence_id = candidate_occurrence_id(
        point_id,
        branch,
        source,
        Some(generation),
        Some(configuration),
    )?;
    Ok(
        CandidateProvenance::new(occurrence_id, branch, source, score_order, authority)
            .with_generation(generation)
            .with_configuration(configuration),
    )
}

fn candidate_occurrence_id(
    point_id: PointId,
    branch: CandidateBranch,
    source: CandidateSourceKind,
    generation: Option<GenerationId>,
    configuration: Option<ConfigurationRevision>,
) -> Result<OccurrenceId> {
    // FNV-1a is deliberately fixed here: occurrence IDs must be reproducible
    // across processes and must change when any source identity component does.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for bytes in [
        point_id.get().to_le_bytes(),
        u64::from(branch.stable_code()).to_le_bytes(),
        u64::from(source.stable_code()).to_le_bytes(),
        generation.map_or(0, GenerationId::get).to_le_bytes(),
        configuration
            .map_or(0, ConfigurationRevision::get)
            .to_le_bytes(),
    ] {
        for byte in bytes {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    OccurrenceId::new(hash).ok_or_else(|| QueryError::PortFailure {
        stage: "candidate_provenance",
        message: "candidate occurrence hash resolved to the reserved zero value".to_owned(),
    })
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
    fn readiness(&mut self, query: &QueryIr, _budget: PortBudget) -> Result<SourceReadiness> {
        nearest_vector(query)?;
        Ok(SourceReadiness::Ready)
    }

    fn candidates(
        &mut self,
        query: &QueryIr,
        filter: Option<&FilterCandidateBatch>,
        limit: usize,
        budget: PortBudget,
    ) -> Result<CandidatePage> {
        let filter_ids = filter.map_or(0, |batch| batch.point_ids().len());
        let query_bytes = dense_vector_copy_bytes(
            nearest_vector(query)?.dimension(),
            1,
            "exact_query_vector_memory_projection",
        )?;
        require_port_memory_bytes(
            candidate_response_peak_bytes(limit, filter_ids, query_bytes)?,
            budget,
            "candidate_memory",
        )?;
        exact_candidate_rows(
            self.collection_id,
            self.registered_vector,
            query,
            filter,
            limit,
            budget.max_comparisons(),
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
type LexicalSourceCache = Rc<RefCell<BTreeMap<String, lexical::CompositeLexicalSource>>>;
type FuzzySourceCache = Rc<RefCell<BTreeMap<String, lexical::CompositeFuzzySource>>>;
type QuantizedArtifactCache = Rc<RefCell<BTreeMap<Option<String>, QuantizedArtifactIdentity>>>;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct QuantizedArtifactIdentity {
    name: String,
    generation: GenerationId,
    configuration: ConfigurationRevision,
}

enum QuantizedArtifactResolution {
    Ready(QuantizedArtifactIdentity),
    RebuildRequired,
    Missing,
}

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
    lexical_sources: LexicalSourceCache,
    fuzzy_sources: FuzzySourceCache,
    quantized_artifacts: QuantizedArtifactCache,
}

impl CandidateSource for PgCandidateRouter<'_> {
    fn readiness(&mut self, query: &QueryIr, budget: PortBudget) -> Result<SourceReadiness> {
        match query.kind() {
            QueryKind::Nearest { .. } => match self.adapter {
                CandidateAdapter::Exact => SpiExactCandidateSource::new(
                    self.collection_id,
                    registered_vector_for_query(self.registered_vectors, query)?,
                )
                .readiness(query, budget),
                CandidateAdapter::Hnsw => {
                    let registered_vector =
                        registered_vector_for_query(self.registered_vectors, query)?;
                    if uses_quantized_mmap(query, registered_vector) {
                        let artifact = match resolve_quantized_artifact(self.collection_id)? {
                            QuantizedArtifactResolution::Ready(artifact) => artifact,
                            QuantizedArtifactResolution::RebuildRequired => {
                                return Ok(SourceReadiness::RebuildRequired {
                                    reason: context_query::ReadinessReason::ConfigurationChanged,
                                });
                            }
                            QuantizedArtifactResolution::Missing => {
                                return Ok(SourceReadiness::NotReady {
                                    reason: context_query::ReadinessReason::GenerationMissing,
                                });
                            }
                        };
                        self.quantized_artifacts
                            .borrow_mut()
                            .insert(dense_vector_key(query)?, artifact);
                        Ok(SourceReadiness::Ready)
                    } else {
                        SpiHnswCandidateSource::new(self.collection_id, registered_vector)
                            .readiness(query, budget)
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
                    budget,
                )?;
                let readiness = source.readiness();
                self.late_interaction.replace(Some(source));
                Ok(readiness)
            }
            QueryKind::Lexical { source, .. } => {
                let prepared = lexical::CompositeLexicalSource::prepare(self.collection_id, query)?;
                let readiness = prepared.readiness();
                self.lexical_sources
                    .borrow_mut()
                    .insert(source.as_str().to_owned(), prepared);
                Ok(readiness)
            }
            QueryKind::Fuzzy { source, .. } => {
                let prepared = lexical::CompositeFuzzySource::prepare(self.collection_id, query)?;
                let readiness = prepared.readiness();
                self.fuzzy_sources
                    .borrow_mut()
                    .insert(source.as_str().to_owned(), prepared);
                Ok(readiness)
            }
            QueryKind::Recommend { .. } | QueryKind::Discover { .. } | QueryKind::Lookup { .. } => {
                Ok(SourceReadiness::Exact)
            }
            _ => Err(QueryError::PortFailure {
                stage: "candidate_router",
                message: "composite node reached a leaf candidate adapter".to_owned(),
            }),
        }
    }

    fn candidate_limit(
        &mut self,
        query: &QueryIr,
        remaining: usize,
        _budget: PortBudget,
    ) -> Result<usize> {
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
        if let QueryKind::Lexical { source, .. } = query.kind() {
            return self
                .lexical_sources
                .borrow()
                .get(source.as_str())
                .map(|prepared| prepared.candidate_limit(query, remaining))
                .ok_or_else(|| lexical_not_prepared("lexical_candidate_source"));
        }
        if let QueryKind::Fuzzy { source, .. } = query.kind() {
            return self
                .fuzzy_sources
                .borrow()
                .get(source.as_str())
                .map(|prepared| prepared.candidate_limit(query, remaining))
                .ok_or_else(|| lexical_not_prepared("fuzzy_candidate_source"));
        }
        Ok(leaf_candidate_limit(query, self.adapter)?.min(remaining))
    }

    fn candidates(
        &mut self,
        query: &QueryIr,
        filter: Option<&FilterCandidateBatch>,
        limit: usize,
        budget: PortBudget,
    ) -> Result<CandidatePage> {
        if matches!(query.kind(), QueryKind::Nearest { .. }) {
            let registered_vector = registered_vector_for_query(self.registered_vectors, query)?;
            return match self.adapter {
                CandidateAdapter::Exact => {
                    SpiExactCandidateSource::new(self.collection_id, registered_vector)
                        .candidates(query, filter, limit, budget)
                        .map(|page| page.with_strategy("dense_exact"))
                }
                CandidateAdapter::Hnsw => {
                    if uses_quantized_mmap(query, registered_vector) {
                        let artifact = self
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
                            &artifact,
                            limit,
                            budget,
                        );
                    }
                    SpiHnswCandidateSource::new(self.collection_id, registered_vector)
                        .candidates(query, filter, limit, budget)
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
                .candidates(self.collection_id, query, filter, limit, budget);
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
                .candidates(limit, budget);
        }
        if let QueryKind::Lexical { source, .. } = query.kind() {
            return self
                .lexical_sources
                .borrow()
                .get(source.as_str())
                .ok_or_else(|| lexical_not_prepared("lexical_candidate_source"))?
                .candidates(query, limit, budget);
        }
        if let QueryKind::Fuzzy { source, .. } = query.kind() {
            return self
                .fuzzy_sources
                .borrow()
                .get(source.as_str())
                .ok_or_else(|| lexical_not_prepared("fuzzy_candidate_source"))?
                .candidates(query, limit, budget);
        }
        if filter.is_some() {
            return Err(QueryError::PortFailure {
                stage: "candidate_router",
                message: "this named source does not accept a filter batch".to_owned(),
            });
        }
        let (rows, scored_count) = advanced_source_rows(
            self.collection_name,
            self.collection_id,
            self.source_table,
            query,
            limit,
            budget,
        )?;
        let branch = match query.kind() {
            QueryKind::Recommend { .. } => CandidateBranch::Recommend,
            QueryKind::Discover { .. } => CandidateBranch::Discover,
            QueryKind::Lookup { .. } => CandidateBranch::Lookup,
            _ => unreachable!("advanced source rows only accepts executable named leaves"),
        };
        let mut cache = self.cache.borrow_mut();
        cache.clear();
        let mut candidates = Vec::with_capacity(rows.len());
        for (rank, row) in rows.into_iter().enumerate() {
            let source = match branch {
                CandidateBranch::Recommend => CandidateSourceKind::Recommendation,
                CandidateBranch::Discover => CandidateSourceKind::Discovery,
                CandidateBranch::Lookup => CandidateSourceKind::Lookup,
                CandidateBranch::DenseAnn
                | CandidateBranch::DenseExact
                | CandidateBranch::Lexical
                | CandidateBranch::Fuzzy
                | CandidateBranch::Sparse
                | CandidateBranch::MultiVector
                | CandidateBranch::Quantized
                | CandidateBranch::Topology
                | CandidateBranch::UserProvided => unreachable!("advanced branch is validated"),
            };
            let candidate = Candidate::new(
                row.point_id(),
                row.score(),
                candidate_provenance(
                    row.point_id(),
                    branch,
                    source,
                    query.score_order(),
                    SourceAuthority::PostgreSqlRow,
                )?,
            )?
            .with_exact_score(row.score())?
            .with_diagnostics(CandidateDiagnostics::new(
                u32::try_from(rank).unwrap_or(u32::MAX),
                1,
            ));
            candidates.push(candidate);
            cache.insert(row.point_id(), row);
        }
        let strategy = match query.kind() {
            QueryKind::Recommend { .. } => "exact_recommend",
            QueryKind::Discover { .. } => "exact_discover",
            QueryKind::Lookup { .. } => "exact_lookup",
            _ => unreachable!("advanced source rows only accepts executable named leaves"),
        };
        Ok(
            CandidatePage::with_scored_count(candidates, scored_count, true)
                .with_strategy(strategy)
                .with_expansion_count(0),
        )
    }
}

struct PgRecheckerRouter<'a> {
    collection_id: i64,
    registered_vectors: &'a DenseVectorMap,
    filter_fields: &'a [FilterField],
    cache: RecheckCache,
    sparse_sources: SparseSourceCache,
    late_interaction: LateInteractionCache,
    lexical_sources: LexicalSourceCache,
    fuzzy_sources: FuzzySourceCache,
}

impl SourceRechecker for PgRecheckerRouter<'_> {
    fn recheck(
        &mut self,
        query: &QueryIr,
        candidates: &[Candidate],
        limit: usize,
        budget: PortBudget,
    ) -> Result<RecheckPage> {
        if matches!(query.kind(), QueryKind::Nearest { .. }) {
            return SpiSourceRechecker {
                collection_id: self.collection_id,
                registered_vector: registered_vector_for_query(self.registered_vectors, query)?,
                filter_fields: self.filter_fields,
            }
            .recheck(query, candidates, limit, budget);
        }
        if let QueryKind::SparseNearest { vector_name, .. } = query.kind() {
            let rows = self
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
                    budget,
                )?;
            return Ok(RecheckPage::new(rows, candidates.len()));
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
                .recheck(candidates, limit, budget);
        }
        if let QueryKind::Lexical { source, .. } = query.kind() {
            require_port_hydration(
                candidates.len().min(limit),
                budget,
                "lexical_recheck_hydration",
            )?;
            let rows = self
                .lexical_sources
                .borrow()
                .get(source.as_str())
                .ok_or_else(|| lexical_not_prepared("lexical_source_recheck"))?
                .recheck(query, candidates, limit)?;
            return Ok(RecheckPage::new(rows, candidates.len()));
        }
        if let QueryKind::Fuzzy { source, .. } = query.kind() {
            require_port_hydration(
                candidates.len().min(limit),
                budget,
                "fuzzy_recheck_hydration",
            )?;
            let rows = self
                .fuzzy_sources
                .borrow()
                .get(source.as_str())
                .ok_or_else(|| lexical_not_prepared("fuzzy_source_recheck"))?
                .recheck(query, candidates, limit)?;
            return Ok(RecheckPage::new(rows, candidates.len()));
        }
        let output_count = candidates.len().min(limit);
        let cache_entries = self.cache.borrow().len();
        require_port_memory_bytes(
            named_source_recheck_peak_bytes(cache_entries, output_count)?,
            budget,
            "named_source_recheck_memory",
        )?;
        require_port_hydration(output_count, budget, "named_source_recheck_hydration")?;
        {
            let cache = self.cache.borrow();
            for candidate in candidates.iter().take(limit) {
                if !cache.contains_key(&candidate.point_id()) {
                    return Err(QueryError::UnexpectedPointId {
                        stage: "named_source_rechecker",
                        point_id: candidate.point_id(),
                    });
                }
            }
        }
        let mut cache = self.cache.borrow_mut();
        let mut rows = Vec::with_capacity(output_count);
        for candidate in candidates.iter().take(limit) {
            rows.push(cache.remove(&candidate.point_id()).ok_or(
                QueryError::UnexpectedPointId {
                    stage: "named_source_rechecker",
                    point_id: candidate.point_id(),
                },
            )?);
        }
        cache.clear();
        Ok(RecheckPage::new(rows, 0))
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
    fn readiness(&mut self, query: &QueryIr, _budget: PortBudget) -> Result<SourceReadiness> {
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
        budget: PortBudget,
    ) -> Result<CandidatePage> {
        let filter_ids = filter.map_or(0, |batch| batch.point_ids().len());
        let query_bytes = dense_vector_copy_bytes(
            nearest_vector(query)?.dimension(),
            4,
            "hnsw_query_vector_memory_projection",
        )?;
        let retained = candidate_response_peak_bytes(limit, filter_ids, query_bytes)?;
        let traversal_memory = budget.max_memory_bytes().checked_sub(retained).ok_or(
            QueryError::WorkBudgetExceeded {
                budget: "candidate_memory",
                actual: retained,
                maximum: budget.max_memory_bytes(),
            },
        )?;
        hnsw_candidate_rows(
            self.collection_id,
            self.registered_vector,
            query,
            filter,
            limit,
            budget.max_comparisons(),
            traversal_memory,
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
        budget: PortBudget,
    ) -> Result<RecheckPage> {
        let output_count = candidates.len().min(limit);
        require_port_memory_bytes(
            source_recheck_peak_bytes(
                candidates.len(),
                limit,
                dense_vector_copy_bytes(
                    nearest_vector(query)?.dimension(),
                    1,
                    "source_recheck_query_vector_memory_projection",
                )?,
            )?,
            budget,
            "source_recheck_memory",
        )?;
        require_port_hydration(output_count, budget, "source_recheck_hydration")?;
        if candidates.len() > budget.max_comparisons() {
            return Err(QueryError::WorkBudgetExceeded {
                budget: "source_recheck_comparisons",
                actual: candidates.len(),
                maximum: budget.max_comparisons(),
            });
        }
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
            let mut output = Vec::with_capacity(output_count);
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
            Ok(RecheckPage::new(output, candidates.len()))
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
    fn candidate_limit(
        &mut self,
        _query: &QueryIr,
        remaining: usize,
        _budget: PortBudget,
    ) -> Result<usize> {
        Ok(match self.adapter {
            CandidateAdapter::Exact => remaining,
            CandidateAdapter::Hnsw => crate::settings::hnsw_mask_candidate_limit_from_guc()
                .max(1)
                .min(remaining),
        })
    }

    fn filter_candidates(
        &mut self,
        query: &QueryIr,
        limit: usize,
        budget: PortBudget,
    ) -> Result<FilterCandidateBatch> {
        let probe_limit = limit.checked_add(1).ok_or(QueryError::ArithmeticOverflow {
            operation: "filter_candidate_probe_limit",
        })?;
        let probe_count = filter_candidate_probe_count(probe_limit, budget.max_comparisons());
        require_port_memory_for::<PointId>(probe_count, budget, "filter_candidate_memory")?;
        let filter = query.filter().ok_or_else(|| QueryError::PortFailure {
            stage: "filter_candidate_source",
            message: "filter adapter called without a query filter".to_owned(),
        })?;
        let plan = resolve_typed_filter_plan(self.filter_fields, filter, 4)
            .map_err(|error| port_failure("filter_candidate_source", error))?;
        let table_name = quote_qualified_identifier(
            &self.source_table.schema_name,
            &self.source_table.table_name,
        );
        let sql_result_limit = sql_limit(probe_limit, "filter_candidate_source")?;
        let comparison_probe = budget.max_comparisons().saturating_add(1);
        let sql_comparison_probe = sql_limit(comparison_probe, "filter_candidate_source")?;
        let sql_max_comparisons = sql_limit(budget.max_comparisons(), "filter_candidate_source")?;
        let sql = format!(
            "WITH visible AS MATERIALIZED (
                 SELECT points.point_id, source.ctid AS heap_tid
                   FROM pgcontext._visible_collection_points AS points
                   JOIN {table_name} AS source ON source.id::text = points.source_key
                  WHERE points.collection_id = $1
                    AND points.deleted_at IS NULL
                  LIMIT $3
             ),
             admission AS (
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
        let mut args = Vec::<DatumWithOid<'_>>::with_capacity(4 + plan.parameters.len());
        args.push(self.collection_id.into());
        args.push(sql_result_limit.into());
        args.push(sql_comparison_probe.into());
        args.push(sql_max_comparisons.into());
        push_filter_parameter_args(&mut args, &plan.parameters);

        Spi::connect(|client| {
            let rows = client
                .select(&sql, Some(sql_result_limit), &args)
                .map_err(|error| port_failure("filter_candidate_source", error))?;
            let mut point_ids = Vec::with_capacity(probe_count);
            let mut evaluated_count = 0_usize;
            for row in rows {
                evaluated_count =
                    usize::try_from(spi_column::<i64>(&row, 2, "filter_candidate_source")?)
                        .map_err(|_| QueryError::PortFailure {
                            stage: "filter_candidate_source",
                            message: "visible filter row count exceeds usize".to_owned(),
                        })?;
                if let Some(point_id) =
                    spi_optional_result_column::<i64>(&row, 1, "filter_candidate_source")?
                {
                    point_ids.push(PointId::from_i64(point_id).ok_or_else(|| {
                        QueryError::PortFailure {
                            stage: "filter_candidate_source",
                            message: format!("negative PostgreSQL point ID: {point_id}"),
                        }
                    })?);
                }
            }
            if evaluated_count > budget.max_comparisons() {
                return Err(QueryError::WorkBudgetExceeded {
                    budget: "filter_comparisons",
                    actual: evaluated_count,
                    maximum: budget.max_comparisons(),
                });
            }
            let exhausted = point_ids.len() <= limit;
            point_ids.truncate(limit);
            Ok(FilterCandidateBatch::new(
                point_ids,
                evaluated_count,
                exhausted,
            ))
        })
    }
}

#[derive(Default)]
pub(crate) struct PgTelemetrySink {
    diagnostics: Vec<StageDiagnostic>,
}

impl TelemetrySink for PgTelemetrySink {
    fn record(&mut self, diagnostic: &StageDiagnostic) -> Result<()> {
        crate::query_stats_async::record(diagnostic);
        self.diagnostics.push(diagnostic.clone());
        Ok(())
    }
}

/// PostgreSQL cooperative cancellation bridge.
pub(crate) struct PgCancellation;

struct PgQueryClock {
    started: Instant,
}

impl PgQueryClock {
    fn start() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl QueryClock for PgQueryClock {
    fn now_micros(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX)
    }
}

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
    let observation = std::sync::Mutex::new(None);
    PgTryBuilder::new(|| run_query_inner(collection_name, query, adapter, &observation))
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

fn run_query_inner(
    collection_name: &CollectionName,
    query: QueryIr,
    adapter: CandidateAdapter,
    observation: &std::sync::Mutex<Option<crate::query_stats_async::ObservationToken>>,
) -> Vec<(i64, String, f32)> {
    let requested_adapter = adapter;
    let adapter = effective_candidate_adapter(&query, adapter);
    let used_fallback =
        requested_adapter == CandidateAdapter::Hnsw && adapter == CandidateAdapter::Exact;
    let collection = resolve_collection(collection_name);
    require_collection_owner(&collection, collection_name);
    let max_elapsed_micros = crate::collection_limits::query_timeout_micros(
        collection.collection_id,
        context_query::DEFAULT_QUERY_ELAPSED_MICROS,
    );
    let clock = PgQueryClock::start();
    let timeout = current_statement_timeout::Guard::arm(max_elapsed_micros)
        .unwrap_or_else(|error| raise_query_error(error));
    #[cfg(feature = "pg_test")]
    run_query_preparation_delay_probe();
    *observation
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        crate::query_stats::begin_automatic_query_stat(
            collection.collection_id,
            &query,
            used_fallback,
        );
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

    let mut telemetry = PgTelemetrySink::default();
    let outcome = execute_prepared_query_with_vectors_and_deadline(
        collection_name.as_str(),
        collection.collection_id,
        &registered_vectors,
        &source_table,
        &filter_fields,
        &query,
        adapter,
        &mut telemetry,
        &clock,
        max_elapsed_micros,
    );
    crate::query_stats::record_automatic_query_stat(
        *observation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        &telemetry.diagnostics,
        outcome.as_ref(),
        used_fallback,
    );
    let outcome = outcome.unwrap_or_else(|error| raise_query_error(error));
    require_complete_outcome(&outcome);
    let rows = outcome_rows(&outcome).unwrap_or_else(|error| raise_query_error(error));
    timeout.restore();
    rows
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
#[cfg(feature = "pg_test")]
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
    let max_elapsed_micros = crate::collection_limits::query_timeout_micros(
        collection_id,
        context_query::DEFAULT_QUERY_ELAPSED_MICROS,
    );
    let clock = PgQueryClock::start();
    let timeout = current_statement_timeout::Guard::arm(max_elapsed_micros)?;
    let outcome = execute_prepared_query_with_vectors_and_deadline(
        collection_name,
        collection_id,
        registered_vectors,
        source_table,
        filter_fields,
        query,
        adapter,
        telemetry,
        &clock,
        max_elapsed_micros,
    );
    timeout.restore();
    outcome
}

#[allow(
    clippy::too_many_arguments,
    reason = "the PostgreSQL adapter composition requires resolved catalog, port, and deadline inputs"
)]
fn execute_prepared_query_with_vectors_and_deadline(
    collection_name: &str,
    collection_id: i64,
    registered_vectors: &DenseVectorMap,
    source_table: &SourceTable,
    filter_fields: &[FilterField],
    query: &QueryIr,
    adapter: CandidateAdapter,
    telemetry: &mut PgTelemetrySink,
    clock: &PgQueryClock,
    max_elapsed_micros: u64,
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
    )?
    .with_resource_limits(
        context_query::DEFAULT_QUERY_COMPARISONS,
        context_query::DEFAULT_QUERY_MEMORY_BYTES,
        context_query::DEFAULT_QUERY_HYDRATION_BYTES,
        max_elapsed_micros,
    )?;
    let cache = Rc::new(RefCell::new(BTreeMap::new()));
    let sparse_sources = Rc::new(RefCell::new(BTreeMap::new()));
    let late_interaction = Rc::new(RefCell::new(None));
    let lexical_sources = Rc::new(RefCell::new(BTreeMap::new()));
    let fuzzy_sources = Rc::new(RefCell::new(BTreeMap::new()));
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
        lexical_sources: Rc::clone(&lexical_sources),
        fuzzy_sources: Rc::clone(&fuzzy_sources),
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
        lexical_sources,
        fuzzy_sources,
    };
    let cancellation = PgCancellation;
    QueryExecutor::new(
        &mut candidates,
        filter_port,
        &mut rechecker,
        telemetry,
        &cancellation,
    )
    .with_clock(clock)
    .execute(query, budget)
}

#[cfg(feature = "pg_test")]
static QUERY_PREPARATION_DELAY_MICROS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[cfg(feature = "pg_test")]
fn run_query_preparation_delay_probe() {
    use std::sync::atomic::Ordering;

    let delay_micros = QUERY_PREPARATION_DELAY_MICROS.swap(0, Ordering::SeqCst);
    if delay_micros == 0 {
        return;
    }
    let delay_seconds = delay_micros as f64 / 1_000_000.0;
    Spi::run(&format!("SELECT pg_catalog.pg_sleep({delay_seconds})"))
        .expect("query preparation delay probe should run");
}

#[cfg(feature = "pg_test")]
pub(crate) fn delay_next_query_preparation_for_test(delay_micros: u64) {
    use std::sync::atomic::Ordering;

    QUERY_PREPARATION_DELAY_MICROS.store(delay_micros, Ordering::SeqCst);
}

#[allow(
    unsafe_code,
    reason = "the bounded query timer calls PostgreSQL's registered current-backend timeout API with fixed STATEMENT_TIMEOUT identity and value-only timestamps"
)]
mod current_statement_timeout {
    use core::ffi::c_int;

    use context_query::{QueryError, Result};
    use pgrx::pg_sys;

    const STATEMENT_TIMEOUT_ID: c_int = 3;

    unsafe extern "C" {
        fn enable_timeout_at(id: c_int, fin_time: pg_sys::TimestampTz);
        fn disable_timeout(id: c_int, keep_indicator: bool);
        fn get_timeout_active(id: c_int) -> bool;
        fn get_timeout_finish_time(id: c_int) -> pg_sys::TimestampTz;
    }

    pub(super) struct Guard {
        original_finish: Option<pg_sys::TimestampTz>,
        armed: bool,
    }

    impl Guard {
        pub(super) fn arm(timeout_micros: u64) -> Result<Self> {
            let delay_micros =
                i64::try_from(timeout_micros).map_err(|_| QueryError::ArithmeticOverflow {
                    operation: "query_timeout_deadline",
                })?;
            // SAFETY: PostgreSQL registers STATEMENT_TIMEOUT (enum value 3 in
            // supported PG17/18) during backend initialization. These calls
            // operate only on this backend's timeout state and copy scalar
            // timestamps; no pointer or borrowed Datum crosses the boundary.
            unsafe {
                let original_finish = get_timeout_active(STATEMENT_TIMEOUT_ID)
                    .then(|| get_timeout_finish_time(STATEMENT_TIMEOUT_ID));
                let requested_finish = pg_sys::GetCurrentTimestamp()
                    .checked_add(delay_micros)
                    .ok_or(QueryError::ArithmeticOverflow {
                        operation: "query_timeout_deadline",
                    })?;
                let finish = original_finish
                    .map_or(requested_finish, |original| original.min(requested_finish));
                if original_finish.is_some() {
                    disable_timeout(STATEMENT_TIMEOUT_ID, false);
                }
                enable_timeout_at(STATEMENT_TIMEOUT_ID, finish);
                Ok(Self {
                    original_finish,
                    armed: true,
                })
            }
        }

        pub(super) fn restore(mut self) {
            self.disarm(true);
        }

        fn disarm(&mut self, restore_original: bool) {
            if !self.armed {
                return;
            }
            // SAFETY: this guard exclusively replaces the backend's statement
            // timeout for its lexical query section. The saved finish time is
            // an owned scalar from the same backend and is restored only on a
            // successful path; PostgreSQL command cleanup owns error paths.
            unsafe {
                disable_timeout(STATEMENT_TIMEOUT_ID, false);
                if restore_original && let Some(original_finish) = self.original_finish {
                    enable_timeout_at(STATEMENT_TIMEOUT_ID, original_finish);
                }
            }
            self.armed = false;
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            self.disarm(false);
        }
    }
}

pub(crate) fn with_candidate_comparison_budget<T>(
    projected: usize,
    budget: PortBudget,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    with_candidate_comparison_limit(projected, budget.max_comparisons(), operation)
}

pub(crate) fn require_port_memory_for<T>(
    count: usize,
    budget: PortBudget,
    budget_name: &'static str,
) -> Result<()> {
    let actual = count
        .checked_mul(size_of::<T>())
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "port_response_memory_projection",
        })?;
    require_port_memory_bytes(actual, budget, budget_name)
}

pub(crate) fn require_port_memory_bytes(
    actual: usize,
    budget: PortBudget,
    budget_name: &'static str,
) -> Result<()> {
    require_memory_bytes_limit(actual, budget.max_memory_bytes(), budget_name)
}

fn require_memory_bytes_limit(
    actual: usize,
    maximum: usize,
    budget_name: &'static str,
) -> Result<()> {
    if actual > maximum {
        return Err(QueryError::WorkBudgetExceeded {
            budget: budget_name,
            actual,
            maximum,
        });
    }
    Ok(())
}

pub(crate) fn require_port_hydration(
    count: usize,
    budget: PortBudget,
    budget_name: &'static str,
) -> Result<()> {
    let actual = count
        .checked_mul(context_core::policy::MAX_SOURCE_KEY_BYTES)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "port_hydration_projection",
        })?;
    if actual > budget.max_hydration_bytes() {
        return Err(QueryError::WorkBudgetExceeded {
            budget: budget_name,
            actual,
            maximum: budget.max_hydration_bytes(),
        });
    }
    Ok(())
}

fn with_candidate_comparison_limit<T>(
    projected: usize,
    maximum: usize,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if projected > maximum {
        return Err(QueryError::WorkBudgetExceeded {
            budget: "candidate_comparisons",
            actual: projected,
            maximum,
        });
    }
    operation()
}

fn filter_candidate_probe_count(probe_limit: usize, maximum_comparisons: usize) -> usize {
    probe_limit.min(maximum_comparisons)
}

fn checked_vec_bytes<T>(count: usize, operation: &'static str) -> Result<usize> {
    count
        .checked_mul(size_of::<T>())
        .ok_or(QueryError::ArithmeticOverflow { operation })
}

fn checked_memory_sum(
    parts: impl IntoIterator<Item = usize>,
    operation: &'static str,
) -> Result<usize> {
    parts.into_iter().try_fold(0_usize, |total, part| {
        total
            .checked_add(part)
            .ok_or(QueryError::ArithmeticOverflow { operation })
    })
}

fn dense_vector_copy_bytes(
    dimensions: usize,
    copies: usize,
    operation: &'static str,
) -> Result<usize> {
    dimensions
        .checked_mul(size_of::<f32>())
        .and_then(|bytes| bytes.checked_mul(copies))
        .ok_or(QueryError::ArithmeticOverflow { operation })
}

fn maximum_source_key_heap_bytes(count: usize, operation: &'static str) -> Result<usize> {
    count
        .checked_mul(context_core::policy::MAX_SOURCE_KEY_BYTES)
        // A PostgreSQL-owned string is copied into a Rust `String`. Charge the
        // allocator's possible geometric slack as well as the logical bytes.
        .and_then(|bytes| bytes.checked_mul(2))
        .ok_or(QueryError::ArithmeticOverflow { operation })
}

/// Conservatively projects the allocation retained by a `BTreeMap`.
///
/// Rust's B-tree nodes reserve several key/value slots at once. The projection
/// charges one completely empty node plus one under-filled node per six
/// entries, including twelve child pointers and four bookkeeping words per
/// node. This is intentionally above the current standard-library layout and
/// avoids treating `len * size_of::<(K, V)>()` as the map's allocation.
fn conservative_btree_bytes<K, V>(entries: usize, operation: &'static str) -> Result<usize> {
    if entries == 0 {
        return Ok(0);
    }
    let node_count = entries
        .checked_add(5)
        .ok_or(QueryError::ArithmeticOverflow { operation })?
        / 6;
    let node_payload = 11_usize
        .checked_mul(
            size_of::<K>()
                .checked_add(size_of::<V>())
                .ok_or(QueryError::ArithmeticOverflow { operation })?,
        )
        .and_then(|bytes| bytes.checked_add(size_of::<[usize; 16]>()));
    node_count
        .checked_add(1)
        .and_then(|nodes| node_payload.and_then(|bytes| nodes.checked_mul(bytes)))
        .ok_or(QueryError::ArithmeticOverflow { operation })
}

fn candidate_response_peak_bytes(
    limit: usize,
    filter_ids: usize,
    query_vector_bytes: usize,
) -> Result<usize> {
    checked_memory_sum(
        [
            checked_vec_bytes::<Candidate>(limit, "candidate_response_memory_projection")?,
            checked_vec_bytes::<i64>(filter_ids, "candidate_filter_id_memory_projection")?,
            query_vector_bytes,
        ],
        "candidate_response_peak_memory_projection",
    )
}

fn source_recheck_peak_bytes(
    candidate_count: usize,
    limit: usize,
    query_vector_bytes: usize,
) -> Result<usize> {
    let output_count = candidate_count.min(limit);
    checked_memory_sum(
        [
            checked_vec_bytes::<i64>(candidate_count, "source_recheck_id_memory_projection")?,
            checked_vec_bytes::<HydratedCandidate>(
                output_count,
                "source_recheck_output_memory_projection",
            )?,
            maximum_source_key_heap_bytes(output_count, "source_recheck_key_memory_projection")?,
            query_vector_bytes,
        ],
        "source_recheck_peak_memory_projection",
    )
}

fn named_source_peak_bytes(limit: usize) -> Result<usize> {
    let key_bytes = maximum_source_key_heap_bytes(limit, "named_source_key_memory_projection")?;
    let sql_tuple_bytes =
        checked_vec_bytes::<(i64, String, f64)>(limit, "named_source_sql_tuple_memory_projection")?;
    let full_text_spi_tuple_bytes =
        checked_vec_bytes::<(Option<i64>, Option<String>, Option<f64>, i64)>(
            limit,
            "named_source_spi_tuple_memory_projection",
        )?;
    let recommendation_tuple_bytes = checked_vec_bytes::<(i64, String, f32)>(
        limit,
        "named_source_recommendation_tuple_memory_projection",
    )?
    .checked_mul(2)
    .ok_or(QueryError::ArithmeticOverflow {
        operation: "named_source_recommendation_capacity_projection",
    })?;
    let provider_peak = checked_memory_sum(
        [
            full_text_spi_tuple_bytes.max(recommendation_tuple_bytes),
            sql_tuple_bytes,
            key_bytes,
        ],
        "named_source_provider_peak_memory_projection",
    )?;

    let downstream_peak = checked_memory_sum(
        [
            checked_vec_bytes::<HydratedCandidate>(
                limit,
                "named_source_hydrated_memory_projection",
            )?,
            checked_vec_bytes::<Candidate>(limit, "named_source_candidate_memory_projection")?,
            conservative_btree_bytes::<PointId, HydratedCandidate>(
                limit,
                "named_source_cache_memory_projection",
            )?,
            key_bytes,
        ],
        "named_source_downstream_peak_memory_projection",
    )?;

    // Lookup also retains its request and SQL-ID arrays while the source tuple
    // page is materialized. Other providers' preparation checks are performed
    // inside their adapters before their larger vector work begins.
    let lookup_peak = checked_memory_sum(
        [
            checked_vec_bytes::<PointId>(limit, "lookup_request_memory_projection")?,
            checked_vec_bytes::<i64>(limit, "lookup_sql_id_memory_projection")?,
            sql_tuple_bytes,
            key_bytes,
        ],
        "lookup_peak_memory_projection",
    )?;

    Ok(provider_peak.max(downstream_peak).max(lookup_peak))
}

fn named_source_recheck_peak_bytes(cache_entries: usize, output_count: usize) -> Result<usize> {
    let retained_count = cache_entries.max(output_count);
    checked_memory_sum(
        [
            conservative_btree_bytes::<PointId, HydratedCandidate>(
                cache_entries,
                "named_source_recheck_cache_memory_projection",
            )?,
            checked_vec_bytes::<HydratedCandidate>(
                output_count,
                "named_source_recheck_output_memory_projection",
            )?,
            maximum_source_key_heap_bytes(
                retained_count,
                "named_source_recheck_key_memory_projection",
            )?,
        ],
        "named_source_recheck_peak_memory_projection",
    )
}

fn recommendation_preparation_memory_limit(limit: usize, budget: PortBudget) -> Result<usize> {
    let retained_output = checked_memory_sum(
        [
            checked_vec_bytes::<(i64, String, f32)>(
                limit,
                "recommendation_output_memory_projection",
            )?
            .checked_mul(2)
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "recommendation_output_capacity_projection",
            })?,
            maximum_source_key_heap_bytes(limit, "recommendation_output_key_memory_projection")?,
        ],
        "recommendation_output_peak_memory_projection",
    )?;
    budget
        .max_memory_bytes()
        .checked_sub(retained_output)
        .ok_or(QueryError::WorkBudgetExceeded {
            budget: "recommendation_memory",
            actual: retained_output,
            maximum: budget.max_memory_bytes(),
        })
}

#[cfg(test)]
mod candidate_budget_tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn over_budget_candidate_operation_is_not_executed() {
        let executed = Cell::new(false);

        let result = with_candidate_comparison_limit(2, 1, || {
            executed.set(true);
            Ok(())
        });

        assert!(matches!(
            result,
            Err(QueryError::WorkBudgetExceeded {
                budget: "candidate_comparisons",
                actual: 2,
                maximum: 1,
            })
        ));
        assert!(!executed.get());
    }

    #[test]
    fn filter_candidate_memory_includes_the_partial_page_probe_row() {
        assert_eq!(filter_candidate_probe_count(9, 20), 9);
        assert_eq!(filter_candidate_probe_count(9, 8), 8);
        assert_eq!(
            filter_candidate_probe_count(9, 20) * size_of::<PointId>(),
            9 * size_of::<PointId>()
        );
    }

    #[test]
    fn named_source_peak_includes_retained_cache_candidates_and_key_storage() {
        let limit = 4;
        let projected = named_source_peak_bytes(limit).expect("projection should fit usize");
        let flat_dtos = limit
            * (size_of::<Candidate>()
                + size_of::<HydratedCandidate>()
                + context_core::policy::MAX_SOURCE_KEY_BYTES);

        assert!(projected > flat_dtos);
        assert!(require_memory_bytes_limit(projected, projected, "named_source_memory").is_ok());
        assert!(matches!(
            require_memory_bytes_limit(projected, projected - 1, "named_source_memory"),
            Err(QueryError::WorkBudgetExceeded {
                budget: "named_source_memory",
                actual,
                maximum,
            }) if actual == projected && maximum == projected - 1
        ));
    }

    #[test]
    fn source_recheck_peak_retains_sql_ids_while_building_hydrated_rows() {
        let candidates = 7;
        let limit = 3;
        let projected =
            source_recheck_peak_bytes(candidates, limit, 0).expect("projection should fit usize");
        let expected = candidates * size_of::<i64>()
            + limit * size_of::<HydratedCandidate>()
            + 2 * limit * context_core::policy::MAX_SOURCE_KEY_BYTES;

        assert_eq!(projected, expected);
        assert!(require_memory_bytes_limit(projected, projected, "source_recheck_memory").is_ok());
    }

    #[test]
    fn candidate_peak_accounts_for_filter_id_array_at_the_exact_boundary() {
        let projected =
            candidate_response_peak_bytes(5, 3, 0).expect("projection should fit usize");

        assert_eq!(projected, 5 * size_of::<Candidate>() + 3 * size_of::<i64>());
        assert!(require_memory_bytes_limit(projected, projected, "candidate_memory").is_ok());
        assert!(require_memory_bytes_limit(projected, projected - 1, "candidate_memory").is_err());
    }

    #[test]
    fn quantized_post_traversal_projection_is_cumulative_for_base_and_delta() {
        let limit = 64;
        let projected = quantized_post_traversal_peak_bytes(limit, 0)
            .expect("bounded projection should not overflow");
        let tuple_bytes = limit * size_of::<(i64, f32)>();
        let candidate_bytes = limit * size_of::<Candidate>();

        assert!(projected >= tuple_bytes * 3);
        assert!(projected >= tuple_bytes * 2 + candidate_bytes);
    }

    #[test]
    fn maximum_dense_query_copy_is_admitted_only_at_its_exact_boundary() {
        let dimensions = context_core::policy::MAX_VECTOR_DIMENSIONS;
        let query_bytes =
            dense_vector_copy_bytes(dimensions, 4, "maximum_dense_query_memory_projection")
                .expect("policy dimensions should fit usize");
        let projected = candidate_response_peak_bytes(1, 0, query_bytes)
            .expect("maximum-dimensional projection should fit usize");

        assert!(require_memory_bytes_limit(projected, projected, "candidate_memory").is_ok());
        assert!(require_memory_bytes_limit(projected, projected - 1, "candidate_memory").is_err());
    }
}

fn lexical_not_prepared(stage: &'static str) -> QueryError {
    QueryError::PortFailure {
        stage,
        message: "registered lexical source was not prepared during readiness".to_owned(),
    }
}

fn resolve_source_table(collection_id: i64) -> Result<SourceTable> {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT source_class.oid,
                        collections.source_schema_name,
                        collections.source_table_name
                   FROM pgcontext._visible_collections AS collections
                   LEFT JOIN pg_catalog.pg_namespace AS source_namespace
                     ON source_namespace.nspname = collections.source_schema_name
                   LEFT JOIN pg_catalog.pg_class AS source_class
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
        let schema_name = spi_column::<String>(&row, 2, "source_table_resolver")?;
        let table_name = spi_column::<String>(&row, 3, "source_table_resolver")?;
        let table_oid = row
            .get::<pg_sys::Oid>(1)
            .map_err(|error| port_failure("source_table_resolver", error))?
            .unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE,
                    format!("registered source table drifted: {schema_name}.{table_name}"),
                )
            });
        Ok(SourceTable {
            table_oid,
            schema_name,
            table_name,
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
        QueryKind::Prefetch { branches, .. } => {
            for branch in branches {
                collect_dense_vector_names(branch, names);
            }
        }
        QueryKind::Weighted { query, .. }
        | QueryKind::ScoreThreshold { query, .. }
        | QueryKind::Formula { query, .. }
        | QueryKind::Rerank { query }
        | QueryKind::ExternalRerank { query, .. }
        | QueryKind::TopologyExpand { query, .. } => collect_dense_vector_names(query, names),
        QueryKind::SparseNearest { .. }
        | QueryKind::Lexical { .. }
        | QueryKind::Fuzzy { .. }
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

fn resolve_quantized_artifact(collection_id: i64) -> Result<QuantizedArtifactResolution> {
    let rows = Spi::connect(|client| {
        client
            .select(
                "SELECT artifacts.artifact_name,
                        artifacts.lifecycle_state,
                        artifacts.config_revision =
                            pgcontext.current_vector_config_revision($1) AS config_matches,
                        artifacts.generation,
                        artifacts.config_revision
                   FROM pgcontext._visible_artifact_segments AS artifacts
                  WHERE artifacts.collection_id = $1
                    AND artifacts.artifact_kind = 'mmap'
                    AND artifacts.segment_kind = 'hnsw_graph'
                  ORDER BY artifacts.generation DESC, artifacts.artifact_id DESC",
                None,
                &[collection_id.into()],
            )
            .map_err(|error| port_failure("quantized_hnsw_readiness", error))?
            .map(|row| {
                Ok((
                    spi_column::<String>(&row, 1, "quantized_hnsw_readiness")?,
                    spi_column::<String>(&row, 2, "quantized_hnsw_readiness")?,
                    row.get::<bool>(3)
                        .map_err(|error| port_failure("quantized_hnsw_readiness", error))?
                        .unwrap_or(false),
                    spi_column::<i64>(&row, 4, "quantized_hnsw_readiness")?,
                    spi_column::<i64>(&row, 5, "quantized_hnsw_readiness")?,
                ))
            })
            .collect::<Result<Vec<_>>>()
    })?;
    let distinct = rows
        .iter()
        .filter(|(_, lifecycle, config_matches, _, _)| {
            lifecycle == "file_materialized" && *config_matches
        })
        .map(|(name, _, _, generation, configuration)| {
            let generation = u64::try_from(*generation)
                .ok()
                .and_then(GenerationId::new)
                .ok_or_else(|| QueryError::PortFailure {
                    stage: "quantized_hnsw_readiness",
                    message: format!("invalid artifact generation {generation}"),
                })?;
            let configuration = u64::try_from(*configuration)
                .ok()
                .and_then(ConfigurationRevision::new)
                .ok_or_else(|| QueryError::PortFailure {
                    stage: "quantized_hnsw_readiness",
                    message: format!("invalid artifact configuration revision {configuration}"),
                })?;
            Ok(QuantizedArtifactIdentity {
                name: name.clone(),
                generation,
                configuration,
            })
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if distinct.len() > 1 {
        return Err(QueryError::PortFailure {
            stage: "quantized_hnsw_readiness",
            message: "multiple serving-ready mapped artifacts match the quantized vector"
                .to_owned(),
        });
    }
    if let Some(identity) = distinct.into_iter().next() {
        return Ok(QuantizedArtifactResolution::Ready(identity));
    }
    if rows.iter().any(|(_, lifecycle, config_matches, _, _)| {
        lifecycle == "rebuild_required" || (lifecycle == "file_materialized" && !*config_matches)
    }) {
        return Ok(QuantizedArtifactResolution::RebuildRequired);
    }
    Ok(QuantizedArtifactResolution::Missing)
}

fn quantized_mmap_candidates(
    collection_name: &str,
    collection_id: i64,
    registered_vector: &SearchVector,
    query: &QueryIr,
    artifact: &QuantizedArtifactIdentity,
    limit: usize,
    budget: PortBudget,
) -> Result<CandidatePage> {
    let comparison_budget = HnswComparisonBudget::new(budget.max_comparisons());
    quantized_mmap_candidates_within_budget(
        collection_name,
        collection_id,
        registered_vector,
        query,
        artifact,
        limit,
        budget,
        &comparison_budget,
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the mapped artifact adapter carries distinct query, port, and shared traversal budgets"
)]
fn quantized_mmap_candidates_within_budget(
    collection_name: &str,
    collection_id: i64,
    registered_vector: &SearchVector,
    query: &QueryIr,
    artifact: &QuantizedArtifactIdentity,
    limit: usize,
    budget: PortBudget,
    comparison_budget: &HnswComparisonBudget,
) -> Result<CandidatePage> {
    let query_dimensions = nearest_vector(query)?.dimension();
    let post_traversal_query_vector_bytes = dense_vector_copy_bytes(
        query_dimensions,
        2,
        "quantized_post_traversal_query_vector_memory_projection",
    )?;
    let retained_query_vector_bytes = dense_vector_copy_bytes(
        query_dimensions,
        3,
        "quantized_retained_query_vector_memory_projection",
    )?;
    let post_traversal_peak_memory_bytes =
        quantized_post_traversal_peak_bytes(limit, post_traversal_query_vector_bytes)?;
    require_port_memory_bytes(
        post_traversal_peak_memory_bytes,
        budget,
        "quantized_candidate_memory",
    )?;
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
    let (generation_point_high_water, mut rows) = load_mmap_artifact_candidates_with_runtime_budget(
        collection_name,
        &artifact.name,
        &query_vector,
        max_mapped_bytes,
        candidate_limit,
        result_limit,
        budget.max_memory_bytes(),
        post_traversal_peak_memory_bytes,
        retained_query_vector_bytes,
        comparison_budget,
    );
    let graph_visits = take_last_mmap_candidate_visits();
    let merged_capacity = limit.checked_mul(2).ok_or(QueryError::ArithmeticOverflow {
        operation: "quantized_merged_candidate_capacity",
    })?;
    let mut merged_rows = Vec::with_capacity(merged_capacity);
    merged_rows.append(&mut rows);
    drop(rows);
    let delta_rows = mmap_delta_candidates_with_comparison_budget(
        collection_id,
        registered_vector,
        &query_vector,
        generation_point_high_water,
        limit,
        comparison_budget,
    );
    let scored_count = graph_visits.saturating_add(take_last_mmap_delta_visits());
    let mut delta_rows = delta_rows;
    merged_rows.append(&mut delta_rows);
    drop(delta_rows);
    merged_rows.sort_unstable_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    merged_rows.dedup_by_key(|(point_id, _)| *point_id);
    merged_rows.truncate(limit);
    let candidates = merged_rows
        .into_iter()
        .enumerate()
        .map(|(rank, (point_id, score))| {
            let point_id = PointId::from_i64(point_id).ok_or_else(|| QueryError::PortFailure {
                stage: "quantized_hnsw_candidate_source",
                message: format!("invalid PostgreSQL point ID {point_id}"),
            })?;
            let provenance = artifact_candidate_provenance(
                point_id,
                CandidateBranch::Quantized,
                CandidateSourceKind::Quantized,
                ScoreOrder::LowerIsBetter,
                SourceAuthority::DerivedArtifact,
                artifact.generation,
                artifact.configuration,
            )?;
            let source_rank = u32::try_from(rank).unwrap_or(u32::MAX);
            Candidate::new(point_id, f64::from(score), provenance).map(|candidate| {
                candidate.with_diagnostics(CandidateDiagnostics::new(source_rank, 1))
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(
        CandidatePage::with_scored_count(candidates, scored_count, true)
            .with_strategy("quantized_mmap_hnsw")
            .with_expansion_count(1),
    )
}

fn quantized_post_traversal_peak_bytes(
    limit: usize,
    retained_query_vector_bytes: usize,
) -> Result<usize> {
    let tuple_bytes =
        limit
            .checked_mul(size_of::<(i64, f32)>())
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "quantized_candidate_tuple_memory_projection",
            })?;
    let candidate_bytes =
        limit
            .checked_mul(size_of::<Candidate>())
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "quantized_candidate_memory_projection",
            })?;
    // Base rows are moved into an exactly pre-sized 2*limit merge buffer
    // before the independently allocated delta page is loaded. At peak, the
    // merge buffer and delta page retain three tuple batches. Conversion then
    // retains the merge allocation while the final Candidate page is built.
    let merge_peak = tuple_bytes
        .checked_mul(3)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "quantized_candidate_merge_memory_projection",
        })?;
    let conversion_peak = tuple_bytes
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(candidate_bytes))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "quantized_candidate_conversion_memory_projection",
        })?;
    merge_peak
        .max(conversion_peak)
        .checked_add(retained_query_vector_bytes)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "quantized_candidate_query_memory_projection",
        })
}

type HydratedSourceRows = (Vec<HydratedCandidate>, usize);

fn advanced_source_rows(
    collection_name: &str,
    collection_id: i64,
    source_table: &SourceTable,
    query: &QueryIr,
    limit: usize,
    budget: PortBudget,
) -> Result<HydratedSourceRows> {
    require_port_memory_bytes(
        named_source_peak_bytes(limit)?,
        budget,
        "named_source_memory",
    )?;
    require_port_hydration(limit, budget, "named_source_hydration")?;
    let sql_limit = i32::try_from(limit).map_err(|_| QueryError::PortFailure {
        stage: "named_candidate_source",
        message: format!("candidate limit {limit} exceeds PostgreSQL integer"),
    })?;
    advanced_source_rows_within_budget(
        collection_name,
        collection_id,
        source_table,
        query,
        limit,
        sql_limit,
        budget,
    )
}

fn advanced_source_rows_within_budget(
    collection_name: &str,
    collection_id: i64,
    source_table: &SourceTable,
    query: &QueryIr,
    limit: usize,
    sql_limit: i32,
    budget: PortBudget,
) -> Result<HydratedSourceRows> {
    let (rows, scored_count) = match query.kind() {
        QueryKind::Recommend { positive, negative } => {
            let example_count = positive.len().checked_add(negative.len()).ok_or(
                QueryError::ArithmeticOverflow {
                    operation: "recommendation_example_count",
                },
            )?;
            let preparation_memory_limit = recommendation_preparation_memory_limit(limit, budget)?;
            crate::table_search::recommend::require_recommendation_preparation_memory(
                example_count,
                preparation_memory_limit,
            )
            .map_err(|error| QueryError::WorkBudgetExceeded {
                budget: error.budget,
                actual: error.actual,
                maximum: error.maximum,
            })?;
            let scored = crate::table_search::recommend::recommend_collection_from_points_scored_within_budget(
                    collection_name.to_owned(),
                    sql_point_ids(positive.iter().copied())?,
                    sql_point_ids(negative.iter().copied())?,
                    sql_limit,
                    budget.max_comparisons(),
                    preparation_memory_limit,
                )
                .map_err(|error| QueryError::WorkBudgetExceeded {
                    budget: error.budget,
                    actual: error.actual,
                    maximum: error.maximum,
                })?;
            let mut rows = Vec::with_capacity(scored.rows.len());
            for (point_id, source_key, score) in scored.rows {
                rows.push((point_id, source_key, f64::from(score)));
            }
            (rows, scored.scored_count)
        }
        QueryKind::Discover { context } => {
            let preparation_memory_limit = recommendation_preparation_memory_limit(limit, budget)?;
            crate::table_search::recommend::require_recommendation_preparation_memory(
                context.len(),
                preparation_memory_limit,
            )
            .map_err(|error| QueryError::WorkBudgetExceeded {
                budget: error.budget,
                actual: error.actual,
                maximum: error.maximum,
            })?;
            let scored = crate::table_search::recommend::discover_or_explore_collection_scored_within_budget(
                    collection_name.to_owned(),
                    sql_point_ids(context.iter().copied())?,
                    sql_limit,
                    budget.max_comparisons(),
                    preparation_memory_limit,
                )
                .map_err(|error| QueryError::WorkBudgetExceeded {
                    budget: error.budget,
                    actual: error.actual,
                    maximum: error.maximum,
                })?;
            let mut rows = Vec::with_capacity(scored.rows.len());
            for (point_id, source_key, score) in scored.rows {
                rows.push((point_id, source_key, f64::from(score)));
            }
            (rows, scored.scored_count)
        }
        QueryKind::Lookup { point_ids } => {
            let scored_count = point_ids.len().min(limit);
            if scored_count > budget.max_comparisons() {
                return Err(QueryError::WorkBudgetExceeded {
                    budget: "candidate_comparisons",
                    actual: scored_count,
                    maximum: budget.max_comparisons(),
                });
            }
            let rows = lookup_source_rows(collection_id, source_table, point_ids, limit)?;
            (rows, scored_count)
        }
        _ => {
            return Err(QueryError::PortFailure {
                stage: "named_candidate_source",
                message: "query kind has no named PostgreSQL adapter".to_owned(),
            });
        }
    };
    let mut hydrated = Vec::with_capacity(rows.len());
    for (point_id, source_key, score) in rows {
        hydrated.push(HydratedCandidate::new(
            PointId::from_i64(point_id).ok_or_else(|| QueryError::PortFailure {
                stage: "named_candidate_source",
                message: format!("invalid PostgreSQL point ID {point_id}"),
            })?,
            SourceKey::new(source_key)?,
            score,
        )?);
    }
    Ok((hydrated, scored_count))
}

fn lookup_source_rows(
    collection_id: i64,
    source_table: &SourceTable,
    point_ids: &[PointId],
    limit: usize,
) -> Result<Vec<(i64, String, f64)>> {
    let bounded_count = point_ids.len().min(limit);
    let sql_ids = sql_point_ids(point_ids.iter().copied().take(limit))?;
    let table_name =
        quote_qualified_identifier(&source_table.schema_name, &source_table.table_name);
    let sql = format!(
        "WITH requested AS MATERIALIZED (
             SELECT requested.point_id, requested.ordinality
               FROM pg_catalog.unnest($2::bigint[])
                    WITH ORDINALITY AS requested(point_id, ordinality)
         )
         SELECT points.point_id,
                CASE WHEN pg_catalog.octet_length(points.source_key) <= {max_source_key_bytes}
                     THEN points.source_key
                END AS source_key,
                -(requested.ordinality - 1)::double precision AS score
           FROM requested
           JOIN pgcontext._visible_collection_points AS points
             ON points.point_id = requested.point_id
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $1
            AND points.deleted_at IS NULL
          ORDER BY requested.ordinality",
        max_source_key_bytes = context_core::policy::MAX_SOURCE_KEY_BYTES
    );
    Spi::connect(|client| {
        let rows = client
            .select(&sql, None, &[collection_id.into(), sql_ids.into()])
            .map_err(|error| port_failure("lookup_candidate_source", error))?;
        let mut output = Vec::with_capacity(bounded_count);
        for row in rows {
            output.push((
                spi_column::<i64>(&row, 1, "lookup_candidate_source")?,
                spi_column::<String>(&row, 2, "lookup_candidate_source")?,
                spi_column::<f64>(&row, 3, "lookup_candidate_source")?,
            ));
        }
        Ok(output)
    })
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
        QueryKind::Prefetch { branches, .. } => {
            branches.iter().try_fold(0_usize, |total, branch| {
                total
                    .checked_add(projected_candidate_limit(branch, adapter)?)
                    .ok_or(QueryError::ArithmeticOverflow {
                        operation: "composite_candidate_projection",
                    })
            })
        }
        QueryKind::Weighted { query, .. }
        | QueryKind::ScoreThreshold { query, .. }
        | QueryKind::Formula { query, .. }
        | QueryKind::Rerank { query }
        | QueryKind::ExternalRerank { query, .. }
        | QueryKind::TopologyExpand { query, .. } => projected_candidate_limit(query, adapter),
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
        QueryKind::Prefetch { branches, .. } => branches.iter().map(filtered_leaf_count).sum(),
        QueryKind::Weighted { query, .. }
        | QueryKind::ScoreThreshold { query, .. }
        | QueryKind::Formula { query, .. }
        | QueryKind::Rerank { query }
        | QueryKind::ExternalRerank { query, .. }
        | QueryKind::TopologyExpand { query, .. } => filtered_leaf_count(query),
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
        QueryKind::Lexical { .. } | QueryKind::Fuzzy { .. } => {
            // An attached GIN/GiST source probes beyond the requested result
            // limit so the bounded candidate page can still report whether it
            // crossed its allowance. Projecting only `limit` here would leave a
            // registered index no headroom and force a fail-closed budget
            // exhaustion on every indexed query.
            crate::settings::lexical_candidate_budget_from_guc()
                .max(query.limit().saturating_add(1))
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
        Completion::Degraded => raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "query execution returned a degraded strategy outcome",
        ),
    }
}

fn exact_candidate_rows(
    collection_id: i64,
    registered_vector: &SearchVector,
    query: &QueryIr,
    filter: Option<&FilterCandidateBatch>,
    limit: usize,
    max_comparisons: usize,
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
    let sql_result_limit = sql_limit(limit, "candidate_source")?;
    let (filter_sql, point_ids) = match filter {
        Some(filter) => (
            " AND points.point_id = ANY($3::bigint[])",
            Some(sql_point_ids(filter.point_ids().iter().copied())?),
        ),
        None => ("", None),
    };
    let limit_placeholder = if point_ids.is_some() { 4 } else { 3 };
    let probe_placeholder = limit_placeholder + 1;
    let budget_placeholder = limit_placeholder + 2;
    let probe_limit = max_comparisons.saturating_add(1);
    let sql_probe_limit = sql_limit(probe_limit, "candidate_source")?;
    let sql_max_comparisons = sql_limit(max_comparisons, "candidate_source")?;
    let sql = format!(
        "WITH eligible AS MATERIALIZED (
             SELECT points.point_id, source.ctid AS heap_tid
               FROM pgcontext._visible_collection_points AS points
               JOIN {table_name} AS source ON source.id::text = points.source_key
              WHERE points.collection_id = $2
                AND points.deleted_at IS NULL
                {filter_sql}
              LIMIT ${probe_placeholder}
         ),
         admission AS (
             SELECT count(*)::bigint AS scored_count FROM eligible
         ),
         ranked AS MATERIALIZED (
             SELECT eligible.point_id, {score_expression} AS score
               FROM eligible
               JOIN {table_name} AS source ON source.ctid = eligible.heap_tid
               CROSS JOIN admission
              WHERE admission.scored_count <= ${budget_placeholder}
              ORDER BY score ASC, eligible.point_id ASC
              LIMIT ${limit_placeholder}
         )
         SELECT ranked.point_id, ranked.score, admission.scored_count
           FROM admission
           LEFT JOIN ranked ON true
          ORDER BY ranked.score ASC NULLS LAST, ranked.point_id ASC"
    );
    let mut args = Vec::<DatumWithOid<'_>>::with_capacity(6);
    args.push(query_vector.into());
    args.push(collection_id.into());
    if let Some(point_ids) = point_ids {
        args.push(point_ids.into());
    }
    args.push(sql_result_limit.into());
    args.push(sql_probe_limit.into());
    args.push(sql_max_comparisons.into());

    Spi::connect(|client| {
        let rows = client
            .select(&sql, Some(sql_result_limit.max(1)), &args)
            .map_err(|error| port_failure("candidate_source", error))?;
        let mut candidates = Vec::with_capacity(limit);
        let mut scored_count = 0;
        for (rank, row) in rows.into_iter().enumerate() {
            scored_count = usize::try_from(spi_column::<i64>(&row, 3, "candidate_source")?)
                .map_err(|_| QueryError::PortFailure {
                    stage: "candidate_source",
                    message: "exact scored row count exceeds usize".to_owned(),
                })?;
            let Some(point_id) = spi_optional_result_column::<i64>(&row, 1, "candidate_source")?
            else {
                continue;
            };
            let point_id = PointId::from_i64(point_id).ok_or_else(|| QueryError::PortFailure {
                stage: "candidate_source",
                message: format!("negative PostgreSQL point ID: {point_id}"),
            })?;
            let score = f64::from(spi_column::<f32>(&row, 2, "candidate_source")?);
            candidates.push(
                Candidate::new(
                    point_id,
                    score,
                    candidate_provenance(
                        point_id,
                        CandidateBranch::DenseExact,
                        CandidateSourceKind::Exact,
                        query.score_order(),
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
        if scored_count > max_comparisons {
            return Err(QueryError::WorkBudgetExceeded {
                budget: "candidate_comparisons",
                actual: scored_count,
                maximum: max_comparisons,
            });
        }
        Ok(CandidatePage::with_scored_count(
            candidates,
            scored_count,
            true,
        ))
    })
}

fn hnsw_candidate_rows(
    collection_id: i64,
    registered_vector: &SearchVector,
    query: &QueryIr,
    filter: Option<&FilterCandidateBatch>,
    limit: usize,
    max_comparisons: usize,
    max_memory_bytes: usize,
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

    let candidates = crate::hnsw_am::with_hnsw_candidate_helper_budget(
        index_oid,
        max_comparisons,
        max_memory_bytes,
        || {
            Spi::connect(|client| {
                let rows = client
                    .select(&sql, Some(sql_limit), &args)
                    .map_err(|error| port_failure("candidate_source", error))?;
                let mut candidates = Vec::with_capacity(limit);
                for (rank, row) in rows.into_iter().enumerate() {
                    let point_id = spi_point_id(&row, 1, "candidate_source")?;
                    let score = spi_column::<f64>(&row, 2, "candidate_source")?;
                    candidates.push(
                        Candidate::new(
                            point_id,
                            score,
                            candidate_provenance(
                                point_id,
                                CandidateBranch::DenseAnn,
                                CandidateSourceKind::Hnsw,
                                query.score_order(),
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
    let visits = Spi::get_one::<i64>("SELECT node_reads FROM pgcontext.hnsw_last_scan_work()")
        .map_err(|error| port_failure("candidate_source", error))?
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(candidates.len());
    Ok(CandidatePage::with_scored_count(candidates, visits, true))
}

#[cfg(feature = "pg_test")]
pub(crate) fn provenance_occurrences_for_test() -> [OccurrenceId; 4] {
    let point_id = PointId::new(7);
    let generation_one = GenerationId::new(1).expect("fixture generation is non-zero");
    let generation_two = GenerationId::new(2).expect("fixture generation is non-zero");
    let configuration = ConfigurationRevision::new(1).expect("fixture configuration is non-zero");
    [
        candidate_provenance(
            point_id,
            CandidateBranch::DenseExact,
            CandidateSourceKind::Exact,
            ScoreOrder::LowerIsBetter,
            SourceAuthority::PostgreSqlRow,
        )
        .expect("exact fixture provenance")
        .occurrence_id(),
        candidate_provenance(
            point_id,
            CandidateBranch::DenseAnn,
            CandidateSourceKind::Hnsw,
            ScoreOrder::LowerIsBetter,
            SourceAuthority::DerivedArtifact,
        )
        .expect("HNSW fixture provenance")
        .occurrence_id(),
        artifact_candidate_provenance(
            point_id,
            CandidateBranch::DenseAnn,
            CandidateSourceKind::Hnsw,
            ScoreOrder::LowerIsBetter,
            SourceAuthority::DerivedArtifact,
            generation_one,
            configuration,
        )
        .expect("generation one fixture provenance")
        .occurrence_id(),
        artifact_candidate_provenance(
            point_id,
            CandidateBranch::DenseAnn,
            CandidateSourceKind::Hnsw,
            ScoreOrder::LowerIsBetter,
            SourceAuthority::DerivedArtifact,
            generation_two,
            configuration,
        )
        .expect("generation two fixture provenance")
        .occurrence_id(),
    ]
}

#[cfg(feature = "pg_test")]
pub(crate) fn first_provenance_for_test(
    collection_name: &str,
    query: QueryIr,
) -> Result<(CandidateBranch, CandidateSourceKind)> {
    let collection_name = CollectionName::new(collection_name.to_owned())?;
    let collection = resolve_collection(&collection_name);
    let source_table = resolve_source_table(collection.collection_id)?;
    let registered_vectors =
        resolve_query_vectors(&collection_name, collection.collection_id, &query);
    let mut telemetry = PgTelemetrySink::default();
    let outcome = execute_prepared_query_with_vectors(
        collection_name.as_str(),
        collection.collection_id,
        &registered_vectors,
        &source_table,
        &[],
        &query,
        CandidateAdapter::Hnsw,
        &mut telemetry,
    )?;
    let contribution = outcome
        .points()
        .first()
        .and_then(|row| row.contributions().first())
        .ok_or_else(|| QueryError::PortFailure {
            stage: "provenance_test",
            message: "query returned no provenance contribution".to_owned(),
        })?;
    Ok((
        contribution.provenance().branch(),
        contribution.provenance().source(),
    ))
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
    let point_ids = point_ids.into_iter();
    let (minimum, maximum) = point_ids.size_hint();
    let mut output = Vec::with_capacity(maximum.unwrap_or(minimum));
    for point_id in point_ids {
        output.push(
            i64::try_from(point_id.get()).map_err(|_| QueryError::PortFailure {
                stage: "postgres_identity",
                message: format!("point ID {} exceeds PostgreSQL bigint", point_id.get()),
            })?,
        );
    }
    Ok(output)
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

fn spi_optional_result_column<T>(
    row: &spi::SpiHeapTupleData<'_>,
    index: usize,
    stage: &'static str,
) -> Result<Option<T>>
where
    T: FromDatum + IntoDatum,
{
    row.get::<T>(index)
        .map_err(|error| port_failure(stage, error))
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
        ScoreOrder::LowerIsBetter,
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
        ScoreOrder::LowerIsBetter,
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
    let query = QueryIr::nearest(None, vec![1.0, 0.0], ScoreOrder::LowerIsBetter, None, 3)
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
    let query = QueryIr::nearest(None, vec![1.0, 0.0], ScoreOrder::LowerIsBetter, None, 1)
        .unwrap_or_else(|error| raise_query_error(error));
    run_query(&collection_name, query, CandidateAdapter::Hnsw)
}
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
