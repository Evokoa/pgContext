//! Validated query intermediate representation.

use context_core::policy::{MAX_FILTER_DEPTH, MAX_FILTER_NODES, MAX_RECALL_CHECK_POINT_IDS};
use context_core::{DenseVector, PointId, SearchLimit, VectorName};
use context_filter::{Filter, parse_filter_json};
use serde_json::Value as JsonValue;

use crate::{Formula, QueryError, Result, ScoreOrder};

const MAX_QUERY_DEPTH: usize = 32;
const MAX_QUERY_NODES: usize = 256;
const MAX_FILTER_SCALAR_BYTES: usize = 64 * 1024;

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
    /// Final exact rerank request.
    Rerank {
        /// Owned child query.
        query: Box<QueryIr>,
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
    validate_kind(&query.kind, depth, nodes)
}

fn validate_kind(kind: &QueryKind, depth: usize, nodes: &mut usize) -> Result<()> {
    match kind {
        QueryKind::Nearest { .. } => {}
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
        }
        QueryKind::Prefetch { branches } => {
            if branches.is_empty() {
                return Err(invalid("branches", "must contain at least one query"));
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
    }
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
