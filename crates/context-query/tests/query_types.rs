//! Validation tests for query IR, budgets, and owned port DTOs.

#![allow(clippy::expect_used)]

use context_core::{PointId, policy::MAX_RECALL_CHECK_POINT_IDS};
use context_query::{
    Candidate, CandidateBranch, ExecutionBudget, Formula, QueryError, QueryIr, QueryKind,
    ScoreOrder,
};

#[test]
fn execution_budget_rejects_every_zero_dimension() {
    let budgets = [
        (0, 1, 1, 1, 1, 1),
        (1, 0, 1, 1, 1, 1),
        (1, 1, 0, 1, 1, 1),
        (1, 1, 1, 0, 1, 1),
        (1, 1, 1, 1, 0, 1),
        (1, 1, 1, 1, 1, 0),
    ];
    for (candidates, filters, rechecks, stages, expansions, results) in budgets {
        assert!(matches!(
            ExecutionBudget::new(candidates, filters, rechecks, stages, expansions, results),
            Err(QueryError::InvalidInput { .. })
        ));
    }
}

#[test]
fn query_ir_rejects_invalid_recursive_semantics() {
    let nearest = QueryIr::nearest(None, vec![1.0, 0.0], ScoreOrder::HigherIsBetter, None, 2)
        .expect("nearest query should be valid");
    assert!(matches!(
        QueryIr::new(
            QueryKind::Weighted {
                query: Box::new(nearest.clone()),
                weight: -1.0,
            },
            ScoreOrder::HigherIsBetter,
            None,
            2,
        ),
        Err(QueryError::InvalidInput {
            field: "weight",
            ..
        })
    ));
    assert!(matches!(
        QueryIr::new(
            QueryKind::ScoreThreshold {
                query: Box::new(nearest),
                minimum: Some(2.0),
                maximum: Some(1.0),
            },
            ScoreOrder::HigherIsBetter,
            None,
            2,
        ),
        Err(QueryError::InvalidInput {
            field: "score_threshold",
            ..
        })
    ));
}

#[test]
fn candidate_scores_must_be_finite() {
    assert!(matches!(
        Candidate::new(PointId::new(1), f64::NAN, CandidateBranch::DenseAnn),
        Err(QueryError::InvalidInput {
            field: "candidate_score",
            ..
        })
    ));
}

#[test]
fn query_ir_rejects_unbounded_recursive_depth() {
    let mut query = QueryIr::nearest(None, vec![1.0, 0.0], ScoreOrder::HigherIsBetter, None, 2)
        .expect("nearest query should be valid");
    for _ in 0..31 {
        query = QueryIr::new(
            QueryKind::Weighted {
                query: Box::new(query),
                weight: 1.0,
            },
            ScoreOrder::HigherIsBetter,
            None,
            2,
        )
        .expect("query at or below the depth limit should be valid");
    }

    assert!(matches!(
        QueryIr::new(
            QueryKind::Weighted {
                query: Box::new(query),
                weight: 1.0,
            },
            ScoreOrder::HigherIsBetter,
            None,
            2,
        ),
        Err(QueryError::InvalidInput { field: "query", .. })
    ));
}

#[test]
fn execution_budget_rejects_values_above_policy_ceilings() {
    assert!(matches!(
        ExecutionBudget::new(usize::MAX, 1, 1, 1, 1, 1),
        Err(QueryError::InvalidInput {
            field: "max_candidates",
            ..
        })
    ));
    assert!(matches!(
        ExecutionBudget::new(1, 1, 1, usize::MAX, 1, 1),
        Err(QueryError::InvalidInput {
            field: "max_stages",
            ..
        })
    ));
    assert!(matches!(
        ExecutionBudget::new(1, 1, 1, 1, usize::MAX, 1),
        Err(QueryError::InvalidInput {
            field: "max_expansions",
            ..
        })
    ));
}

#[test]
fn query_ir_rejects_oversized_point_lists_and_filters_before_encoding() {
    let points = (0..=MAX_RECALL_CHECK_POINT_IDS)
        .map(|point_id| PointId::new(point_id as u64))
        .collect::<Vec<_>>();
    assert!(matches!(
        QueryIr::new(
            QueryKind::Recommend {
                positive: points,
                negative: Vec::new(),
            },
            ScoreOrder::HigherIsBetter,
            None,
            2,
        ),
        Err(QueryError::InvalidInput {
            field: "recommend",
            ..
        })
    ));

    let oversized_filter = serde_json::json!({
        "must": [{
            "key": "tenant",
            "match": {"any": (0..300).collect::<Vec<_>>()}
        }]
    });
    assert!(matches!(
        QueryIr::nearest(
            None,
            vec![1.0, 0.0],
            ScoreOrder::HigherIsBetter,
            Some(oversized_filter),
            2,
        ),
        Err(QueryError::InvalidInput {
            field: "filter",
            reason,
        }) if reason == "exceeds maximum node count"
    ));

    let oversized_scalar_filter = serde_json::json!({
        "must": [{
            "key": "tenant",
            "match": {"value": "x".repeat(64 * 1024 + 1)}
        }]
    });
    assert!(matches!(
        QueryIr::nearest(
            None,
            vec![1.0, 0.0],
            ScoreOrder::HigherIsBetter,
            Some(oversized_scalar_filter),
            2,
        ),
        Err(QueryError::InvalidInput {
            field: "filter",
            reason,
        }) if reason == "scalar bytes exceed policy maximum"
    ));
}

#[test]
fn query_ir_owns_ordered_lookup_and_formula_shapes() {
    assert!(matches!(
        QueryIr::new(
            QueryKind::Lookup {
                point_ids: Vec::new(),
            },
            ScoreOrder::HigherIsBetter,
            None,
            2,
        ),
        Err(QueryError::InvalidInput {
            field: "point_ids",
            ..
        })
    ));

    let lookup = QueryIr::new(
        QueryKind::Lookup {
            point_ids: vec![PointId::new(7), PointId::new(8)],
        },
        ScoreOrder::HigherIsBetter,
        None,
        2,
    )
    .expect("ordered lookup should be valid");
    QueryIr::new(
        QueryKind::Formula {
            query: Box::new(lookup),
            formula: Formula::new("$score * 0.5").expect("formula should be valid"),
        },
        ScoreOrder::HigherIsBetter,
        None,
        2,
    )
    .expect("formula query should be valid");
}
