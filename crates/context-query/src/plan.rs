//! Strict JSON query-plan decoding.

use context_core::{PointId, SparseVector};
use serde_json::{Map, Value};

use crate::{Formula, QueryError, QueryIr, QueryKind, Result, ScoreOrder};

const MAX_PLAN_DEPTH: usize = 32;
const MAX_PLAN_NODES: usize = 256;

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
    if depth > MAX_PLAN_DEPTH {
        return Err(invalid_plan("plan exceeds maximum nesting depth"));
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > MAX_PLAN_NODES {
        return Err(invalid_plan("plan exceeds maximum node count"));
    }
    let object = plan
        .as_object()
        .ok_or_else(|| invalid_plan("each query node must be an object"))?;
    match string_field(object, "kind")? {
        "nearest" => parse_nearest(object),
        "sparse_nearest" => parse_sparse_nearest(object),
        "full_text" => {
            require_keys(object, &["kind", "text_query", "text_column", "limit"])?;
            QueryIr::full_text(
                string_field(object, "text_column")?.to_owned(),
                string_field(object, "text_query")?.to_owned(),
                limit_field(object)?,
            )
        }
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

fn parse_late_interaction(object: &Map<String, Value>) -> Result<QueryIr> {
    require_keys(
        object,
        &["kind", "query_vectors", "candidates_per_query", "limit"],
    )?;
    let vectors = object
        .get("query_vectors")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_field("query_vectors", "must be an array"))?
        .iter()
        .map(|value| {
            let object = Map::from_iter([("vector".to_owned(), value.clone())]);
            f32_array(&object, "vector")
        })
        .collect::<Result<Vec<_>>>()?;
    QueryIr::late_interaction(
        vectors,
        positive_usize_field(object, "candidates_per_query")?,
        limit_field(object)?,
    )
}

fn parse_prefetch(object: &Map<String, Value>, depth: usize, nodes: &mut usize) -> Result<QueryIr> {
    require_keys(object, &["kind", "branches"])?;
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
    QueryIr::new(
        QueryKind::Prefetch { branches },
        ScoreOrder::HigherIsBetter,
        None,
        limit,
    )
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
    object
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_field(field, "must be an array"))?
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

fn point_ids(object: &Map<String, Value>, field: &'static str) -> Result<Vec<PointId>> {
    object
        .get(field)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_field(field, "must be an array"))?
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
}
