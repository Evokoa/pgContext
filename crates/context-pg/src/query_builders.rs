//! SQL constructors for client-side query plans.

use context_query::{Formula, QueryPlanValidator};
use pgrx::JsonB;
use pgrx::prelude::*;
use serde_json::{Value, json};

use crate::error::raise_query_error;
use crate::vector::Vector;

#[pg_extern(schema = "pgcontext")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_nearest(vector: Vector, limit: i32) -> JsonB {
    JsonB(json!({
        "kind": "nearest",
        "vector": vector_values(vector),
        "limit": query_limit(limit),
    }))
}

#[pg_extern(schema = "pgcontext")]
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

#[pg_extern(schema = "pgcontext")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_discover(context_point_ids: Vec<i64>, limit: i32) -> JsonB {
    validate_or_raise(QueryPlanValidator::discover_point_ids(&context_point_ids));
    JsonB(json!({
        "kind": "discover",
        "context_point_ids": context_point_ids,
        "limit": query_limit(limit),
    }))
}

#[pg_extern(schema = "pgcontext")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_lookup(point_ids: Vec<i64>) -> JsonB {
    validate_or_raise(QueryPlanValidator::lookup_point_ids(&point_ids));
    JsonB(json!({
        "kind": "lookup",
        "point_ids": point_ids,
    }))
}

#[pg_extern(schema = "pgcontext")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_prefetch(branches: Vec<JsonB>) -> JsonB {
    validate_or_raise(QueryPlanValidator::prefetch_branches(branches.len()));
    JsonB(json!({
        "kind": "prefetch",
        "branches": json_values(branches),
    }))
}

#[pg_extern(schema = "pgcontext")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_weight(branch: JsonB, weight: f64) -> JsonB {
    validate_or_raise(QueryPlanValidator::weight(weight));
    JsonB(json!({
        "kind": "weight",
        "weight": weight,
        "branch": branch.0,
    }))
}

#[pg_extern(schema = "pgcontext")]
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

#[pg_extern(schema = "pgcontext")]
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

#[pg_extern(schema = "pgcontext")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn query_rerank(branch: JsonB, limit: i32) -> JsonB {
    JsonB(json!({
        "kind": "rerank",
        "limit": query_limit(limit),
        "branch": branch.0,
    }))
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

fn json_values(values: Vec<JsonB>) -> Vec<Value> {
    values.into_iter().map(|value| value.0).collect()
}

fn validate_or_raise(result: context_query::Result<()>) {
    if let Err(error) = result {
        raise_query_error(error);
    }
}
