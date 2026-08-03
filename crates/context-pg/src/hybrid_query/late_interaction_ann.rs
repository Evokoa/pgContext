//! Experimental ANN candidate generation for late-interaction SQL search.

use core::cmp::Ordering;
use core::mem::size_of;
use std::collections::HashSet;

use context_core::{
    CollectionName, DenseVector, Error as CoreError, QualifiedTableName, ScoreOrder, SearchLimit,
    SourceAuthority, SqlIdentifier, policy::MAX_SOURCE_KEY_BYTES,
};
use context_query::MultiVectorAnnStrategyKind;
use context_query::{
    Candidate, CandidateBranch, CandidatePage, CandidateSourceKind, HydratedCandidate, PortBudget,
    QueryError, QueryIr, QueryKind, ReadinessReason, RecheckPage, Result, SourceReadiness,
};
use pgrx::{pg_sys, prelude::*};

use crate::error::{raise_core_error, raise_sql_error};
use crate::late_interaction::{enforce_late_interaction_budget, late_interaction_score};
use crate::vector::Vector;

use super::late_interaction::{
    LateInteractionCandidateStats, late_interaction_ann_candidate_strategy,
    late_interaction_ann_detail, late_interaction_ann_status, late_interaction_ann_strategy_name,
    late_interaction_candidate_stats, late_interaction_rows_from_spi,
    late_interaction_rows_from_spi_with_limit, require_late_interaction_collection_owner,
    require_late_interaction_table_select_privilege, resolve_late_interaction_collection,
    validate_late_interaction_drift,
};
use super::{
    QueryExplainStatus, collection_name_from_sql, policy_to_i64, quote_identifier,
    quote_qualified_identifier, search_limit_from_sql, session_user, spi_iter_required_column,
    spi_optional_column, spi_required_column,
};

type LateInteractionScoredRow = (i64, String, f64);
type LateInteractionScoredWork = (Vec<LateInteractionScoredRow>, usize);

#[derive(Debug, Clone)]
pub(super) struct LateInteractionAnnSource {
    table_oid: pg_sys::Oid,
    schema_name: String,
    table_name: String,
    source_key_column: String,
    vector_column: String,
    vector_dimensions: usize,
}

#[derive(Debug, Clone)]
struct OwnedLateInteractionAnnSource {
    token_column: String,
    dimensions: Option<usize>,
    point_count: usize,
    token_count: usize,
    index_oid: Option<pg_sys::Oid>,
    ready: bool,
}

/// Prepared owned late-interaction source used by composite query ports.
#[derive(Debug, Clone)]
pub(crate) struct CompositeLateInteractionSource {
    collection: super::late_interaction::LateInteractionCollection,
    ann_source: OwnedLateInteractionAnnSource,
    query_vectors: Vec<DenseVector>,
    candidates_per_query: usize,
    query_vector_bytes: usize,
}

impl CompositeLateInteractionSource {
    pub(crate) fn prepare(
        collection_name: &CollectionName,
        query: &QueryIr,
        budget: PortBudget,
    ) -> Result<Self> {
        let QueryKind::LateInteraction {
            vectors,
            candidates_per_query,
        } = query.kind()
        else {
            return Err(QueryError::PortFailure {
                stage: "late_interaction_candidate_source",
                message: "late-interaction adapter requires a late-interaction query".to_owned(),
            });
        };
        let mut collection = resolve_late_interaction_collection(collection_name);
        require_late_interaction_collection_owner(&collection, collection_name);
        let ann_source = resolve_owned_late_interaction_ann_source(collection.collection_id);
        validate_late_interaction_drift(&mut collection, &ann_source.token_column);
        require_late_interaction_table_select_privilege(&collection);
        validate_owned_late_interaction_dimensions(&ann_source, vectors);
        let query_vector_bytes = late_interaction_query_vector_bytes(vectors, 1)?;
        require_late_interaction_memory(
            query_vector_bytes,
            budget.max_memory_bytes(),
            "late_interaction_readiness_memory",
        )?;
        Ok(Self {
            collection,
            ann_source,
            query_vectors: vectors.clone(),
            candidates_per_query: candidates_per_query.get(),
            query_vector_bytes,
        })
    }

    pub(crate) fn readiness(&self) -> SourceReadiness {
        let stats = owned_late_interaction_candidate_stats(&self.ann_source);
        if stats.point_count == 0 {
            SourceReadiness::Exact
        } else if self.ann_source.ready
            && self.ann_source.index_oid.is_some()
            && self.ann_source.dimensions.is_some()
        {
            SourceReadiness::Ready
        } else {
            SourceReadiness::NotReady {
                reason: ReadinessReason::GenerationMissing,
            }
        }
    }

    pub(crate) fn candidate_limit(&self, remaining: usize) -> usize {
        late_interaction_projected_candidate_count(
            self.query_vectors.len(),
            self.candidates_per_query,
        )
        .min(remaining)
    }

    pub(crate) fn candidates(&self, limit: usize, budget: PortBudget) -> Result<CandidatePage> {
        if self.ann_source.point_count == 0 {
            require_late_interaction_memory(
                self.query_vector_bytes,
                budget.max_memory_bytes(),
                "candidate_memory",
            )?;
            return Ok(
                CandidatePage::new(Vec::new(), true).with_strategy("owned_late_interaction_empty")
            );
        }
        let maximum_dimensions = self
            .query_vectors
            .iter()
            .map(DenseVector::dimension)
            .max()
            .unwrap_or_default();
        let temporary_query_bytes = maximum_dimensions
            .checked_mul(size_of::<f32>())
            .and_then(|bytes| bytes.checked_mul(4))
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "late_interaction_candidate_query_memory_projection",
            })?;
        let candidate_working_bytes = late_interaction_candidate_memory_bytes(
            limit,
            self.query_vector_bytes,
            temporary_query_bytes,
        )?;
        let traversal_memory = budget
            .max_memory_bytes()
            .checked_sub(candidate_working_bytes)
            .ok_or(QueryError::WorkBudgetExceeded {
                budget: "candidate_memory",
                actual: candidate_working_bytes,
                maximum: budget.max_memory_bytes(),
            })?;
        require_owned_late_interaction_ready(&self.ann_source);
        crate::hnsw_am::with_hnsw_query_budget(budget.max_comparisons(), traversal_memory, || {
            self.candidates_within_budget(limit)
        })
    }

    fn candidates_within_budget(&self, limit: usize) -> Result<CandidatePage> {
        let per_query = limit
            .checked_div(self.query_vectors.len())
            .unwrap_or_default()
            .max(1)
            .min(self.candidates_per_query);
        let (point_ids, visits) = owned_late_interaction_ann_point_ids(
            self.collection.collection_id,
            &self.query_vectors,
            per_query,
        );
        let candidates = point_ids
            .into_iter()
            .map(|point_id| {
                let point_id = context_core::PointId::from_i64(point_id).ok_or_else(|| {
                    QueryError::PortFailure {
                        stage: "late_interaction_candidate_source",
                        message: format!("invalid PostgreSQL point ID {point_id}"),
                    }
                })?;
                Candidate::new(
                    point_id,
                    0.0,
                    crate::retrieval::candidate_provenance(
                        point_id,
                        CandidateBranch::MultiVector,
                        CandidateSourceKind::MultiVector,
                        ScoreOrder::LowerIsBetter,
                        SourceAuthority::DerivedArtifact,
                    )?,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(CandidatePage::with_scored_count(candidates, visits, true)
            .with_strategy("owned_late_interaction_ann")
            .with_expansion_count(1))
    }

    pub(crate) fn recheck(
        &self,
        candidates: &[Candidate],
        limit: usize,
        budget: PortBudget,
    ) -> Result<RecheckPage> {
        let recheck_memory = budget
            .max_memory_bytes()
            .checked_sub(self.query_vector_bytes)
            .ok_or(QueryError::WorkBudgetExceeded {
                budget: "source_recheck_memory",
                actual: self.query_vector_bytes,
                maximum: budget.max_memory_bytes(),
            })?;
        let vector_memory_bytes = require_late_interaction_recheck_capacity(
            candidates.len(),
            limit,
            recheck_memory,
            budget.max_hydration_bytes(),
        )?;
        let point_ids = candidates
            .iter()
            .map(|candidate| {
                i64::try_from(candidate.point_id().get()).map_err(|_| QueryError::PortFailure {
                    stage: "late_interaction_source_rechecker",
                    message: "point ID exceeds PostgreSQL bigint".to_owned(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let limit = SearchLimit::new(limit).map_err(QueryError::from)?;
        let expected_dimensions =
            self.ann_source
                .dimensions
                .ok_or_else(|| QueryError::PortFailure {
                    stage: "late_interaction_source_rechecker",
                    message: "ready late-interaction source does not declare dimensions".to_owned(),
                })?;
        let (rows, comparisons) = search_late_interaction_candidate_points_with_port_budget(
            &self.collection,
            &self.query_vectors,
            &self.ann_source.token_column,
            &point_ids,
            limit,
            LateInteractionRecheckBudget {
                max_comparisons: budget.max_comparisons(),
                max_vector_memory_bytes: vector_memory_bytes,
                expected_dimensions,
            },
        )?;
        let rows = rows
            .into_iter()
            .map(|(point_id, source_key, score)| {
                HydratedCandidate::new(
                    context_core::PointId::from_i64(point_id).ok_or_else(|| {
                        QueryError::PortFailure {
                            stage: "late_interaction_source_rechecker",
                            message: format!("invalid PostgreSQL point ID {point_id}"),
                        }
                    })?,
                    context_core::SourceKey::new(source_key)?,
                    score,
                )
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(RecheckPage::new(rows, comparisons))
    }
}

fn late_interaction_query_vector_bytes(vectors: &[DenseVector], copies: usize) -> Result<usize> {
    let scalar_cells = vectors.iter().try_fold(0_usize, |total, vector| {
        total
            .checked_add(vector.dimension())
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "late_interaction_query_vector_memory_projection",
            })
    })?;
    late_interaction_shape_copy_bytes(vectors.len(), scalar_cells, copies)
}

fn late_interaction_shape_copy_bytes(
    vector_count: usize,
    scalar_cells: usize,
    copies: usize,
) -> Result<usize> {
    let scalar_bytes = scalar_cells
        .checked_mul(size_of::<f32>())
        .and_then(|bytes| bytes.checked_mul(copies))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "late_interaction_query_vector_memory_projection",
        })?;
    let vector_headers = vector_count
        .checked_mul(size_of::<DenseVector>())
        .and_then(|bytes| bytes.checked_mul(copies))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "late_interaction_query_vector_header_memory_projection",
        })?;
    scalar_bytes
        .checked_add(vector_headers)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "late_interaction_query_vector_memory_projection",
        })
}

fn late_interaction_candidate_memory_bytes(
    limit: usize,
    retained_query_bytes: usize,
    temporary_query_bytes: usize,
) -> Result<usize> {
    // Candidate conversion retains the point-id Vec allocation. Charge that
    // Vec at its possible 2x geometric capacity plus a conservative two-word
    // HashSet bucket/control allowance for every unique ID.
    limit
        .checked_mul(size_of::<Candidate>() + size_of::<i64>() * 4)
        .and_then(|bytes| bytes.checked_add(retained_query_bytes))
        .and_then(|bytes| bytes.checked_add(temporary_query_bytes))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "late_interaction_candidate_memory_projection",
        })
}

fn require_late_interaction_memory(
    actual: usize,
    maximum: usize,
    budget: &'static str,
) -> Result<()> {
    if actual > maximum {
        return Err(QueryError::WorkBudgetExceeded {
            budget,
            actual,
            maximum,
        });
    }
    Ok(())
}

/// Searches the collection through its pgContext-owned late-interaction index.
///
/// Registration binds the source token column and maintains both token rows and
/// the collection-scoped HNSW index, so callers only provide the query and
/// budgets. Candidate generation is approximate; final MaxSim scoring hydrates
/// the source table as the invoker and therefore preserves ACL, RLS, and MVCC.
#[pg_extern(name = "search_late_interaction_ann")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn search_owned_late_interaction_ann(
    collection: String,
    query_vectors: Vec<Vector>,
    candidates_per_query: i32,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f64),
    ),
> {
    let collection_name = collection_name_from_sql(collection);
    let mut collection = resolve_late_interaction_collection(&collection_name);
    require_late_interaction_collection_owner(&collection, &collection_name);
    let ann_source = resolve_owned_late_interaction_ann_source(collection.collection_id);
    validate_late_interaction_drift(&mut collection, &ann_source.token_column);
    require_late_interaction_table_select_privilege(&collection);

    let candidates_per_query = search_limit_from_sql(candidates_per_query);
    let query_vectors = super::late_interaction::dense_vectors_from_sql(
        "late interaction query_vectors",
        query_vectors,
    );
    validate_owned_late_interaction_dimensions(&ann_source, &query_vectors);
    let limit = search_limit_from_sql(limit);
    let projected_candidate_count =
        late_interaction_projected_candidate_count(query_vectors.len(), candidates_per_query.get());
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    crate::collection_limits::enforce_candidate_budget(
        collection.collection_id,
        &collection_name,
        projected_candidate_count,
    );

    let candidate_stats = owned_late_interaction_candidate_stats(&ann_source);
    let ann_strategy = late_interaction_ann_candidate_strategy(
        &query_vectors,
        candidate_stats,
        candidates_per_query,
    );
    match ann_strategy.kind() {
        MultiVectorAnnStrategyKind::AnnCandidateServing => {
            require_owned_late_interaction_ready(&ann_source)
        }
        MultiVectorAnnStrategyKind::ExactNoOp => return TableIterator::new(Vec::new()),
        MultiVectorAnnStrategyKind::Rejected => raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!(
                "late interaction comparison budget exceeded: {} > {}",
                ann_strategy.projected_comparisons(),
                crate::late_interaction::MAX_LATE_INTERACTION_COMPARISONS
            ),
        ),
        MultiVectorAnnStrategyKind::ExactTableScan
        | MultiVectorAnnStrategyKind::PlannedNotServingReady => raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            late_interaction_ann_detail(&ann_strategy),
        ),
    }

    let rows = search_owned_late_interaction_adaptive(
        &collection,
        &collection_name,
        &ann_source,
        &query_vectors,
        candidates_per_query,
        limit,
    );
    TableIterator::new(rows)
}

/// Explains the pgContext-owned late-interaction ANN candidate path.
#[pg_extern(name = "explain_late_interaction_ann")]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
pub fn explain_owned_late_interaction_ann(
    collection: String,
    query_vectors: Vec<Vector>,
    candidates_per_query: i32,
) -> TableIterator<
    'static,
    (
        name!(stage, String),
        name!(detail, String),
        name!(branch, Option<String>),
        name!(strategy, String),
        name!(status, QueryExplainStatus),
        name!(estimated_candidates, Option<i64>),
        name!(candidate_budget, Option<i64>),
    ),
> {
    let collection_name = collection_name_from_sql(collection);
    let mut collection = resolve_late_interaction_collection(&collection_name);
    require_late_interaction_collection_owner(&collection, &collection_name);
    let ann_source = resolve_owned_late_interaction_ann_source(collection.collection_id);
    validate_late_interaction_drift(&mut collection, &ann_source.token_column);
    require_late_interaction_table_select_privilege(&collection);

    let candidates_per_query = search_limit_from_sql(candidates_per_query);
    let query_vectors = super::late_interaction::dense_vectors_from_sql(
        "late interaction query_vectors",
        query_vectors,
    );
    validate_owned_late_interaction_dimensions(&ann_source, &query_vectors);
    let projected_candidate_count =
        late_interaction_projected_candidate_count(query_vectors.len(), candidates_per_query.get());
    crate::collection_limits::enforce_candidate_budget(
        collection.collection_id,
        &collection_name,
        projected_candidate_count,
    );
    let candidate_stats = owned_late_interaction_candidate_stats(&ann_source);
    let ann_strategy = late_interaction_ann_candidate_strategy(
        &query_vectors,
        candidate_stats,
        candidates_per_query,
    );
    let source_status = if ann_source.ready || candidate_stats.point_count == 0 {
        QueryExplainStatus::Ready
    } else {
        QueryExplainStatus::Fallback
    };

    TableIterator::new(vec![
        (
            "ann_source".to_owned(),
            format!(
                "owned_relation=pgcontext._collection_late_interaction_tokens token_source={} index_oid={}",
                ann_source.token_column,
                ann_source
                    .index_oid
                    .map_or_else(|| "none".to_owned(), |oid| oid.to_string()),
            ),
            Some("multi_vector".to_owned()),
            "owned_hnsw_token_candidates".to_owned(),
            source_status,
            None,
            Some(policy_to_i64(
                candidates_per_query.get(),
                "late_interaction_candidates_per_query",
            )),
        ),
        (
            "ann_planner".to_owned(),
            late_interaction_ann_detail(&ann_strategy),
            Some("multi_vector".to_owned()),
            late_interaction_ann_strategy_name(ann_strategy.kind()).to_owned(),
            late_interaction_ann_status(ann_strategy.kind()),
            Some(policy_to_i64(
                ann_strategy.projected_comparisons(),
                "late_interaction_projected_comparisons",
            )),
            Some(policy_to_i64(
                crate::late_interaction::MAX_LATE_INTERACTION_COMPARISONS,
                "max_late_interaction_comparisons",
            )),
        ),
    ])
}

fn resolve_owned_late_interaction_ann_source(collection_id: i64) -> OwnedLateInteractionAnnSource {
    Spi::connect(|client| {
        let rows = client
            .select(
                "SELECT registrations.token_column_name,
                        registrations.dimensions,
                        registrations.point_count,
                        registrations.token_count,
                        registrations.hnsw_index_oid,
                        registrations.status = 'ready'
                            AND index_metadata.indisvalid
                            AND index_metadata.indisready
                            AND access_method.amname = 'pgcontext_hnsw'
                            AND index_class.relname = pg_catalog.format(
                                'pgcontext_late_interaction_%s_hnsw',
                                registrations.collection_id
                            )
                            AND index_metadata.indnkeyatts = 1
                            AND index_metadata.indnatts = 1
                            AND operator_namespace.nspname = 'pgcontext'
                            AND operator_class.opcname = 'vector_hnsw_ip_ops'
                            AND pg_catalog.regexp_replace(
                                pg_catalog.pg_get_expr(
                                    index_metadata.indpred,
                                    index_metadata.indrelid,
                                    true
                                ),
                                '[()[:space:]]',
                                '',
                                'g'
                            ) = pg_catalog.format(
                                'collection_id=%s',
                                registrations.collection_id
                            )
                            AND pg_catalog.regexp_replace(
                                pg_catalog.pg_get_indexdef(
                                    index_metadata.indexrelid,
                                    1,
                                    true
                                ),
                                '[()[:space:]]',
                                '',
                                'g'
                            ) IN (
                                pg_catalog.format(
                                    'token_vector::vector%s',
                                    registrations.dimensions
                                ),
                                pg_catalog.format(
                                    'token_vector::pgcontext.vector%s',
                                    registrations.dimensions
                                )
                            ) AS ready,
                        registrations.source_table_oid = collections.source_table_oid
                            AND token_attribute.attnum = registrations.token_attnum
                            AND token_attribute.atttypid = 'pgcontext.vector[]'::regtype
                            AND token_attribute.attnotnull AS source_binding_valid
                   FROM pgcontext._visible_collection_late_interaction AS registrations
                   JOIN pgcontext._visible_collections AS collections USING (collection_id)
                   LEFT JOIN pg_catalog.pg_attribute AS token_attribute
                     ON token_attribute.attrelid = registrations.source_table_oid
                    AND token_attribute.attname = registrations.token_column_name
                    AND token_attribute.attnum > 0
                    AND NOT token_attribute.attisdropped
                   LEFT JOIN pg_catalog.pg_index AS index_metadata
                     ON index_metadata.indexrelid = registrations.hnsw_index_oid
                    AND index_metadata.indrelid = 'pgcontext._collection_late_interaction_tokens'::regclass
                   LEFT JOIN pg_catalog.pg_class AS index_class
                     ON index_class.oid = index_metadata.indexrelid
                   LEFT JOIN pg_catalog.pg_am AS access_method
                     ON access_method.oid = index_class.relam
                   LEFT JOIN pg_catalog.pg_opclass AS operator_class
                     ON operator_class.oid = index_metadata.indclass[0]
                   LEFT JOIN pg_catalog.pg_namespace AS operator_namespace
                     ON operator_namespace.oid = operator_class.opcnamespace
                  WHERE registrations.collection_id = $1",
                Some(1),
                &[collection_id.into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to resolve owned late-interaction ANN source: {error}"),
                )
            });
        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_OBJECT,
                "late-interaction registration does not exist for collection",
            );
        }
        let row = rows.first();
        if spi_optional_column::<bool>(&row, 7, "late_interaction_source_binding_valid")
            != Some(true)
        {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                "late-interaction source binding has drifted; run pgcontext.repair_late_interaction",
            );
        }
        let dimensions = spi_optional_column::<i32>(&row, 2, "late_interaction_dimensions")
            .map(|value| usize_from_owned_count(i64::from(value), "late_interaction_dimensions"));
        let point_count = usize_from_owned_count(
            spi_required_column::<i64>(&row, 3, "late_interaction_point_count"),
            "late_interaction_point_count",
        );
        let token_count = usize_from_owned_count(
            spi_required_column::<i64>(&row, 4, "late_interaction_token_count"),
            "late_interaction_token_count",
        );
        if (point_count == 0) != (token_count == 0) || (point_count > 0 && dimensions.is_none()) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "late-interaction registration counters are inconsistent; run pgcontext.repair_late_interaction",
            );
        }
        OwnedLateInteractionAnnSource {
            token_column: spi_required_column(&row, 1, "late_interaction_token_column"),
            dimensions,
            point_count,
            token_count,
            index_oid: spi_optional_column(&row, 5, "late_interaction_hnsw_index_oid"),
            ready: spi_optional_column::<bool>(&row, 6, "late_interaction_ready").unwrap_or(false),
        }
    })
}

fn usize_from_owned_count(value: i64, label: &'static str) -> usize {
    usize::try_from(value).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!("{label} is negative or exceeds usize range: {value}"),
        )
    })
}

fn owned_late_interaction_candidate_stats(
    ann_source: &OwnedLateInteractionAnnSource,
) -> LateInteractionCandidateStats {
    LateInteractionCandidateStats {
        point_count: ann_source.point_count,
        vector_count: ann_source.token_count,
    }
}

fn validate_owned_late_interaction_dimensions(
    ann_source: &OwnedLateInteractionAnnSource,
    query_vectors: &[DenseVector],
) {
    let Some(first_query_vector) = query_vectors.first() else {
        raise_core_error(CoreError::InvalidVector(
            "late-interaction query vectors must not be empty".to_owned(),
        ));
    };
    let query_dimensions = first_query_vector.dimension();
    for query_vector in &query_vectors[1..] {
        if query_vector.dimension() != query_dimensions {
            raise_core_error(CoreError::DimensionMismatch {
                left: query_dimensions,
                right: query_vector.dimension(),
            });
        }
    }
    if let Some(expected_dimensions) = ann_source.dimensions
        && query_dimensions != expected_dimensions
    {
        raise_core_error(CoreError::DimensionMismatch {
            left: query_dimensions,
            right: expected_dimensions,
        });
    }
}

fn require_owned_late_interaction_ready(ann_source: &OwnedLateInteractionAnnSource) {
    if !ann_source.ready || ann_source.index_oid.is_none() || ann_source.dimensions.is_none() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            "late-interaction ANN generation is not ready; run pgcontext.repair_late_interaction",
        );
    }
}

fn owned_late_interaction_ann_point_ids(
    collection_id: i64,
    query_vectors: &[DenseVector],
    candidates_per_query: usize,
) -> (Vec<i64>, usize) {
    let candidate_limit = i32::try_from(candidates_per_query).unwrap_or_else(|_| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            "late-interaction candidates_per_query exceeds integer range",
        )
    });
    let mut seen = HashSet::new();
    let mut point_ids = Vec::new();
    let mut visits = 0_usize;
    for query_vector in query_vectors {
        let sql_vector = Vector::from_dense(query_vector.clone());
        Spi::connect(|client| {
            let rows = client
                .select(
                    "SELECT point_id
                       FROM pgcontext._late_interaction_ann_candidate_points($1, $2, $3)",
                    None,
                    &[
                        collection_id.into(),
                        sql_vector.into(),
                        candidate_limit.into(),
                    ],
                )
                .unwrap_or_else(|error| {
                    raise_sql_error(
                        PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                        format!("failed to collect owned late-interaction ANN candidates: {error}"),
                    )
                });
            for row in rows {
                let point_id = spi_iter_required_column::<i64>(&row, 1, "ann_candidate_point_id");
                if seen.insert(point_id) {
                    point_ids.push(point_id);
                }
            }
        });
        let query_visits =
            Spi::get_one::<i64>("SELECT node_reads FROM pgcontext.hnsw_last_scan_work()")
                .ok()
                .flatten()
                .and_then(|count| usize::try_from(count).ok())
                .unwrap_or_default();
        visits = visits.saturating_add(query_visits);
    }
    (point_ids, visits)
}

fn search_owned_late_interaction_adaptive(
    collection: &super::late_interaction::LateInteractionCollection,
    collection_name: &CollectionName,
    ann_source: &OwnedLateInteractionAnnSource,
    query_vectors: &[DenseVector],
    candidates_per_query: SearchLimit,
    limit: SearchLimit,
) -> Vec<(i64, String, f64)> {
    let max_per_query = crate::late_interaction::MAX_LATE_INTERACTION_COMPARISONS
        .checked_div(query_vectors.len())
        .unwrap_or(0)
        .max(1)
        .min(ann_source.token_count);
    let mut candidate_limit = candidates_per_query.get().min(max_per_query);
    loop {
        let projected_candidates =
            late_interaction_projected_candidate_count(query_vectors.len(), candidate_limit);
        crate::collection_limits::enforce_candidate_budget(
            collection.collection_id,
            collection_name,
            projected_candidates,
        );
        let (point_ids, _visits) = owned_late_interaction_ann_point_ids(
            collection.collection_id,
            query_vectors,
            candidate_limit,
        );
        let rows = search_late_interaction_candidate_points(
            collection,
            query_vectors,
            &ann_source.token_column,
            &point_ids,
            limit,
        );
        if rows.len() >= limit.get() || candidate_limit >= ann_source.token_count {
            return rows;
        }
        if candidate_limit >= max_per_query {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                format!(
                    "late interaction visibility recheck exhausted the bounded ANN candidate ceiling at {candidate_limit} candidates per query"
                ),
            );
        }
        candidate_limit = candidate_limit
            .saturating_mul(2)
            .max(candidate_limit.saturating_add(1))
            .min(max_per_query);
    }
}

#[derive(Clone, Copy, Debug)]
struct LateInteractionRecheckBudget {
    max_comparisons: usize,
    max_vector_memory_bytes: usize,
    expected_dimensions: usize,
}

fn require_late_interaction_recheck_capacity(
    candidate_count: usize,
    result_limit: usize,
    max_memory_bytes: usize,
    max_hydration_bytes: usize,
) -> Result<usize> {
    let hydrated_key_bytes = candidate_count.checked_mul(MAX_SOURCE_KEY_BYTES).ok_or(
        QueryError::ArithmeticOverflow {
            operation: "late_interaction_hydration_projection",
        },
    )?;
    if hydrated_key_bytes > max_hydration_bytes {
        return Err(QueryError::WorkBudgetExceeded {
            budget: "source_recheck_hydration",
            actual: hydrated_key_bytes,
            maximum: max_hydration_bytes,
        });
    }

    let point_id_bytes = candidate_count
        .checked_mul(size_of::<i64>())
        .and_then(|bytes| bytes.checked_mul(2))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "late_interaction_point_id_memory_projection",
        })?;
    let scored_row_bytes = candidate_count
        .checked_mul(size_of::<LateInteractionScoredRow>())
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "late_interaction_scored_row_memory_projection",
        })?;
    let vector_list_bytes = candidate_count
        .checked_mul(size_of::<Vec<Vector>>() + size_of::<Vec<DenseVector>>())
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "late_interaction_vector_list_memory_projection",
        })?;
    let result_bytes = result_limit
        .checked_mul(size_of::<HydratedCandidate>())
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "late_interaction_result_memory_projection",
        })?;
    let fixed_bytes = point_id_bytes
        .checked_add(scored_row_bytes)
        .and_then(|bytes| bytes.checked_add(vector_list_bytes))
        .and_then(|bytes| bytes.checked_add(result_bytes))
        .and_then(|bytes| bytes.checked_add(hydrated_key_bytes))
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "late_interaction_recheck_memory_projection",
        })?;
    if fixed_bytes > max_memory_bytes {
        return Err(QueryError::WorkBudgetExceeded {
            budget: "source_recheck_memory",
            actual: fixed_bytes,
            maximum: max_memory_bytes,
        });
    }
    Ok(max_memory_bytes - fixed_bytes)
}

fn search_late_interaction_candidate_points(
    collection: &super::late_interaction::LateInteractionCollection,
    query_vectors: &[DenseVector],
    vector_column: &str,
    point_ids: &[i64],
    limit: SearchLimit,
) -> Vec<(i64, String, f64)> {
    search_late_interaction_candidate_points_with_work(
        collection,
        query_vectors,
        vector_column,
        point_ids,
        limit,
        crate::late_interaction::MAX_LATE_INTERACTION_COMPARISONS,
    )
    .0
}

fn search_late_interaction_candidate_points_with_work(
    collection: &super::late_interaction::LateInteractionCollection,
    query_vectors: &[DenseVector],
    vector_column: &str,
    point_ids: &[i64],
    limit: SearchLimit,
    max_comparisons: usize,
) -> (Vec<(i64, String, f64)>, usize) {
    if point_ids.is_empty() {
        return (Vec::new(), 0);
    }

    let table_name = quote_qualified_identifier(&collection.schema_name, &collection.table_name);
    let vector_column = quote_identifier(vector_column);
    let sql = format!(
        "WITH candidate_points AS MATERIALIZED (
             SELECT DISTINCT candidate_point_id
               FROM unnest($2::bigint[]) AS candidate(candidate_point_id)
         )
         SELECT points.point_id,
                points.source_key,
                source.{vector_column}
           FROM candidate_points AS candidates
           JOIN pgcontext._visible_collection_points AS points
             ON points.point_id = candidates.candidate_point_id
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $1
            AND points.deleted_at IS NULL"
    );
    let mut scored_rows = Spi::connect(|client| {
        let rows = client
            .select(
                &sql,
                None,
                &[collection.collection_id.into(), point_ids.to_vec().into()],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to load owned late-interaction ANN candidates: {error}"),
                )
            });
        late_interaction_rows_from_spi_with_limit(rows, query_vectors, max_comparisons)
    });
    scored_rows.0.sort_by(|left, right| {
        right
            .2
            .partial_cmp(&left.2)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    scored_rows.0.truncate(limit.get());
    scored_rows
}

fn search_late_interaction_candidate_points_with_port_budget(
    collection: &super::late_interaction::LateInteractionCollection,
    query_vectors: &[DenseVector],
    vector_column: &str,
    point_ids: &[i64],
    limit: SearchLimit,
    budget: LateInteractionRecheckBudget,
) -> Result<LateInteractionScoredWork> {
    if point_ids.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let query_vector_count = query_vectors.len();
    let max_candidate_vectors = budget
        .max_comparisons
        .checked_div(query_vector_count)
        .unwrap_or_default();
    let expected_dimensions =
        i64::try_from(budget.expected_dimensions).map_err(|_| QueryError::ArithmeticOverflow {
            operation: "late_interaction_expected_dimensions_conversion",
        })?;
    let max_candidate_vectors =
        i64::try_from(max_candidate_vectors).map_err(|_| QueryError::ArithmeticOverflow {
            operation: "late_interaction_candidate_vector_limit_conversion",
        })?;
    let max_vector_memory_bytes = i64::try_from(budget.max_vector_memory_bytes).map_err(|_| {
        QueryError::ArithmeticOverflow {
            operation: "late_interaction_vector_memory_limit_conversion",
        }
    })?;

    let table_name = quote_qualified_identifier(&collection.schema_name, &collection.table_name);
    let vector_column = quote_identifier(vector_column);
    let sql = bounded_late_interaction_recheck_sql(&table_name, &vector_column);
    let mut scored_rows = Spi::connect(|client| {
        let rows = client
            .select(
                &sql,
                None,
                &[
                    collection.collection_id.into(),
                    point_ids.to_vec().into(),
                    expected_dimensions.into(),
                    max_candidate_vectors.into(),
                    max_vector_memory_bytes.into(),
                ],
            )
            .map_err(|error| QueryError::PortFailure {
                stage: "late_interaction_source_rechecker",
                message: format!("failed to load bounded late-interaction candidates: {error}"),
            })?;
        bounded_late_interaction_rows_from_spi(rows, query_vectors, budget)
    })?;
    scored_rows.0.sort_by(|left, right| {
        right
            .2
            .partial_cmp(&left.2)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    scored_rows.0.truncate(limit.get());
    Ok(scored_rows)
}

fn bounded_late_interaction_recheck_sql(table_name: &str, vector_column: &str) -> String {
    let vector_header_bytes = size_of::<Vector>() + size_of::<DenseVector>();
    let scalar_cell_bytes = size_of::<f32>() * 2;
    format!(
        "WITH candidate_points AS MATERIALIZED (
             SELECT DISTINCT candidate_point_id
               FROM unnest($2::bigint[]) AS candidate(candidate_point_id)
         ),
         candidate_rows AS MATERIALIZED (
             SELECT points.point_id,
                    points.source_key,
                    source.{vector_column} AS candidate_vectors,
                    token_work.token_count,
                    token_work.scalar_cells,
                    token_work.dimensions_valid
               FROM candidate_points AS candidates
               JOIN pgcontext._visible_collection_points AS points
                 ON points.point_id = candidates.candidate_point_id
               JOIN {table_name} AS source ON source.id::text = points.source_key
              CROSS JOIN LATERAL (
                    SELECT pg_catalog.count(*)::bigint AS token_count,
                           COALESCE(pg_catalog.sum(pgcontext.vector_dims(token)), 0)::bigint
                               AS scalar_cells,
                           COALESCE(
                               pg_catalog.bool_and(
                                   token IS NOT NULL
                                   AND pgcontext.vector_dims(token) = $3
                               ),
                               false
                           ) AS dimensions_valid
                      FROM unnest(source.{vector_column}) AS expanded(token)
              ) AS token_work
              WHERE points.collection_id = $1
                AND points.deleted_at IS NULL
         ),
         source_work AS MATERIALIZED (
             SELECT COALESCE(pg_catalog.sum(token_count), 0)::bigint AS token_count,
                    COALESCE(pg_catalog.sum(scalar_cells), 0)::bigint AS scalar_cells,
                    COALESCE(pg_catalog.bool_and(dimensions_valid), true) AS dimensions_valid
               FROM candidate_rows
         ),
         admission AS MATERIALIZED (
             SELECT token_count,
                    scalar_cells,
                    dimensions_valid,
                    dimensions_valid
                        AND token_count <= $4
                        AND token_count * {vector_header_bytes}::bigint
                            + scalar_cells * {scalar_cell_bytes}::bigint <= $5
                        AS admitted
               FROM source_work
         )
         SELECT rows.point_id,
                pg_catalog.octet_length(rows.source_key) <= {MAX_SOURCE_KEY_BYTES}
                    AS source_key_valid,
                CASE WHEN pg_catalog.octet_length(rows.source_key) <= {MAX_SOURCE_KEY_BYTES}
                     THEN rows.source_key
                END AS source_key,
                admission.dimensions_valid,
                admission.token_count,
                admission.scalar_cells,
                CASE WHEN admission.admitted THEN rows.candidate_vectors END AS candidate_vectors
           FROM candidate_rows AS rows
          CROSS JOIN admission"
    )
}

fn bounded_late_interaction_rows_from_spi(
    rows: spi::SpiTupleTable<'_>,
    query_vectors: &[DenseVector],
    budget: LateInteractionRecheckBudget,
) -> Result<LateInteractionScoredWork> {
    let mut output = Vec::with_capacity(rows.len());
    let mut observed_candidate_vectors = 0_usize;
    for row in rows {
        let point_id = spi_iter_required_column::<i64>(&row, 1, "late_interaction_point_id");
        if !spi_iter_required_column::<bool>(&row, 2, "late_interaction_source_key_valid") {
            return Err(QueryError::PortFailure {
                stage: "late_interaction_source_rechecker",
                message: format!(
                    "source key for point ID {point_id} exceeds {MAX_SOURCE_KEY_BYTES} bytes"
                ),
            });
        }
        if !spi_iter_required_column::<bool>(&row, 4, "late_interaction_dimensions_valid") {
            return Err(QueryError::PortFailure {
                stage: "late_interaction_source_rechecker",
                message: format!(
                    "candidate token vector dimensions do not match expected dimensions {}",
                    budget.expected_dimensions
                ),
            });
        }
        let projected_candidate_vectors = bounded_count_from_sql(
            spi_iter_required_column::<i64>(&row, 5, "late_interaction_candidate_vectors"),
            "late_interaction_candidate_vectors",
        )?;
        let projected_scalar_cells = bounded_count_from_sql(
            spi_iter_required_column::<i64>(&row, 6, "late_interaction_scalar_cells"),
            "late_interaction_scalar_cells",
        )?;
        let projected_vector_memory = late_interaction_vector_memory_bytes(
            projected_candidate_vectors,
            projected_scalar_cells,
        )?;
        if projected_vector_memory > budget.max_vector_memory_bytes {
            return Err(QueryError::WorkBudgetExceeded {
                budget: "source_recheck_vector_memory",
                actual: projected_vector_memory,
                maximum: budget.max_vector_memory_bytes,
            });
        }
        let projected_comparisons = query_vectors
            .len()
            .checked_mul(projected_candidate_vectors)
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "late_interaction_recheck_comparison_projection",
            })?;
        if projected_comparisons > budget.max_comparisons {
            return Err(QueryError::WorkBudgetExceeded {
                budget: "source_recheck_comparisons",
                actual: projected_comparisons,
                maximum: budget.max_comparisons,
            });
        }

        let source_key = spi_iter_required_column::<String>(&row, 3, "late_interaction_source_key");
        let candidate_vectors = row
            .get::<Vec<Vector>>(7)
            .map_err(|error| QueryError::PortFailure {
                stage: "late_interaction_source_rechecker",
                message: format!("failed to read bounded late-interaction vector array: {error}"),
            })?
            .ok_or_else(|| QueryError::PortFailure {
                stage: "late_interaction_source_rechecker",
                message: "late-interaction vector array was rejected by resource admission"
                    .to_owned(),
            })?;
        if candidate_vectors.is_empty()
            || candidate_vectors
                .iter()
                .any(|vector| vector.dimension() != budget.expected_dimensions)
        {
            return Err(QueryError::PortFailure {
                stage: "late_interaction_source_rechecker",
                message: format!(
                    "candidate token vectors must be non-empty and have {} dimensions",
                    budget.expected_dimensions
                ),
            });
        }
        observed_candidate_vectors = observed_candidate_vectors
            .checked_add(candidate_vectors.len())
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "late_interaction_candidate_vector_count",
            })?;
        enforce_late_interaction_budget(query_vectors.len(), observed_candidate_vectors);
        let observed_comparisons = query_vectors
            .len()
            .checked_mul(observed_candidate_vectors)
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "late_interaction_recheck_comparison_count",
            })?;
        if observed_comparisons > budget.max_comparisons {
            return Err(QueryError::WorkBudgetExceeded {
                budget: "source_recheck_comparisons",
                actual: observed_comparisons,
                maximum: budget.max_comparisons,
            });
        }
        let candidate_vectors = candidate_vectors
            .into_iter()
            .map(|vector| vector.to_dense().map_err(QueryError::from))
            .collect::<Result<Vec<_>>>()?;
        let score = f64::from(late_interaction_score(query_vectors, &candidate_vectors));
        output.push((point_id, source_key, score));
    }
    let comparisons = query_vectors
        .len()
        .checked_mul(observed_candidate_vectors)
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "late_interaction_recheck_comparison_count",
        })?;
    Ok((output, comparisons))
}

fn bounded_count_from_sql(value: i64, label: &'static str) -> Result<usize> {
    usize::try_from(value).map_err(|_| QueryError::PortFailure {
        stage: "late_interaction_source_rechecker",
        message: format!("{label} is negative or exceeds usize range: {value}"),
    })
}

fn late_interaction_vector_memory_bytes(
    candidate_vector_count: usize,
    scalar_cell_count: usize,
) -> Result<usize> {
    let vector_header_bytes = size_of::<Vector>() + size_of::<DenseVector>();
    let scalar_cell_bytes = size_of::<f32>() * 2;
    candidate_vector_count
        .checked_mul(vector_header_bytes)
        .and_then(|bytes| {
            scalar_cell_count
                .checked_mul(scalar_cell_bytes)
                .and_then(|cells| bytes.checked_add(cells))
        })
        .ok_or(QueryError::ArithmeticOverflow {
            operation: "late_interaction_vector_memory_projection",
        })
}

/// Experimental ANN candidate generation for table-backed late interaction.
///
/// The token table supplies approximate candidate source keys using a
/// `pgcontext_hnsw` index over one token vector per row. Final ordering still
/// hydrates the authoritative collection source table and applies exact MaxSim.
#[pg_extern(name = "search_late_interaction_ann")]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::too_many_arguments,
    reason = "SQL surface keeps the source and token table contract explicit"
)]
pub fn search_late_interaction_ann(
    collection: String,
    query_vectors: Vec<Vector>,
    vector_column: String,
    token_table: String,
    token_source_key_column: String,
    token_vector_column: String,
    candidates_per_query: i32,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f64),
    ),
> {
    let collection_name = collection_name_from_sql(collection);
    let mut collection = resolve_late_interaction_collection(&collection_name);
    require_late_interaction_collection_owner(&collection, &collection_name);
    validate_late_interaction_drift(&mut collection, &vector_column);
    require_late_interaction_table_select_privilege(&collection);

    let ann_source = resolve_late_interaction_ann_source(
        &token_table,
        &token_source_key_column,
        &token_vector_column,
    );
    require_late_interaction_ann_table_select_privilege(&ann_source);
    let candidates_per_query = search_limit_from_sql(candidates_per_query);
    let query_vectors = super::late_interaction::dense_vectors_from_sql(
        "late interaction query_vectors",
        query_vectors,
    );
    validate_late_interaction_ann_token_dimensions(&ann_source, &query_vectors);
    let limit = search_limit_from_sql(limit);
    let projected_candidate_count =
        late_interaction_projected_candidate_count(query_vectors.len(), candidates_per_query.get());
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    crate::collection_limits::enforce_candidate_budget(
        collection.collection_id,
        &collection_name,
        projected_candidate_count,
    );

    let candidate_stats = late_interaction_candidate_stats(&collection, &vector_column);
    let ann_strategy = late_interaction_ann_candidate_strategy(
        &query_vectors,
        candidate_stats,
        candidates_per_query,
    );
    match ann_strategy.kind() {
        MultiVectorAnnStrategyKind::AnnCandidateServing => {}
        MultiVectorAnnStrategyKind::ExactNoOp => return TableIterator::new(Vec::new()),
        MultiVectorAnnStrategyKind::Rejected => raise_sql_error(
            PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
            format!(
                "late interaction comparison budget exceeded: {} > {}",
                ann_strategy.projected_comparisons(),
                crate::late_interaction::MAX_LATE_INTERACTION_COMPARISONS
            ),
        ),
        MultiVectorAnnStrategyKind::ExactTableScan
        | MultiVectorAnnStrategyKind::PlannedNotServingReady => raise_sql_error(
            PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
            late_interaction_ann_detail(&ann_strategy),
        ),
    }

    let source_keys =
        late_interaction_ann_source_keys(&ann_source, &query_vectors, candidates_per_query);
    let rows = search_late_interaction_candidate_keys(
        &collection,
        &query_vectors,
        &vector_column,
        &source_keys,
        limit,
    );
    TableIterator::new(rows)
}

/// Explains experimental ANN candidate generation for late interaction.
#[pg_extern(name = "explain_late_interaction_ann")]
#[search_path(pg_catalog, pgcontext, public)]
#[allow(
    clippy::too_many_arguments,
    clippy::type_complexity,
    reason = "pgrx SQL generation requires the explicit table row tuple"
)]
pub fn explain_late_interaction_ann(
    collection: String,
    query_vectors: Vec<Vector>,
    vector_column: String,
    token_table: String,
    token_source_key_column: String,
    token_vector_column: String,
    candidates_per_query: i32,
) -> TableIterator<
    'static,
    (
        name!(stage, String),
        name!(detail, String),
        name!(branch, Option<String>),
        name!(strategy, String),
        name!(status, QueryExplainStatus),
        name!(estimated_candidates, Option<i64>),
        name!(candidate_budget, Option<i64>),
    ),
> {
    let collection_name = collection_name_from_sql(collection);
    let mut collection = resolve_late_interaction_collection(&collection_name);
    require_late_interaction_collection_owner(&collection, &collection_name);
    validate_late_interaction_drift(&mut collection, &vector_column);
    require_late_interaction_table_select_privilege(&collection);

    let ann_source = resolve_late_interaction_ann_source(
        &token_table,
        &token_source_key_column,
        &token_vector_column,
    );
    require_late_interaction_ann_table_select_privilege(&ann_source);
    let candidates_per_query = search_limit_from_sql(candidates_per_query);
    let query_vectors = super::late_interaction::dense_vectors_from_sql(
        "late interaction query_vectors",
        query_vectors,
    );
    validate_late_interaction_ann_token_dimensions(&ann_source, &query_vectors);
    let projected_candidate_count =
        late_interaction_projected_candidate_count(query_vectors.len(), candidates_per_query.get());
    crate::collection_limits::enforce_candidate_budget(
        collection.collection_id,
        &collection_name,
        projected_candidate_count,
    );
    let candidate_stats = late_interaction_candidate_stats(&collection, &vector_column);
    let ann_strategy = late_interaction_ann_candidate_strategy(
        &query_vectors,
        candidate_stats,
        candidates_per_query,
    );

    TableIterator::new(vec![
        (
            "ann_source".to_owned(),
            format!(
                "token_table={}.{} source_key_column={} token_vector_column={}",
                ann_source.schema_name,
                ann_source.table_name,
                ann_source.source_key_column,
                ann_source.vector_column
            ),
            Some("multi_vector".to_owned()),
            "hnsw_token_candidates".to_owned(),
            QueryExplainStatus::Ready,
            Some(policy_to_i64(
                candidate_stats.vector_count,
                "late_interaction_candidate_vectors",
            )),
            Some(policy_to_i64(
                candidates_per_query.get(),
                "late_interaction_candidates_per_query",
            )),
        ),
        (
            "ann_planner".to_owned(),
            late_interaction_ann_detail(&ann_strategy),
            Some("multi_vector".to_owned()),
            late_interaction_ann_strategy_name(ann_strategy.kind()).to_owned(),
            late_interaction_ann_status(ann_strategy.kind()),
            Some(policy_to_i64(
                ann_strategy.projected_comparisons(),
                "late_interaction_projected_comparisons",
            )),
            Some(policy_to_i64(
                crate::late_interaction::MAX_LATE_INTERACTION_COMPARISONS,
                "max_late_interaction_comparisons",
            )),
        ),
    ])
}

fn late_interaction_projected_candidate_count(
    query_vector_count: usize,
    candidates_per_query: usize,
) -> usize {
    query_vector_count
        .checked_mul(candidates_per_query)
        .unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_PROGRAM_LIMIT_EXCEEDED,
                "late interaction candidate budget overflow",
            )
        })
}

fn validate_late_interaction_ann_token_dimensions(
    ann_source: &LateInteractionAnnSource,
    query_vectors: &[DenseVector],
) {
    let Some(first_query_vector) = query_vectors.first() else {
        raise_core_error(CoreError::InvalidVector(
            "late-interaction query vectors must not be empty".to_owned(),
        ));
    };
    let expected_dimensions = first_query_vector.dimension();
    for query_vector in &query_vectors[1..] {
        if query_vector.dimension() != expected_dimensions {
            raise_core_error(CoreError::DimensionMismatch {
                left: expected_dimensions,
                right: query_vector.dimension(),
            });
        }
    }

    if expected_dimensions != ann_source.vector_dimensions {
        raise_core_error(CoreError::DimensionMismatch {
            left: expected_dimensions,
            right: ann_source.vector_dimensions,
        });
    }
}

fn search_late_interaction_candidate_keys(
    collection: &super::late_interaction::LateInteractionCollection,
    query_vectors: &[DenseVector],
    vector_column: &str,
    source_keys: &[String],
    limit: SearchLimit,
) -> Vec<(i64, String, f64)> {
    if source_keys.is_empty() {
        return Vec::new();
    }

    let table_name = quote_qualified_identifier(&collection.schema_name, &collection.table_name);
    let vector_column = quote_identifier(vector_column);
    let sql = format!(
        "WITH candidate_keys AS MATERIALIZED (
             SELECT DISTINCT key::text AS source_key
               FROM unnest($2::text[]) AS key
         )
         SELECT points.point_id,
                points.source_key,
                source.{vector_column}
           FROM candidate_keys AS candidates
           JOIN pgcontext._visible_collection_points AS points
             ON points.source_key = candidates.source_key
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $1
            AND points.deleted_at IS NULL"
    );

    let mut scored_rows = Spi::connect(|client| {
        let rows = match client.select(
            &sql,
            None,
            &[collection.collection_id.into(), source_keys.to_vec().into()],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to load late-interaction ANN candidates: {error}"),
            ),
        };
        late_interaction_rows_from_spi(rows, query_vectors)
    });

    scored_rows.sort_by(|left, right| {
        right
            .2
            .partial_cmp(&left.2)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    scored_rows.truncate(limit.get());
    scored_rows
}

fn late_interaction_ann_source_keys(
    ann_source: &LateInteractionAnnSource,
    query_vectors: &[DenseVector],
    candidates_per_query: SearchLimit,
) -> Vec<String> {
    let table_name = quote_qualified_identifier(&ann_source.schema_name, &ann_source.table_name);
    let source_key_column = quote_identifier(&ann_source.source_key_column);
    let vector_column = quote_identifier(&ann_source.vector_column);
    let sql = format!(
        "SELECT token.{source_key_column}::text
           FROM {table_name} AS token
          ORDER BY token.{vector_column} OPERATOR(pgcontext.<#>) $1
          LIMIT $2"
    );

    let mut source_keys = Vec::<String>::new();
    let candidate_limit = policy_to_i64(
        candidates_per_query.get(),
        "late_interaction_candidates_per_query",
    );
    for query_vector in query_vectors {
        let sql_vector = Vector::from_dense(query_vector.clone());
        Spi::connect(|client| {
            let rows = match client.select(&sql, None, &[sql_vector.into(), candidate_limit.into()])
            {
                Ok(rows) => rows,
                Err(error) => raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to collect late-interaction ANN candidates: {error}"),
                ),
            };
            for row in rows {
                let source_key =
                    spi_iter_required_column::<String>(&row, 1, "ann_candidate_source_key");
                if !source_keys.contains(&source_key) {
                    source_keys.push(source_key);
                }
            }
            Ok::<_, spi::Error>(())
        })
        .unwrap_or_else(|error| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to collect late-interaction ANN candidates: {error}"),
            )
        });
    }
    source_keys
}

fn resolve_late_interaction_ann_source(
    token_table: &str,
    source_key_column: &str,
    vector_column: &str,
) -> LateInteractionAnnSource {
    let table_name = qualified_table_name_from_sql(token_table);
    let source_key_column = sql_identifier_from_sql(source_key_column);
    let vector_column = sql_identifier_from_sql(vector_column);
    let schema_name = table_name.schema().as_str().to_owned();
    let relation_name = table_name.table().as_str().to_owned();
    let source_key_column_name = source_key_column.as_str().to_owned();
    let vector_column_name = vector_column.as_str().to_owned();

    Spi::connect(|client| {
        let rows = match client.select(
            "SELECT class.oid,
                    source_attribute.attname IS NOT NULL AS source_key_exists,
                    source_attribute.attnotnull AS source_key_not_null,
                    vector_attribute.attname IS NOT NULL AS vector_exists,
                    vector_attribute.atttypid = 'pgcontext.vector'::regtype AS vector_is_valid,
                    vector_attribute.atttypmod AS vector_typmod,
                    vector_attribute.attnotnull AS vector_not_null,
                    EXISTS (
                        SELECT 1
                          FROM pg_catalog.pg_index AS idx
                          JOIN pg_catalog.pg_class AS index_class ON index_class.oid = idx.indexrelid
                          JOIN pg_catalog.pg_am AS am ON am.oid = index_class.relam
                         WHERE idx.indrelid = class.oid
                           AND am.amname = 'pgcontext_hnsw'
                           AND idx.indisvalid
                           AND idx.indisready
                           AND idx.indpred IS NULL
                           AND vector_attribute.attnum = ANY(idx.indkey::int2[])
                    ) AS has_hnsw_index
               FROM pg_catalog.pg_class AS class
               JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = class.relnamespace
               LEFT JOIN pg_catalog.pg_attribute AS source_attribute
                 ON source_attribute.attrelid = class.oid
                AND source_attribute.attname = $3
                AND source_attribute.attnum > 0
                AND NOT source_attribute.attisdropped
               LEFT JOIN pg_catalog.pg_attribute AS vector_attribute
                 ON vector_attribute.attrelid = class.oid
                AND vector_attribute.attname = $4
                AND vector_attribute.attnum > 0
                AND NOT vector_attribute.attisdropped
              WHERE namespace.nspname = $1
                AND class.relname = $2
                AND class.relkind IN ('r', 'p')",
            Some(1),
            &[
                schema_name.as_str().into(),
                relation_name.as_str().into(),
                source_key_column_name.as_str().into(),
                vector_column_name.as_str().into(),
            ],
        ) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to validate late-interaction ANN token table: {error}"),
            ),
        };

        if rows.is_empty() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_TABLE,
                format!("late-interaction ANN token table does not exist: {token_table}"),
            );
        }

        let row = rows.first();
        if !spi_required_column::<bool>(&row, 2, "ann_source_key_exists") {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "late-interaction ANN source key column does not exist: {token_table}.{source_key_column_name}"
                ),
            );
        }
        if spi_optional_column::<bool>(&row, 3, "ann_source_key_not_null") != Some(true) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                format!(
                    "late-interaction ANN source key column must be NOT NULL: {token_table}.{source_key_column_name}"
                ),
            );
        }
        if !spi_required_column::<bool>(&row, 4, "ann_vector_exists") {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_UNDEFINED_COLUMN,
                format!(
                    "late-interaction ANN vector column does not exist: {token_table}.{vector_column_name}"
                ),
            );
        }
        if spi_optional_column::<bool>(&row, 5, "ann_vector_is_valid") != Some(true) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATATYPE_MISMATCH,
                format!(
                    "late-interaction ANN vector column must have type vector: {token_table}.{vector_column_name}"
                ),
            );
        }
        if spi_optional_column::<bool>(&row, 7, "ann_vector_not_null") != Some(true) {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                format!(
                    "late-interaction ANN vector column must be NOT NULL: {token_table}.{vector_column_name}"
                ),
            );
        }
        let vector_typmod = spi_optional_column::<i32>(&row, 6, "ann_vector_typmod");
        let vector_dimensions = match vector_typmod.and_then(|value| usize::try_from(value).ok()) {
            Some(dimensions) if dimensions > 0 => dimensions,
            _ => raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                format!(
                    "late-interaction ANN vector column must declare dimensions with vector(n): {token_table}.{vector_column_name}"
                ),
            ),
        };
        if !spi_required_column::<bool>(&row, 8, "ann_has_hnsw_index") {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_OBJECT_NOT_IN_PREREQUISITE_STATE,
                format!(
                    "late-interaction ANN token table requires a pgcontext_hnsw index on {token_table}.{vector_column_name}"
                ),
            );
        }

        LateInteractionAnnSource {
            table_oid: spi_required_column::<pg_sys::Oid>(&row, 1, "ann_table_oid"),
            schema_name,
            table_name: relation_name,
            source_key_column: source_key_column_name,
            vector_column: vector_column_name,
            vector_dimensions,
        }
    })
}

fn require_late_interaction_ann_table_select_privilege(ann_source: &LateInteractionAnnSource) {
    let session_user = session_user();
    let has_select = Spi::get_one_with_args::<bool>(
        "SELECT pg_catalog.has_table_privilege($1, $2, 'SELECT')",
        &[session_user.as_str().into(), ann_source.table_oid.into()],
    )
    .unwrap_or_else(|error| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
            format!("failed to check ANN token table privileges: {error}"),
        )
    })
    .unwrap_or(false);

    if !has_select {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INSUFFICIENT_PRIVILEGE,
            format!(
                "permission denied for ANN token table: {}.{}",
                ann_source.schema_name, ann_source.table_name
            ),
        );
    }
}

fn qualified_table_name_from_sql(table_name: &str) -> QualifiedTableName {
    match QualifiedTableName::new(table_name) {
        Ok(table_name) => table_name,
        Err(error) => raise_core_error(error),
    }
}

fn sql_identifier_from_sql(identifier: &str) -> SqlIdentifier {
    match SqlIdentifier::new(identifier) {
        Ok(identifier) => identifier,
        Err(error) => raise_core_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composite_recheck_rejects_tiny_memory_before_source_loading() {
        let result = require_late_interaction_recheck_capacity(1, 1, 1, MAX_SOURCE_KEY_BYTES);

        assert!(matches!(
            result,
            Err(QueryError::WorkBudgetExceeded {
                budget: "source_recheck_memory",
                maximum: 1,
                ..
            })
        ));
    }

    #[test]
    fn composite_recheck_rejects_tiny_hydration_before_source_loading() {
        let result = require_late_interaction_recheck_capacity(2, 1, usize::MAX, 1);

        assert_eq!(
            result,
            Err(QueryError::WorkBudgetExceeded {
                budget: "source_recheck_hydration",
                actual: MAX_SOURCE_KEY_BYTES * 2,
                maximum: 1,
            })
        );
    }

    #[test]
    fn composite_recheck_sql_gates_keys_and_vectors_before_materialization() {
        let sql = bounded_late_interaction_recheck_sql("public.items", "token_vectors");

        assert!(sql.contains("octet_length(rows.source_key) <= 1024"));
        assert!(sql.contains("CASE WHEN admission.admitted THEN rows.candidate_vectors END"));
        assert!(sql.contains("AND pgcontext.vector_dims(token) = $3"));
        assert!(sql.contains("token_count <= $4"));
        assert!(sql.contains("scalar_cells"));
    }

    #[test]
    fn composite_recheck_vector_memory_projection_is_checked() {
        assert!(matches!(
            late_interaction_vector_memory_bytes(usize::MAX, 1),
            Err(QueryError::ArithmeticOverflow {
                operation: "late_interaction_vector_memory_projection"
            })
        ));
    }

    #[test]
    fn maximum_query_scalar_cache_is_admitted_only_at_exact_boundary() {
        let bytes = late_interaction_shape_copy_bytes(
            1,
            context_query::MAX_LATE_INTERACTION_SCALAR_CELLS,
            1,
        )
        .unwrap_or_else(|error| unreachable!("policy scalar cells must fit usize: {error}"));

        assert!(
            require_late_interaction_memory(bytes, bytes, "late_interaction_readiness_memory")
                .is_ok()
        );
        assert!(
            require_late_interaction_memory(bytes, bytes - 1, "late_interaction_readiness_memory")
                .is_err()
        );
    }

    #[test]
    fn many_short_query_vectors_include_every_cached_vector_header() {
        let vector_count = context_query::MAX_LATE_INTERACTION_SCALAR_CELLS;
        let bytes = late_interaction_shape_copy_bytes(vector_count, vector_count, 1)
            .unwrap_or_else(|error| unreachable!("policy query shape must fit usize: {error}"));
        let scalar_only = vector_count * size_of::<f32>();

        assert_eq!(bytes, scalar_only + vector_count * size_of::<DenseVector>());
        assert!(
            require_late_interaction_memory(
                bytes,
                scalar_only,
                "late_interaction_readiness_memory"
            )
            .is_err()
        );
    }

    #[test]
    fn unique_candidate_ids_include_vec_growth_and_hash_bucket_storage() {
        let limit = 1_024;
        let projected = late_interaction_candidate_memory_bytes(limit, 0, 0)
            .unwrap_or_else(|error| unreachable!("candidate projection must fit: {error}"));
        let flat_candidates = limit * size_of::<Candidate>();
        let container_bytes = limit * size_of::<i64>() * 4;

        assert_eq!(projected, flat_candidates + container_bytes);
        assert!(require_late_interaction_memory(projected, projected, "candidate_memory").is_ok());
        assert!(
            require_late_interaction_memory(projected, projected - 1, "candidate_memory").is_err()
        );
    }
}
