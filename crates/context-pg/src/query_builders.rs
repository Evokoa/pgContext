//! SQL constructors for client-side query plans.

use context_query::{Formula, QueryIr, QueryKind, QueryPlanValidator, parse_query_plan};
use pgrx::JsonB;
use pgrx::prelude::*;
use serde_json::{Value, json};

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

/// Builds a dense nearest-neighbor leaf with named-vector and filter selection.
#[pg_extern(name = "query_nearest")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_nearest_configured(
    vector_name: Option<String>,
    vector: Vector,
    filter: Option<JsonB>,
    limit: i32,
) -> JsonB {
    let vector = vector_values(vector);
    let filter = filter.map(|value| value.0);
    QueryIr::nearest(
        vector_name.clone(),
        vector.clone(),
        context_query::ScoreOrder::LowerIsBetter,
        filter.clone(),
        query_limit_usize(limit),
    )
    .unwrap_or_else(|error| raise_query_error(error));
    JsonB(json!({
        "kind": "nearest",
        "vector_name": vector_name,
        "vector": vector,
        "filter": filter,
        "limit": limit,
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

/// Builds a named sparse nearest-neighbor leaf with an optional filter.
#[pg_extern(name = "query_sparse_nearest")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_sparse_nearest_filtered(
    vector_name: String,
    vector: SparseVec,
    filter: Option<JsonB>,
    limit: i32,
) -> JsonB {
    let vector = vector
        .to_sparse()
        .unwrap_or_else(|error| crate::error::raise_core_error(error));
    let filter = filter.map(|value| value.0);
    QueryIr::sparse_nearest(
        vector_name.clone(),
        vector.clone(),
        context_query::ScoreOrder::LowerIsBetter,
        filter.clone(),
        query_limit_usize(limit),
    )
    .unwrap_or_else(|error| raise_query_error(error));
    JsonB(json!({
        "kind": "sparse_nearest",
        "vector_name": vector_name,
        "vector": vector.to_string(),
        "filter": filter,
        "limit": limit,
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
