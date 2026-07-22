//! Validation tests for query IR, budgets, and owned port DTOs.

#![allow(clippy::expect_used)]

use context_core::{
    PointId, SparseEntry, SparseVector,
    policy::{MAX_HNSW_CANDIDATE_MASK_POINTS, MAX_RECALL_CHECK_POINT_IDS},
};
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
fn query_ir_requires_filters_on_executable_leaf_branches() {
    let nearest = QueryIr::nearest(None, vec![1.0, 0.0], ScoreOrder::HigherIsBetter, None, 2)
        .expect("nearest query should be valid");
    assert!(matches!(
        QueryIr::new(
            QueryKind::Rerank {
                query: Box::new(nearest),
            },
            ScoreOrder::HigherIsBetter,
            Some(serde_json::json!({
                "must": [{"key": "tenant", "match": {"value": "acme"}}]
            })),
            2,
        ),
        Err(QueryError::InvalidInput {
            field: "filter",
            ..
        })
    ));
}

#[test]
fn prefetch_requires_higher_is_better_fusion_order() {
    let branch = QueryIr::nearest(None, vec![1.0, 0.0], ScoreOrder::HigherIsBetter, None, 2)
        .expect("nearest query should be valid");
    assert!(matches!(
        QueryIr::new(
            QueryKind::Prefetch {
                branches: vec![branch],
            },
            ScoreOrder::LowerIsBetter,
            None,
            2,
        ),
        Err(QueryError::InvalidInput {
            field: "score_order",
            ..
        })
    ));
}

#[test]
fn composite_tree_reports_the_largest_descendant_limit() {
    let branch = QueryIr::nearest(None, vec![1.0, 0.0], ScoreOrder::HigherIsBetter, None, 8)
        .expect("nearest query should be valid");
    let query = QueryIr::new(
        QueryKind::Rerank {
            query: Box::new(branch),
        },
        ScoreOrder::HigherIsBetter,
        None,
        2,
    )
    .expect("rerank query should be valid");

    assert_eq!(query.max_node_limit(), 8);
}

#[test]
fn named_source_leaves_validate_full_text_and_late_interaction_inputs() {
    let full_text = QueryIr::full_text("body".to_owned(), "rust postgres".to_owned(), 5)
        .expect("full-text leaf should be valid");
    assert!(matches!(full_text.kind(), QueryKind::FullText { .. }));

    let late = QueryIr::late_interaction(vec![vec![1.0, 0.0], vec![0.0, 1.0]], 8, 3)
        .expect("late-interaction leaf should be valid");
    assert!(matches!(late.kind(), QueryKind::LateInteraction { .. }));
    assert!(QueryIr::full_text("body;drop".to_owned(), "query".to_owned(), 1).is_err());
    assert!(QueryIr::late_interaction(vec![vec![1.0], vec![1.0, 2.0]], 2, 1).is_err());
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
fn sparse_nearest_ir_preserves_named_sparse_vector() {
    let vector = SparseVector::new(
        8,
        vec![
            SparseEntry::new(1, 0.5).expect("entry should be valid"),
            SparseEntry::new(6, 1.0).expect("entry should be valid"),
        ],
    )
    .expect("sparse vector should be valid");
    let query = QueryIr::sparse_nearest(
        "keywords".to_owned(),
        vector.clone(),
        ScoreOrder::LowerIsBetter,
        None,
        3,
    )
    .expect("sparse nearest query should be valid");
    assert!(matches!(
        query.kind(),
        QueryKind::SparseNearest { vector: stored, .. } if stored == &vector
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
        ExecutionBudget::new(
            1,
            MAX_HNSW_CANDIDATE_MASK_POINTS.saturating_add(1),
            1,
            1,
            1,
            1,
        ),
        Err(QueryError::InvalidInput {
            field: "max_filter_candidates",
            ..
        })
    ));
    assert!(
        ExecutionBudget::new(1, MAX_HNSW_CANDIDATE_MASK_POINTS, 1, 1, 1, 1).is_ok(),
        "the query budget must admit the configured HNSW mask ceiling"
    );
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
