//! SQL constructors for client-side query plans.

use context_core::{PointId, SparseVector};
use context_query::{Formula, QueryError, QueryIr, QueryKind, QueryPlanValidator, ScoreOrder};
use pgrx::JsonB;
use pgrx::prelude::*;
use serde_json::{Map, Value, json};

use crate::error::raise_query_error;
use crate::vector::Vector;
use crate::vector_variants::SparseVec;

#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_nearest(vector: Vector, limit: i32) -> JsonB {
    JsonB(json!({
        "kind": "nearest",
        "vector": vector_values(vector),
        "limit": query_limit(limit),
    }))
}

#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_sparse_nearest(vector_name: String, vector: SparseVec, limit: i32) -> JsonB {
    let vector = vector
        .to_sparse()
        .unwrap_or_else(|error| crate::error::raise_core_error(error));
    JsonB(json!({
        "kind": "sparse_nearest",
        "vector_name": vector_name,
        "vector": vector.to_string(),
        "limit": query_limit(limit),
    }))
}

#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_full_text(text_query: String, text_column: String, limit: i32) -> JsonB {
    let node = QueryIr::full_text(text_column, text_query, query_limit_usize(limit))
        .unwrap_or_else(|error| raise_query_error(error));
    let QueryKind::FullText { text_column, query } = node.kind() else {
        unreachable!("full-text constructor must produce a full-text node")
    };
    JsonB(json!({
        "kind": "full_text",
        "text_query": query,
        "text_column": text_column,
        "limit": node.limit(),
    }))
}

#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_late_interaction(
    query_vectors: Vec<Vector>,
    candidates_per_query: i32,
    limit: i32,
) -> JsonB {
    let vectors = query_vectors
        .into_iter()
        .map(vector_values)
        .collect::<Vec<_>>();
    let query = QueryIr::late_interaction(
        vectors.clone(),
        query_limit_usize(candidates_per_query),
        query_limit_usize(limit),
    )
    .unwrap_or_else(|error| raise_query_error(error));
    let QueryKind::LateInteraction {
        candidates_per_query,
        ..
    } = query.kind()
    else {
        unreachable!("late-interaction constructor must produce a late-interaction node")
    };
    JsonB(json!({
        "kind": "late_interaction",
        "query_vectors": vectors,
        "candidates_per_query": candidates_per_query.get(),
        "limit": query.limit(),
    }))
}

#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_recommend(
    positive_point_ids: Vec<i64>,
    negative_point_ids: Vec<i64>,
    limit: i32,
) -> JsonB {
    validate_or_raise(QueryPlanValidator::recommend_point_ids(
        &positive_point_ids,
        &negative_point_ids,
    ));
    JsonB(json!({
        "kind": "recommend",
        "positive_point_ids": positive_point_ids,
        "negative_point_ids": negative_point_ids,
        "limit": query_limit(limit),
    }))
}

#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_discover(context_point_ids: Vec<i64>, limit: i32) -> JsonB {
    validate_or_raise(QueryPlanValidator::discover_point_ids(&context_point_ids));
    JsonB(json!({
        "kind": "discover",
        "context_point_ids": context_point_ids,
        "limit": query_limit(limit),
    }))
}

#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_lookup(point_ids: Vec<i64>) -> JsonB {
    validate_or_raise(QueryPlanValidator::lookup_point_ids(&point_ids));
    JsonB(json!({
        "kind": "lookup",
        "point_ids": point_ids,
    }))
}

#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_prefetch(branches: Vec<JsonB>) -> JsonB {
    validate_or_raise(QueryPlanValidator::prefetch_branches(branches.len()));
    JsonB(json!({
        "kind": "prefetch",
        "branches": json_values(branches),
    }))
}

#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_weight(branch: JsonB, weight: f64) -> JsonB {
    validate_or_raise(QueryPlanValidator::weight(weight));
    JsonB(json!({
        "kind": "weight",
        "weight": weight,
        "branch": branch.0,
    }))
}

#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_score_threshold(
    branch: JsonB,
    min_score: Option<f64>,
    max_score: Option<f64>,
) -> JsonB {
    validate_or_raise(QueryPlanValidator::score_threshold(min_score, max_score));
    JsonB(json!({
        "kind": "score_threshold",
        "min_score": min_score,
        "max_score": max_score,
        "branch": branch.0,
    }))
}

#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_formula(branch: JsonB, formula: String) -> JsonB {
    let formula = match Formula::new(formula) {
        Ok(formula) => formula,
        Err(error) => raise_query_error(error),
    };
    JsonB(json!({
        "kind": "formula",
        "formula": formula.into_string(),
        "branch": branch.0,
    }))
}

#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_rerank(branch: JsonB, limit: i32) -> JsonB {
    JsonB(json!({
        "kind": "rerank",
        "limit": query_limit(limit),
        "branch": branch.0,
    }))
}

/// Executes a validated query-constructor plan against a registered collection.
#[pg_extern]
#[search_path(pg_catalog, pgcontext, public)]
pub fn execute_query(
    collection: String,
    plan: JsonB,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f32),
    ),
> {
    let query = parse_query_plan(&plan.0).unwrap_or_else(|error| raise_query_error(error));
    let collection = crate::table_search::collection_name_from_sql(collection);
    TableIterator::new(crate::retrieval::run_query(
        &collection,
        query,
        crate::retrieval::CandidateAdapter::Hnsw,
    ))
}

pub(crate) fn parse_query_plan(plan: &Value) -> context_query::Result<QueryIr> {
    let mut nodes = 0;
    parse_query_node(plan, 1, &mut nodes)
}

fn parse_query_node(
    plan: &Value,
    depth: usize,
    nodes: &mut usize,
) -> context_query::Result<QueryIr> {
    if depth > 32 {
        return Err(invalid_plan("plan exceeds maximum nesting depth"));
    }
    *nodes = nodes.saturating_add(1);
    if *nodes > 256 {
        return Err(invalid_plan("plan exceeds maximum node count"));
    }
    let object = plan
        .as_object()
        .ok_or_else(|| invalid_plan("each query node must be an object"))?;
    let kind = string_field(object, "kind")?;
    match kind {
        "nearest" => {
            require_keys(object, &["kind", "vector", "limit"])?;
            let vector = f32_array(object, "vector")?;
            QueryIr::nearest(
                None,
                vector,
                ScoreOrder::LowerIsBetter,
                None,
                limit_field(object)?,
            )
        }
        "sparse_nearest" => {
            require_keys(object, &["kind", "vector_name", "vector", "limit"])?;
            let vector = string_field(object, "vector")?
                .parse::<SparseVector>()
                .map_err(QueryError::from)?;
            QueryIr::sparse_nearest(
                string_field(object, "vector_name")?.to_owned(),
                vector,
                ScoreOrder::LowerIsBetter,
                None,
                limit_field(object)?,
            )
        }
        "full_text" => {
            require_keys(object, &["kind", "text_query", "text_column", "limit"])?;
            QueryIr::full_text(
                string_field(object, "text_column")?.to_owned(),
                string_field(object, "text_query")?.to_owned(),
                limit_field(object)?,
            )
        }
        "late_interaction" => {
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
                .collect::<context_query::Result<Vec<_>>>()?;
            QueryIr::late_interaction(
                vectors,
                positive_usize_field(object, "candidates_per_query")?,
                limit_field(object)?,
            )
        }
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
                ScoreOrder::HigherIsBetter,
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
        "prefetch" => {
            require_keys(object, &["kind", "branches"])?;
            let values = object
                .get("branches")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid_plan("branches must be an array"))?;
            let branches = values
                .iter()
                .map(|branch| parse_query_node(branch, depth.saturating_add(1), nodes))
                .collect::<context_query::Result<Vec<_>>>()?;
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
            QueryIr::new(
                QueryKind::Rerank {
                    query: Box::new(branch),
                },
                ScoreOrder::HigherIsBetter,
                None,
                limit_field(object)?,
            )
        }
        _ => Err(invalid_plan("unsupported query kind")),
    }
}

fn child(
    object: &Map<String, Value>,
    depth: usize,
    nodes: &mut usize,
) -> context_query::Result<QueryIr> {
    parse_query_node(
        object
            .get("branch")
            .ok_or_else(|| invalid_plan("missing branch"))?,
        depth.saturating_add(1),
        nodes,
    )
}

fn require_keys(object: &Map<String, Value>, allowed: &[&str]) -> context_query::Result<()> {
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid_plan("query node contains an unknown field"));
    }
    Ok(())
}

fn string_field<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
) -> context_query::Result<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_field(field, "must be a string"))
}

fn limit_field(object: &Map<String, Value>) -> context_query::Result<usize> {
    positive_usize_field(object, "limit")
}

fn positive_usize_field(
    object: &Map<String, Value>,
    field: &'static str,
) -> context_query::Result<usize> {
    let limit = object
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| invalid_field(field, "must be a positive integer"))?;
    usize::try_from(limit).map_err(|_| invalid_field(field, "must be a positive integer"))
}

fn f32_array(object: &Map<String, Value>, field: &'static str) -> context_query::Result<Vec<f32>> {
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

fn point_ids(
    object: &Map<String, Value>,
    field: &'static str,
) -> context_query::Result<Vec<PointId>> {
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

fn finite_number(object: &Map<String, Value>, field: &'static str) -> context_query::Result<f64> {
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

fn optional_finite_number(
    object: &Map<String, Value>,
    field: &'static str,
) -> context_query::Result<Option<f64>> {
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

fn vector_values(vector: Vector) -> Vec<f32> {
    match vector.to_dense() {
        Ok(vector) => vector.as_slice().to_vec(),
        Err(error) => crate::error::raise_core_error(error),
    }
}

fn query_limit(limit: i32) -> i32 {
    validate_or_raise(QueryPlanValidator::limit(i64::from(limit)));
    limit
}

fn query_limit_usize(limit: i32) -> usize {
    let limit = query_limit(limit);
    usize::try_from(limit).unwrap_or_else(|_| unreachable!("validated query limit is positive"))
}

fn json_values(values: Vec<JsonB>) -> Vec<Value> {
    values.into_iter().map(|value| value.0).collect()
}

fn validate_or_raise(result: context_query::Result<()>) {
    if let Err(error) = result {
        raise_query_error(error);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn parser_accepts_nested_constructor_shape_and_preserves_branch_limits() {
        let plan = json!({
            "kind": "rerank",
            "limit": 2,
            "branch": {
                "kind": "prefetch",
                "branches": [
                    {"kind": "nearest", "vector": [1.0, 0.0], "limit": 8},
                    {
                        "kind": "weight",
                        "weight": 0.5,
                        "branch": {"kind": "nearest", "vector": [0.0, 1.0], "limit": 4}
                    }
                ]
            }
        });
        let query = parse_query_plan(&plan).expect("constructor plan should parse");

        assert_eq!(query.limit(), 2);
        assert_eq!(query.max_node_limit(), 8);
        assert!(matches!(query.kind(), QueryKind::Rerank { .. }));
    }

    #[test]
    fn parser_rejects_unknown_fields_and_nonpositive_ids() {
        let unknown = json!({
            "kind": "nearest",
            "vector": [1.0],
            "limit": 1,
            "raw_sql": "select 1"
        });
        assert!(matches!(
            parse_query_plan(&unknown),
            Err(QueryError::InvalidInput { field: "plan", .. })
        ));
        let invalid_id = json!({"kind": "lookup", "point_ids": [0]});
        assert!(matches!(
            parse_query_plan(&invalid_id),
            Err(QueryError::InvalidInput {
                field: "point_ids",
                ..
            })
        ));
    }
}
