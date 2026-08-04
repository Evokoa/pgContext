//! Strict JSON query-plan decoding.

use context_core::{
    PointId, SearchLimit, SparseVector,
    policy::{MAX_RECALL_CHECK_POINT_IDS, MAX_VECTOR_DIMENSIONS},
};
use serde_json::{Map, Value};

use crate::{
    Formula, Fusion, FuzzyMode, FuzzyQuery, FuzzySourceName, FuzzyThreshold, LateInteractionWork,
    LexicalQuery, LexicalSourceName, LexicalText, MAX_LATE_INTERACTION_COMPARISONS,
    MAX_LATE_INTERACTION_SCALAR_CELLS, MAX_QUERY_DEPTH, MAX_QUERY_NODES, QueryError, QueryIr,
    QueryKind, Result, ScoreOrder,
};

/// Parses an untrusted JSON value into the validated query IR.
///
/// Unknown fields are rejected so plan typos cannot silently change query
/// semantics. Recursive depth and node counts are bounded before IR creation.
///
/// # Errors
///
/// Returns [`QueryError::InvalidInput`] for malformed, unsupported, or
/// semantically invalid plans.
pub fn parse_query_plan(plan: &Value) -> Result<QueryIr> {
    let mut nodes = 0;
    parse_query_node(plan, 1, &mut nodes)
}

fn parse_query_node(plan: &Value, depth: usize, nodes: &mut usize) -> Result<QueryIr> {
    if depth > MAX_QUERY_DEPTH {
        return Err(invalid_plan("plan exceeds maximum nesting depth"));
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_QUERY_NODES {
        return Err(invalid_plan("plan exceeds maximum node count"));
    }
    let object = plan
        .as_object()
        .ok_or_else(|| invalid_plan("each query node must be an object"))?;
    match string_field(object, "kind")? {
        "nearest" => parse_nearest(object),
        "sparse_nearest" => parse_sparse_nearest(object),
        "lexical" => parse_lexical(object),
        "fuzzy" => parse_fuzzy(object),
        "late_interaction" => parse_late_interaction(object),
        "recommend" => {
            require_keys(
                object,
                &["kind", "positive_point_ids", "negative_point_ids", "limit"],
            )?;
            QueryIr::new(
                QueryKind::Recommend {
                    positive: point_ids(object, "positive_point_ids")?,
                    negative: point_ids(object, "negative_point_ids")?,
                },
                ScoreOrder::LowerIsBetter,
                None,
                limit_field(object)?,
            )
        }
        "discover" => {
            require_keys(object, &["kind", "context_point_ids", "limit"])?;
            QueryIr::new(
                QueryKind::Discover {
                    context: point_ids(object, "context_point_ids")?,
                },
                ScoreOrder::HigherIsBetter,
                None,
                limit_field(object)?,
            )
        }
        "lookup" => {
            require_keys(object, &["kind", "point_ids"])?;
            let point_ids = point_ids(object, "point_ids")?;
            QueryIr::new(
                QueryKind::Lookup {
                    point_ids: point_ids.clone(),
                },
                ScoreOrder::HigherIsBetter,
                None,
                point_ids.len(),
            )
        }
        "prefetch" => parse_prefetch(object, depth, nodes),
        "weight" => {
            require_keys(object, &["kind", "weight", "branch"])?;
            let branch = child(object, depth, nodes)?;
            let limit = branch.limit();
            let order = branch.score_order();
            QueryIr::new(
                QueryKind::Weighted {
                    query: Box::new(branch),
                    weight: finite_number(object, "weight")?,
                },
                order,
                None,
                limit,
            )
        }
        "score_threshold" => {
            require_keys(object, &["kind", "min_score", "max_score", "branch"])?;
            let branch = child(object, depth, nodes)?;
            let limit = branch.limit();
            let order = branch.score_order();
            QueryIr::new(
                QueryKind::ScoreThreshold {
                    query: Box::new(branch),
                    minimum: optional_finite_number(object, "min_score")?,
                    maximum: optional_finite_number(object, "max_score")?,
                },
                order,
                None,
                limit,
            )
        }
        "formula" => {
            require_keys(object, &["kind", "formula", "branch"])?;
            let branch = child(object, depth, nodes)?;
            let limit = branch.limit();
            QueryIr::new(
                QueryKind::Formula {
                    query: Box::new(branch),
                    formula: Formula::new(string_field(object, "formula")?)?,
                },
                ScoreOrder::HigherIsBetter,
                None,
                limit,
            )
        }
        "rerank" => {
            require_keys(object, &["kind", "limit", "branch"])?;
            let branch = child(object, depth, nodes)?;
            let order = branch.score_order();
            QueryIr::new(
                QueryKind::Rerank {
                    query: Box::new(branch),
                },
                order,
                None,
                limit_field(object)?,
            )
        }
        "external_rerank" => {
            require_keys(object, &["kind", "limit", "model_revision", "branch"])?;
            let branch = child(object, depth, nodes)?;
            QueryIr::new(
                QueryKind::ExternalRerank {
                    query: Box::new(branch),
                    model_revision: positive_u64_field(object, "model_revision")?,
                },
                ScoreOrder::HigherIsBetter,
                None,
                limit_field(object)?,
            )
        }
        "topology_expand" => {
            require_keys(object, &["kind", "limit", "max_depth", "branch"])?;
            let branch = child(object, depth, nodes)?;
            QueryIr::new(
                QueryKind::TopologyExpand {
                    query: Box::new(branch),
                    max_depth: positive_usize_field(object, "max_depth")?,
                },
                ScoreOrder::HigherIsBetter,
                None,
                limit_field(object)?,
            )
        }
        _ => Err(invalid_plan("unsupported query kind")),
    }
}

fn parse_nearest(object: &Map<String, Value>) -> Result<QueryIr> {
    require_keys(
        object,
        &["kind", "vector_name", "vector", "filter", "limit"],
    )?;
    QueryIr::nearest(
        optional_string(object, "vector_name")?,
        f32_array(object, "vector")?,
        ScoreOrder::LowerIsBetter,
        optional_value(object, "filter"),
        limit_field(object)?,
    )
}

fn parse_sparse_nearest(object: &Map<String, Value>) -> Result<QueryIr> {
    require_keys(
        object,
        &["kind", "vector_name", "vector", "filter", "limit"],
    )?;
    let vector = string_field(object, "vector")?
        .parse::<SparseVector>()
        .map_err(QueryError::from)?;
    QueryIr::sparse_nearest(
        string_field(object, "vector_name")?.to_owned(),
        vector,
        ScoreOrder::LowerIsBetter,
        optional_value(object, "filter"),
        limit_field(object)?,
    )
}

fn parse_lexical(object: &Map<String, Value>) -> Result<QueryIr> {
    require_keys(object, &["kind", "source", "query", "filter", "limit"])?;
    let query = object
        .get("query")
        .ok_or_else(|| invalid_field("query", "is required"))?;
    QueryIr::lexical(
        LexicalSourceName::new(string_field(object, "source")?)?,
        LexicalQuery::from_json(query)?,
        optional_value(object, "filter"),
        limit_field(object)?,
    )
}

fn parse_fuzzy(object: &Map<String, Value>) -> Result<QueryIr> {
    require_keys(
        object,
        &[
            "kind",
            "source",
            "query",
            "mode",
            "threshold",
            "filter",
            "limit",
        ],
    )?;
    let query = FuzzyQuery::new(
        LexicalText::new(string_field(object, "query")?)?,
        FuzzyMode::parse(string_field(object, "mode")?)?,
        FuzzyThreshold::new(finite_number(object, "threshold")?)?,
    );
    QueryIr::fuzzy(
        FuzzySourceName::new(string_field(object, "source")?)?,
        query,
        optional_value(object, "filter"),
        limit_field(object)?,
    )
}

fn parse_late_interaction(object: &Map<String, Value>) -> Result<QueryIr> {
    require_keys(
        object,
        &["kind", "query_vectors", "candidates_per_query", "limit"],
    )?;
    let values = object
        .get("query_vectors")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_field("query_vectors", "must be an array"))?;
    if values.is_empty() {
        return Err(invalid_field(
            "query_vectors",
            "must contain at least one vector",
        ));
    }
    if values.len() > MAX_RECALL_CHECK_POINT_IDS {
        return Err(invalid_field(
            "query_vectors",
            "vector list exceeds policy maximum",
        ));
    }
    let candidates_per_query =
        SearchLimit::new(positive_usize_field(object, "candidates_per_query")?)?;
    LateInteractionWork::new(
        values.len(),
        candidates_per_query.get(),
        MAX_LATE_INTERACTION_COMPARISONS,
    )?;
    let mut scalar_cells = 0_usize;
    for value in values {
        let vector_values = value
            .as_array()
            .ok_or_else(|| invalid_field("query_vectors", "must contain vector arrays"))?;
        validate_f32_array_length(vector_values, "query_vectors")?;
        scalar_cells = scalar_cells.checked_add(vector_values.len()).ok_or(
            QueryError::ArithmeticOverflow {
                operation: "late_interaction_scalar_cell_projection",
            },
        )?;
        if scalar_cells > MAX_LATE_INTERACTION_SCALAR_CELLS {
            return Err(invalid_field(
                "query_vectors",
                "total scalar cells exceed policy maximum",
            ));
        }
    }
    let vectors = values
        .iter()
        .map(|value| f32_value_array(value, "query_vectors"))
        .collect::<Result<Vec<_>>>()?;
    QueryIr::late_interaction(vectors, candidates_per_query.get(), limit_field(object)?)
}

fn parse_prefetch(object: &Map<String, Value>, depth: usize, nodes: &mut usize) -> Result<QueryIr> {
    require_keys(object, &["kind", "branches", "fusion", "rank_constant"])?;
    let values = object
        .get("branches")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_plan("branches must be an array"))?;
    let branches = values
        .iter()
        .map(|branch| parse_query_node(branch, depth.saturating_add(1), nodes))
        .collect::<Result<Vec<_>>>()?;
    let limit = branches
        .iter()
        .map(QueryIr::limit)
        .max()
        .unwrap_or_default();
    let rank_constant = match object.get("rank_constant") {
        Some(_) => positive_u32_field(object, "rank_constant")?,
        None => 60,
    };
    let fusion = match object.get("fusion").and_then(Value::as_str) {
        None | Some("rrf") => Fusion::Rrf { rank_constant },
        Some("weighted_rrf") => Fusion::WeightedRrf { rank_constant },
        Some(_) => return Err(invalid_field("fusion", "must be rrf or weighted_rrf")),
    };
    QueryIr::new(
        QueryKind::Prefetch { branches, fusion },
        ScoreOrder::HigherIsBetter,
        None,
        limit,
    )
}

fn positive_u32_field(object: &Map<String, Value>, field: &'static str) -> Result<u32> {
    u32::try_from(positive_usize_field(object, field)?)
        .map_err(|_| invalid_field(field, "must fit in a positive u32"))
}

fn positive_u64_field(object: &Map<String, Value>, field: &'static str) -> Result<u64> {
    let value = object
        .get(field)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid_field(field, "must be a positive integer"))?;
    Ok(value)
}

fn child(object: &Map<String, Value>, depth: usize, nodes: &mut usize) -> Result<QueryIr> {
    parse_query_node(
        object
            .get("branch")
            .ok_or_else(|| invalid_plan("missing branch"))?,
        depth.saturating_add(1),
        nodes,
    )
}

fn require_keys(object: &Map<String, Value>, allowed: &[&str]) -> Result<()> {
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid_plan("query node contains an unknown field"));
    }
    Ok(())
}

fn string_field<'a>(object: &'a Map<String, Value>, field: &'static str) -> Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_field(field, "must be a string"))
}

fn optional_string(object: &Map<String, Value>, field: &'static str) -> Result<Option<String>> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid_field(field, "must be a string or null")),
    }
}

fn optional_value(object: &Map<String, Value>, field: &'static str) -> Option<Value> {
    object.get(field).filter(|value| !value.is_null()).cloned()
}

fn limit_field(object: &Map<String, Value>) -> Result<usize> {
    positive_usize_field(object, "limit")
}

fn positive_usize_field(object: &Map<String, Value>, field: &'static str) -> Result<usize> {
    let value = object
        .get(field)
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .ok_or_else(|| invalid_field(field, "must be a positive integer"))?;
    usize::try_from(value).map_err(|_| invalid_field(field, "must be a positive integer"))
}

fn f32_array(object: &Map<String, Value>, field: &'static str) -> Result<Vec<f32>> {
    let value = object
        .get(field)
        .ok_or_else(|| invalid_field(field, "must be an array"))?;
    f32_value_array(value, field)
}

fn f32_value_array(value: &Value, field: &'static str) -> Result<Vec<f32>> {
    let values = value
        .as_array()
        .ok_or_else(|| invalid_field(field, "must be an array"))?;
    validate_f32_array_length(values, field)?;
    values
        .iter()
        .map(|value| {
            let narrowed = value
                .to_string()
                .parse::<f32>()
                .map_err(|_| invalid_field(field, "must contain finite f32 values"))?;
            if narrowed.is_finite() {
                Ok(narrowed)
            } else {
                Err(invalid_field(field, "must contain finite f32 values"))
            }
        })
        .collect()
}

fn validate_f32_array_length(values: &[Value], field: &'static str) -> Result<()> {
    if values.len() > MAX_VECTOR_DIMENSIONS {
        return Err(invalid_field(
            field,
            "vector dimensions exceed policy maximum",
        ));
    }
    Ok(())
}

fn point_ids(object: &Map<String, Value>, field: &'static str) -> Result<Vec<PointId>> {
    let values = object
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_field(field, "must be an array"))?;
    if values.len() > MAX_RECALL_CHECK_POINT_IDS {
        return Err(invalid_field(field, "point list exceeds policy maximum"));
    }
    values
        .iter()
        .map(|value| {
            let value = value
                .as_i64()
                .filter(|value| *value > 0)
                .ok_or_else(|| invalid_field(field, "must contain positive integers"))?;
            Ok(PointId::new(u64::try_from(value).map_err(|_| {
                invalid_field(field, "must contain positive integers")
            })?))
        })
        .collect()
}

fn finite_number(object: &Map<String, Value>, field: &'static str) -> Result<f64> {
    let value = object
        .get(field)
        .and_then(Value::as_f64)
        .ok_or_else(|| invalid_field(field, "must be a finite number"))?;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(invalid_field(field, "must be a finite number"))
    }
}

fn optional_finite_number(object: &Map<String, Value>, field: &'static str) -> Result<Option<f64>> {
    match object.get(field) {
        Some(Value::Null) => Ok(None),
        Some(_) => finite_number(object, field).map(Some),
        None => Err(invalid_field(field, "is required")),
    }
}

fn invalid_plan(reason: &'static str) -> QueryError {
    invalid_field("plan", reason)
}

fn invalid_field(field: &'static str, reason: &'static str) -> QueryError {
    QueryError::InvalidInput {
        field,
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use context_core::policy::{MAX_RECALL_CHECK_POINT_IDS, MAX_VECTOR_DIMENSIONS};
    use serde_json::json;

    #[test]
    fn nearest_accepts_named_vector_and_filter() -> Result<()> {
        let query = parse_query_plan(&json!({
            "kind": "nearest",
            "vector_name": "title",
            "vector": [1.0, 0.0],
            "filter": {"must": [{"key": "tenant", "match": {"value": "a"}}]},
            "limit": 4
        }))?;
        assert!(query.filter().is_some());
        assert!(matches!(
            query.kind(),
            QueryKind::Nearest {
                vector_name: Some(_),
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn recommendation_and_rerank_preserve_lower_is_better_order() -> Result<()> {
        let query = parse_query_plan(&json!({
            "kind": "rerank",
            "limit": 2,
            "branch": {
                "kind": "recommend",
                "positive_point_ids": [1],
                "negative_point_ids": [],
                "limit": 4
            }
        }))?;
        assert_eq!(query.score_order(), ScoreOrder::LowerIsBetter);
        Ok(())
    }

    #[test]
    fn untrusted_arrays_are_rejected_at_policy_bounds() {
        let oversized_vector = vec![Value::from(0.0); MAX_VECTOR_DIMENSIONS.saturating_add(1)];
        assert!(matches!(
            parse_query_plan(&json!({
                "kind": "nearest",
                "vector_name": null,
                "vector": oversized_vector,
                "filter": null,
                "limit": 1
            })),
            Err(QueryError::InvalidInput {
                field: "vector",
                ..
            })
        ));

        let oversized_point_ids = (1..=MAX_RECALL_CHECK_POINT_IDS.saturating_add(1))
            .map(Value::from)
            .collect::<Vec<_>>();
        assert!(matches!(
            parse_query_plan(&json!({
                "kind": "lookup",
                "point_ids": oversized_point_ids
            })),
            Err(QueryError::InvalidInput {
                field: "point_ids",
                ..
            })
        ));
    }

    #[test]
    fn lookup_rejects_duplicate_point_ids_during_json_validation() {
        assert!(matches!(
            parse_query_plan(&json!({
                "kind": "lookup",
                "point_ids": [7, 7]
            })),
            Err(QueryError::InvalidInput {
                field: "point_ids",
                ..
            })
        ));
    }

    #[test]
    fn late_interaction_rejects_oversized_vector_lists_before_conversion() {
        let query_vectors = vec![json!([1.0]); MAX_RECALL_CHECK_POINT_IDS.saturating_add(1)];
        assert!(matches!(
            parse_query_plan(&json!({
                "kind": "late_interaction",
                "query_vectors": query_vectors,
                "candidates_per_query": 1,
                "limit": 1
            })),
            Err(QueryError::InvalidInput {
                field: "query_vectors",
                ..
            })
        ));
    }

    #[test]
    fn late_interaction_rejects_oversized_work_before_vector_conversion() {
        let query_vectors = vec![json!([1.0]); 101];
        assert!(matches!(
            parse_query_plan(&json!({
                "kind": "late_interaction",
                "query_vectors": query_vectors,
                "candidates_per_query": 10_000,
                "limit": 1
            })),
            Err(QueryError::WorkBudgetExceeded {
                budget: "late_interaction_comparisons",
                ..
            })
        ));
    }

    #[test]
    fn late_interaction_rejects_oversized_scalar_cells_before_conversion() {
        let vector = vec![Value::from(1.0); MAX_VECTOR_DIMENSIONS];
        let vector_count = MAX_LATE_INTERACTION_SCALAR_CELLS
            .checked_div(MAX_VECTOR_DIMENSIONS)
            .unwrap_or_default()
            .saturating_add(1);
        let query_vectors = vec![Value::Array(vector); vector_count];
        assert!(matches!(
            parse_query_plan(&json!({
                "kind": "late_interaction",
                "query_vectors": query_vectors,
                "candidates_per_query": 1,
                "limit": 1
            })),
            Err(QueryError::InvalidInput {
                field: "query_vectors",
                ..
            })
        ));
    }

    #[test]
    fn prefetch_parses_parameterized_rank_only_fusion() -> Result<()> {
        let leaf = json!({
            "kind": "nearest",
            "vector_name": null,
            "vector": [1.0, 0.0],
            "filter": null,
            "limit": 2
        });
        let query = parse_query_plan(&json!({
            "kind": "prefetch",
            "fusion": "rrf",
            "rank_constant": 17,
            "branches": [leaf]
        }))?;
        assert!(matches!(
            query.kind(),
            QueryKind::Prefetch {
                fusion: Fusion::Rrf { rank_constant: 17 },
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn weighted_rrf_rejects_unweighted_branches_and_zero_rank_constant() {
        let leaf = json!({
            "kind": "nearest",
            "vector_name": null,
            "vector": [1.0, 0.0],
            "filter": null,
            "limit": 2
        });
        assert!(
            parse_query_plan(&json!({
                "kind": "prefetch",
                "fusion": "weighted_rrf",
                "rank_constant": 17,
                "branches": [leaf.clone()]
            }))
            .is_err()
        );
        assert!(
            parse_query_plan(&json!({
                "kind": "prefetch",
                "fusion": "rrf",
                "rank_constant": 0,
                "branches": [leaf]
            }))
            .is_err()
        );
    }
}
