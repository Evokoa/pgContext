//! Validated query intermediate representation.

use std::collections::BTreeSet;

use context_core::policy::{MAX_FILTER_DEPTH, MAX_FILTER_NODES, MAX_RECALL_CHECK_POINT_IDS};
use context_core::{DenseVector, PointId, SearchLimit, SparseVector, VectorName};
use context_filter::{Filter, parse_filter_json};
use serde_json::Value as JsonValue;

use crate::{
    Formula, FuzzyQuery, FuzzySourceName, LateInteractionWork, LexicalQuery, LexicalSourceName,
    MAX_LATE_INTERACTION_COMPARISONS, MAX_LATE_INTERACTION_SCALAR_CELLS, QueryError, Result,
    ScoreOrder,
};

/// Maximum nesting depth accepted by a typed query plan.
pub const MAX_QUERY_DEPTH: usize = 32;
/// Maximum total nodes accepted by a typed query plan.
pub const MAX_QUERY_NODES: usize = 256;
const MAX_FILTER_SCALAR_BYTES: usize = 64 * 1024;

/// Rank-only fusion policy for a prefetch node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fusion {
    /// Reciprocal-rank fusion with equal branch weight.
    Rrf {
        /// Positive denominator constant.
        rank_constant: u32,
    },
    /// Reciprocal-rank fusion with explicit branch weights.
    WeightedRrf {
        /// Positive denominator constant.
        rank_constant: u32,
    },
}

impl Fusion {
    /// Conventional reciprocal-rank fusion policy (`k = 60`).
    pub const STANDARD_RRF: Self = Self::Rrf { rank_constant: 60 };

    /// Returns the positive denominator constant.
    #[must_use]
    pub const fn rank_constant(self) -> u32 {
        match self {
            Self::Rrf { rank_constant } | Self::WeightedRrf { rank_constant } => rank_constant,
        }
    }
}

/// Application-level query shape independent of PostgreSQL JSONB conversion.
#[derive(Clone, Debug, PartialEq)]
pub enum QueryKind {
    /// Nearest-neighbor retrieval over a selected dense vector.
    Nearest {
        /// Optional named-vector selector.
        vector_name: Option<VectorName>,
        /// Validated query vector.
        vector: DenseVector,
    },
    /// Nearest-neighbor retrieval over a selected sparse vector.
    SparseNearest {
        /// Named sparse-vector selector.
        vector_name: VectorName,
        /// Validated sparse query vector.
        vector: SparseVector,
    },
    /// PostgreSQL-native lexical retrieval over a registered lexical source.
    Lexical {
        /// Registered lexical source identifier.
        source: LexicalSourceName,
        /// Validated typed lexical query.
        query: LexicalQuery,
    },
    /// PostgreSQL `pg_trgm` retrieval over a registered fuzzy source.
    Fuzzy {
        /// Registered fuzzy source identifier.
        source: FuzzySourceName,
        /// Validated typed trigram query.
        query: FuzzyQuery,
    },
    /// Owned late-interaction retrieval over query token vectors.
    LateInteraction {
        /// Validated nonempty query token vectors.
        vectors: Vec<DenseVector>,
        /// Candidate budget requested per query token.
        candidates_per_query: SearchLimit,
    },
    /// Positive/negative-example recommendation.
    Recommend {
        /// Positive logical examples.
        positive: Vec<PointId>,
        /// Negative logical examples.
        negative: Vec<PointId>,
    },
    /// Diversity-oriented discovery from context examples.
    Discover {
        /// Logical context examples.
        context: Vec<PointId>,
    },
    /// Ordered lookup of one or more logical points.
    Lookup {
        /// Ordered logical points to load.
        point_ids: Vec<PointId>,
    },
    /// Parallel prefetch branches consumed by a later query stage.
    Prefetch {
        /// Owned child queries.
        branches: Vec<QueryIr>,
        /// Explicit rank-only fusion policy.
        fusion: Fusion,
    },
    /// Weighted child query.
    Weighted {
        /// Owned child query.
        query: Box<QueryIr>,
        /// Finite non-negative weight.
        weight: f64,
    },
    /// Score threshold around a child query.
    ScoreThreshold {
        /// Owned child query.
        query: Box<QueryIr>,
        /// Optional inclusive minimum.
        minimum: Option<f64>,
        /// Optional inclusive maximum.
        maximum: Option<f64>,
    },
    /// Validated formula wrapper around a child query.
    Formula {
        /// Owned child query.
        query: Box<QueryIr>,
        /// Bounded formula text.
        formula: Formula,
    },
    /// Final deterministic score-ordering and result-limit request.
    Rerank {
        /// Owned child query.
        query: Box<QueryIr>,
    },
    /// Model-backed reranking through a query-owned port.
    ExternalRerank {
        /// Owned child query.
        query: Box<QueryIr>,
        /// Immutable adapter/model revision required by the plan.
        model_revision: u64,
    },
    /// Bounded graph or topology expansion through a query-owned port.
    TopologyExpand {
        /// Owned seed query.
        query: Box<QueryIr>,
        /// Maximum expansion depth.
        max_depth: usize,
    },
}

/// Validated query request consumed by pure execution ports.
#[derive(Clone, Debug, PartialEq)]
pub struct QueryIr {
    kind: QueryKind,
    filter: Option<Filter>,
    limit: SearchLimit,
    score_order: ScoreOrder,
}

impl QueryIr {
    /// Creates a validated nearest-neighbor request.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for an invalid vector, vector name,
    /// filter shape, or zero limit.
    pub fn nearest(
        vector_name: Option<String>,
        vector: Vec<f32>,
        score_order: ScoreOrder,
        filter: Option<JsonValue>,
        limit: usize,
    ) -> Result<Self> {
        let query = Self {
            kind: QueryKind::Nearest {
                vector_name: vector_name.map(VectorName::new).transpose()?,
                vector: DenseVector::new(vector)?,
            },
            filter: parse_filter(filter)?,
            limit: SearchLimit::new(limit)?,
            score_order,
        };
        query.validate()?;
        Ok(query)
    }

    /// Creates a validated sparse nearest-neighbor request.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for an invalid vector name, sparse
    /// vector, filter shape, or zero limit.
    pub fn sparse_nearest(
        vector_name: String,
        vector: SparseVector,
        score_order: ScoreOrder,
        filter: Option<JsonValue>,
        limit: usize,
    ) -> Result<Self> {
        let query = Self {
            kind: QueryKind::SparseNearest {
                vector_name: VectorName::new(vector_name)?,
                vector,
            },
            filter: parse_filter(filter)?,
            limit: SearchLimit::new(limit)?,
            score_order,
        };
        query.validate()?;
        Ok(query)
    }

    /// Creates a validated registered lexical leaf request.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for an invalid lexical query tree,
    /// filter shape, or zero limit.
    pub fn lexical(
        source: LexicalSourceName,
        query: LexicalQuery,
        filter: Option<JsonValue>,
        limit: usize,
    ) -> Result<Self> {
        let query = Self {
            kind: QueryKind::Lexical { source, query },
            filter: parse_filter(filter)?,
            limit: SearchLimit::new(limit)?,
            score_order: ScoreOrder::HigherIsBetter,
        };
        query.validate()?;
        Ok(query)
    }

    /// Creates a validated registered fuzzy leaf request.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for an invalid filter shape or
    /// zero limit.
    pub fn fuzzy(
        source: FuzzySourceName,
        query: FuzzyQuery,
        filter: Option<JsonValue>,
        limit: usize,
    ) -> Result<Self> {
        let query = Self {
            kind: QueryKind::Fuzzy { source, query },
            filter: parse_filter(filter)?,
            limit: SearchLimit::new(limit)?,
            score_order: ScoreOrder::HigherIsBetter,
        };
        query.validate()?;
        Ok(query)
    }

    /// Creates a validated owned late-interaction leaf request.
    pub fn late_interaction(
        vectors: Vec<Vec<f32>>,
        candidates_per_query: usize,
        limit: usize,
    ) -> Result<Self> {
        if vectors.is_empty() {
            return Err(invalid("query_vectors", "must contain at least one vector"));
        }
        validate_late_interaction_raw_shape(&vectors, candidates_per_query)?;
        let vectors = vectors
            .into_iter()
            .map(DenseVector::new)
            .collect::<core::result::Result<Vec<_>, _>>()?;
        let dimensions = vectors[0].dimension();
        if vectors
            .iter()
            .any(|vector| vector.dimension() != dimensions)
        {
            return Err(invalid(
                "query_vectors",
                "all vectors must have the same dimensions",
            ));
        }
        let query = Self {
            kind: QueryKind::LateInteraction {
                vectors,
                candidates_per_query: SearchLimit::new(candidates_per_query)?,
            },
            filter: None,
            limit: SearchLimit::new(limit)?,
            score_order: ScoreOrder::HigherIsBetter,
        };
        query.validate()?;
        Ok(query)
    }

    /// Creates a query from an application-level kind.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when recursive query semantics,
    /// filter shape, or limit are invalid.
    pub fn new(
        kind: QueryKind,
        score_order: ScoreOrder,
        filter: Option<JsonValue>,
        limit: usize,
    ) -> Result<Self> {
        let query = Self {
            kind,
            filter: parse_filter(filter)?,
            limit: SearchLimit::new(limit)?,
            score_order,
        };
        query.validate()?;
        Ok(query)
    }

    /// Returns the application query shape.
    #[must_use]
    pub const fn kind(&self) -> &QueryKind {
        &self.kind
    }

    /// Returns optional filter JSON for a filter-candidate adapter.
    #[must_use]
    pub const fn filter(&self) -> Option<&Filter> {
        self.filter.as_ref()
    }

    /// Returns the requested final result limit.
    #[must_use]
    pub const fn limit(&self) -> usize {
        self.limit.get()
    }

    /// Returns final score ordering semantics.
    #[must_use]
    pub const fn score_order(&self) -> ScoreOrder {
        self.score_order
    }

    /// Reports whether this node or any descendant executable leaf has a filter.
    #[must_use]
    pub fn has_filter_in_subtree(&self) -> bool {
        self.filter.is_some()
            || match &self.kind {
                QueryKind::Prefetch { branches, .. } => {
                    branches.iter().any(Self::has_filter_in_subtree)
                }
                QueryKind::Weighted { query, .. }
                | QueryKind::ScoreThreshold { query, .. }
                | QueryKind::Formula { query, .. }
                | QueryKind::Rerank { query }
                | QueryKind::ExternalRerank { query, .. }
                | QueryKind::TopologyExpand { query, .. } => query.has_filter_in_subtree(),
                QueryKind::Nearest { .. }
                | QueryKind::SparseNearest { .. }
                | QueryKind::Lexical { .. }
                | QueryKind::Fuzzy { .. }
                | QueryKind::LateInteraction { .. }
                | QueryKind::Recommend { .. }
                | QueryKind::Discover { .. }
                | QueryKind::Lookup { .. } => false,
            }
    }

    /// Returns the largest result limit requested by any node in this tree.
    #[must_use]
    pub fn max_node_limit(&self) -> usize {
        let child_maximum = match &self.kind {
            QueryKind::Prefetch { branches, .. } => branches
                .iter()
                .map(Self::max_node_limit)
                .max()
                .unwrap_or_default(),
            QueryKind::Weighted { query, .. }
            | QueryKind::ScoreThreshold { query, .. }
            | QueryKind::Formula { query, .. }
            | QueryKind::Rerank { query }
            | QueryKind::ExternalRerank { query, .. }
            | QueryKind::TopologyExpand { query, .. } => query.max_node_limit(),
            QueryKind::Nearest { .. }
            | QueryKind::SparseNearest { .. }
            | QueryKind::Lexical { .. }
            | QueryKind::Fuzzy { .. }
            | QueryKind::LateInteraction { .. }
            | QueryKind::Recommend { .. }
            | QueryKind::Discover { .. }
            | QueryKind::Lookup { .. } => 0,
        };
        self.limit().max(child_maximum)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        let mut nodes = 0;
        validate_query(self, 1, &mut nodes)
    }
}

fn validate_query(query: &QueryIr, depth: usize, nodes: &mut usize) -> Result<()> {
    if depth > MAX_QUERY_DEPTH {
        return Err(invalid("query", "exceeds maximum nesting depth"));
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_QUERY_NODES {
        return Err(invalid("query", "exceeds maximum node count"));
    }
    if query.filter.is_some()
        && matches!(
            query.kind,
            QueryKind::Prefetch { .. }
                | QueryKind::Weighted { .. }
                | QueryKind::ScoreThreshold { .. }
                | QueryKind::Formula { .. }
                | QueryKind::Rerank { .. }
                | QueryKind::ExternalRerank { .. }
                | QueryKind::TopologyExpand { .. }
        )
    {
        return Err(invalid(
            "filter",
            "must be attached to executable leaf branches",
        ));
    }
    let expected_order = match &query.kind {
        QueryKind::Lexical { .. }
        | QueryKind::Fuzzy { .. }
        | QueryKind::LateInteraction { .. }
        | QueryKind::Discover { .. }
        | QueryKind::Lookup { .. }
        | QueryKind::Prefetch { .. }
        | QueryKind::Formula { .. }
        | QueryKind::ExternalRerank { .. }
        | QueryKind::TopologyExpand { .. } => Some(ScoreOrder::HigherIsBetter),
        QueryKind::Recommend { .. } => Some(ScoreOrder::LowerIsBetter),
        QueryKind::Weighted { query: child, .. }
        | QueryKind::ScoreThreshold { query: child, .. }
        | QueryKind::Rerank { query: child } => Some(child.score_order()),
        QueryKind::Nearest { .. } | QueryKind::SparseNearest { .. } => None,
    };
    if expected_order.is_some_and(|expected| query.score_order != expected) {
        return Err(invalid(
            "score_order",
            "does not match the query kind's canonical score ordering",
        ));
    }
    validate_kind(&query.kind, depth, nodes)
}

fn validate_kind(kind: &QueryKind, depth: usize, nodes: &mut usize) -> Result<()> {
    match kind {
        QueryKind::Nearest { .. } | QueryKind::SparseNearest { .. } => {}
        QueryKind::Lexical { query, .. } => query.validate()?,
        QueryKind::Fuzzy { .. } => {}
        QueryKind::LateInteraction {
            vectors,
            candidates_per_query,
        } => validate_late_interaction_vectors(vectors, candidates_per_query.get())?,
        QueryKind::Recommend { positive, negative } => {
            if positive.is_empty() {
                return Err(invalid("positive", "must contain at least one point"));
            }
            if positive.len().saturating_add(negative.len()) > MAX_RECALL_CHECK_POINT_IDS {
                return Err(invalid("recommend", "point examples exceed policy maximum"));
            }
        }
        QueryKind::Discover { context } => {
            if context.is_empty() {
                return Err(invalid("context", "must contain at least one point"));
            }
            if context.len() > MAX_RECALL_CHECK_POINT_IDS {
                return Err(invalid("context", "point examples exceed policy maximum"));
            }
        }
        QueryKind::Lookup { point_ids } => {
            if point_ids.is_empty() {
                return Err(invalid("point_ids", "must contain at least one point"));
            }
            if point_ids.len() > MAX_RECALL_CHECK_POINT_IDS {
                return Err(invalid("point_ids", "point list exceeds policy maximum"));
            }
            if point_ids.iter().copied().collect::<BTreeSet<_>>().len() != point_ids.len() {
                return Err(invalid("point_ids", "must not contain duplicate points"));
            }
        }
        QueryKind::Prefetch { branches, fusion } => {
            if branches.is_empty() {
                return Err(invalid("branches", "must contain at least one query"));
            }
            if fusion.rank_constant() == 0 {
                return Err(invalid("rank_constant", "must be positive"));
            }
            if matches!(fusion, Fusion::WeightedRrf { .. })
                && branches
                    .iter()
                    .any(|branch| !matches!(branch.kind(), QueryKind::Weighted { .. }))
            {
                return Err(invalid(
                    "branches",
                    "weighted RRF requires every branch to carry an explicit weight",
                ));
            }
            if matches!(fusion, Fusion::WeightedRrf { .. }) {
                let total_weight = branches.iter().fold(0.0, |total, branch| {
                    let QueryKind::Weighted { weight, .. } = branch.kind() else {
                        return total;
                    };
                    total + weight
                });
                if !total_weight.is_finite() || total_weight <= 0.0 {
                    return Err(invalid(
                        "branches",
                        "weighted RRF branch weights must have a finite positive sum",
                    ));
                }
            }
            if matches!(fusion, Fusion::Rrf { .. })
                && branches
                    .iter()
                    .any(|branch| matches!(branch.kind(), QueryKind::Weighted { .. }))
            {
                return Err(invalid(
                    "branches",
                    "weighted branches require the weighted RRF fusion policy",
                ));
            }
            for branch in branches {
                validate_query(branch, depth.saturating_add(1), nodes)?;
            }
        }
        QueryKind::Weighted { query, weight } => {
            validate_query(query, depth.saturating_add(1), nodes)?;
            if !weight.is_finite() || *weight < 0.0 {
                return Err(invalid("weight", "must be finite and non-negative"));
            }
        }
        QueryKind::ScoreThreshold {
            query,
            minimum,
            maximum,
        } => {
            validate_query(query, depth.saturating_add(1), nodes)?;
            if minimum.is_some_and(|value| !value.is_finite())
                || maximum.is_some_and(|value| !value.is_finite())
            {
                return Err(invalid("score_threshold", "bounds must be finite"));
            }
            if let (Some(minimum), Some(maximum)) = (minimum, maximum)
                && minimum > maximum
            {
                return Err(invalid("score_threshold", "minimum exceeds maximum"));
            }
        }
        QueryKind::Formula { query, .. } => {
            validate_query(query, depth.saturating_add(1), nodes)?;
        }
        QueryKind::Rerank { query } => {
            validate_query(query, depth.saturating_add(1), nodes)?;
        }
        QueryKind::ExternalRerank {
            query,
            model_revision,
        } => {
            validate_query(query, depth.saturating_add(1), nodes)?;
            if *model_revision == 0 {
                return Err(invalid("model_revision", "must be positive"));
            }
        }
        QueryKind::TopologyExpand { query, max_depth } => {
            validate_query(query, depth.saturating_add(1), nodes)?;
            if *max_depth == 0 || *max_depth > MAX_QUERY_DEPTH {
                return Err(invalid(
                    "max_depth",
                    "must be within the global query-depth bound",
                ));
            }
        }
    }
    Ok(())
}

fn validate_late_interaction_raw_shape(
    vectors: &[Vec<f32>],
    candidates_per_query: usize,
) -> Result<()> {
    if vectors.len() > MAX_RECALL_CHECK_POINT_IDS {
        return Err(invalid(
            "query_vectors",
            "vector list exceeds policy maximum",
        ));
    }
    let scalar_cells = vectors.iter().try_fold(0_usize, |total, vector| {
        total
            .checked_add(vector.len())
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "late_interaction_scalar_cell_projection",
            })
    })?;
    if scalar_cells > MAX_LATE_INTERACTION_SCALAR_CELLS {
        return Err(invalid(
            "query_vectors",
            "total scalar cells exceed policy maximum",
        ));
    }
    LateInteractionWork::new(
        vectors.len(),
        candidates_per_query,
        MAX_LATE_INTERACTION_COMPARISONS,
    )?;
    Ok(())
}

fn validate_late_interaction_vectors(
    vectors: &[DenseVector],
    candidates_per_query: usize,
) -> Result<()> {
    if vectors.is_empty() {
        return Err(invalid("query_vectors", "must contain at least one vector"));
    }
    if vectors.len() > MAX_RECALL_CHECK_POINT_IDS {
        return Err(invalid(
            "query_vectors",
            "vector list exceeds policy maximum",
        ));
    }
    let dimensions = vectors[0].dimension();
    let scalar_cells =
        vectors
            .len()
            .checked_mul(dimensions)
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "late_interaction_scalar_cell_projection",
            })?;
    if scalar_cells > MAX_LATE_INTERACTION_SCALAR_CELLS {
        return Err(invalid(
            "query_vectors",
            "total scalar cells exceed policy maximum",
        ));
    }
    if vectors
        .iter()
        .any(|vector| vector.dimension() != dimensions)
    {
        return Err(invalid(
            "query_vectors",
            "all vectors must have the same dimensions",
        ));
    }
    LateInteractionWork::new(
        vectors.len(),
        candidates_per_query,
        MAX_LATE_INTERACTION_COMPARISONS,
    )?;
    Ok(())
}

fn invalid(field: &'static str, reason: &'static str) -> QueryError {
    QueryError::InvalidInput {
        field,
        reason: reason.to_owned(),
    }
}

fn parse_filter(filter: Option<JsonValue>) -> Result<Option<Filter>> {
    filter
        .map(|filter| {
            let mut nodes = 0;
            let mut scalar_bytes = 0;
            validate_filter_value(&filter, 1, &mut nodes, &mut scalar_bytes)?;
            let encoded =
                serde_json::to_string(&filter).map_err(|error| QueryError::InvalidInput {
                    field: "filter",
                    reason: error.to_string(),
                })?;
            parse_filter_json(&encoded).map_err(|error| QueryError::InvalidInput {
                field: "filter",
                reason: error.to_string(),
            })
        })
        .transpose()
}

fn validate_filter_value(
    value: &JsonValue,
    depth: usize,
    nodes: &mut usize,
    scalar_bytes: &mut usize,
) -> Result<()> {
    if depth > MAX_FILTER_DEPTH {
        return Err(invalid("filter", "exceeds maximum nesting depth"));
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_FILTER_NODES {
        return Err(invalid("filter", "exceeds maximum node count"));
    }
    match value {
        JsonValue::Array(values) => {
            for value in values {
                validate_filter_value(value, depth.saturating_add(1), nodes, scalar_bytes)?;
            }
        }
        JsonValue::Object(values) => {
            for (key, value) in values {
                *scalar_bytes = scalar_bytes.saturating_add(key.len());
                validate_filter_value(value, depth.saturating_add(1), nodes, scalar_bytes)?;
            }
        }
        JsonValue::String(value) => {
            *scalar_bytes = scalar_bytes.saturating_add(value.len());
        }
        JsonValue::Number(value) => {
            *scalar_bytes = scalar_bytes.saturating_add(value.to_string().len());
        }
        JsonValue::Bool(_) | JsonValue::Null => {}
    }
    if *scalar_bytes > MAX_FILTER_SCALAR_BYTES {
        return Err(invalid("filter", "scalar bytes exceed policy maximum"));
    }
    Ok(())
}
