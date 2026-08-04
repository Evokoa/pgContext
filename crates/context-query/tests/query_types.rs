//! Validation tests for query IR, budgets, and owned port DTOs.

#![allow(clippy::expect_used)]

use context_core::{
    ConfigurationRevision, GenerationId, OccurrenceId, ProfileId, SourceAuthority, SourceVersion,
};
use context_core::{
    DenseVector, PointId, SparseEntry, SparseVector,
    policy::{MAX_HNSW_CANDIDATE_MASK_POINTS, MAX_RECALL_CHECK_POINT_IDS, MAX_VECTOR_DIMENSIONS},
};
use context_query::{
    Candidate, CandidateBranch, CandidateDiagnostics, CandidateProvenance, CandidateSourceKind,
    ExecutionBudget, Formula, Fusion, LexicalQuery, LexicalSourceName, LexicalText,
    MAX_LATE_INTERACTION_SCALAR_CELLS, QueryError, QueryIr, QueryKind, ScoreOrder,
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
fn direct_late_interaction_ir_rejects_oversized_scalar_cells() {
    let vector = DenseVector::new(vec![1.0; MAX_VECTOR_DIMENSIONS]).expect("bounded vector");
    let vector_count = MAX_LATE_INTERACTION_SCALAR_CELLS
        .checked_div(MAX_VECTOR_DIMENSIONS)
        .unwrap_or_default()
        .saturating_add(1);
    assert!(matches!(
        QueryIr::new(
            QueryKind::LateInteraction {
                vectors: vec![vector; vector_count],
                candidates_per_query: context_core::SearchLimit::new(1).expect("one"),
            },
            ScoreOrder::HigherIsBetter,
            None,
            1,
        ),
        Err(QueryError::InvalidInput {
            field: "query_vectors",
            ..
        })
    ));
}

#[test]
fn direct_ir_rejects_contradictory_fixed_score_orders() {
    let lexical = QueryIr::new(
        QueryKind::Lexical {
            source: LexicalSourceName::new("body").expect("lexical source"),
            query: LexicalQuery::Plain(LexicalText::new("rust").expect("lexical text")),
        },
        ScoreOrder::LowerIsBetter,
        None,
        1,
    );
    assert!(matches!(
        lexical,
        Err(QueryError::InvalidInput {
            field: "score_order",
            ..
        })
    ));

    let child = QueryIr::nearest(None, vec![1.0, 0.0], ScoreOrder::LowerIsBetter, None, 1)
        .expect("nearest child");
    let wrapper = QueryIr::new(
        QueryKind::Rerank {
            query: Box::new(child),
        },
        ScoreOrder::HigherIsBetter,
        None,
        1,
    );
    assert!(matches!(
        wrapper,
        Err(QueryError::InvalidInput {
            field: "score_order",
            ..
        })
    ));
}

#[test]
fn direct_ir_rejects_duplicate_lookup_points() {
    let lookup = QueryIr::new(
        QueryKind::Lookup {
            point_ids: vec![PointId::new(7), PointId::new(7)],
        },
        ScoreOrder::HigherIsBetter,
        None,
        2,
    );
    assert!(matches!(
        lookup,
        Err(QueryError::InvalidInput {
            field: "point_ids",
            ..
        })
    ));
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
                fusion: Fusion::STANDARD_RRF,
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
fn named_source_leaves_validate_lexical_and_late_interaction_inputs() {
    let lexical = QueryIr::lexical(
        LexicalSourceName::new("body").expect("lexical source"),
        LexicalQuery::Plain(LexicalText::new("rust postgres").expect("lexical text")),
        None,
        5,
    )
    .expect("lexical leaf should be valid");
    assert!(matches!(lexical.kind(), QueryKind::Lexical { .. }));

    let late = QueryIr::late_interaction(vec![vec![1.0, 0.0], vec![0.0, 1.0]], 8, 3)
        .expect("late-interaction leaf should be valid");
    assert!(matches!(late.kind(), QueryKind::LateInteraction { .. }));
    assert!(LexicalSourceName::new("body;drop").is_err());
    assert!(QueryIr::late_interaction(vec![vec![1.0], vec![1.0, 2.0]], 2, 1).is_err());
}

#[test]
fn candidate_scores_must_be_finite() {
    let provenance = CandidateProvenance::new(
        OccurrenceId::new(1).expect("non-zero occurrence"),
        CandidateBranch::DenseAnn,
        CandidateSourceKind::Hnsw,
        ScoreOrder::LowerIsBetter,
        SourceAuthority::DerivedArtifact,
    )
    .with_generation(GenerationId::new(3).expect("non-zero generation"))
    .with_configuration(ConfigurationRevision::new(4).expect("non-zero configuration"))
    .with_profile(ProfileId::new(5).expect("non-zero profile"));
    assert!(matches!(
        Candidate::new(PointId::new(1), f64::NAN, provenance),
        Err(QueryError::InvalidInput {
            field: "candidate_score",
            ..
        })
    ));
}

#[test]
fn candidate_branch_and_source_registries_are_exhaustive() {
    let branches = [
        CandidateBranch::DenseExact,
        CandidateBranch::DenseAnn,
        CandidateBranch::Lexical,
        CandidateBranch::Sparse,
        CandidateBranch::MultiVector,
        CandidateBranch::Quantized,
        CandidateBranch::Recommend,
        CandidateBranch::Discover,
        CandidateBranch::Lookup,
        CandidateBranch::Topology,
        CandidateBranch::UserProvided,
        CandidateBranch::Fuzzy,
    ];
    let sources = [
        CandidateSourceKind::Exact,
        CandidateSourceKind::Hnsw,
        CandidateSourceKind::IvfFlat,
        CandidateSourceKind::Lexical,
        CandidateSourceKind::Sparse,
        CandidateSourceKind::MultiVector,
        CandidateSourceKind::Quantized,
        CandidateSourceKind::Recommendation,
        CandidateSourceKind::Discovery,
        CandidateSourceKind::Lookup,
        CandidateSourceKind::UserProvided,
        CandidateSourceKind::Topology,
        CandidateSourceKind::Fuzzy,
    ];

    assert_eq!(
        branches.map(CandidateBranch::stable_name),
        [
            "dense_exact",
            "dense_ann",
            "lexical",
            "sparse",
            "multi_vector",
            "quantized",
            "recommend",
            "discover",
            "lookup",
            "topology",
            "user_provided",
            "fuzzy",
        ]
    );
    assert_eq!(
        branches.map(CandidateBranch::stable_code),
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]
    );
    assert_eq!(
        sources.map(CandidateSourceKind::stable_name),
        [
            "exact",
            "hnsw",
            "ivf_flat",
            "lexical",
            "sparse",
            "multi_vector",
            "quantized",
            "recommendation",
            "discovery",
            "lookup",
            "user_provided",
            "topology",
            "fuzzy",
        ]
    );
    assert_eq!(
        sources.map(CandidateSourceKind::stable_code),
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
    );
}

#[test]
fn candidate_envelope_preserves_typed_provenance_and_scores() -> Result<(), QueryError> {
    let provenance = CandidateProvenance::new(
        OccurrenceId::new(11).expect("non-zero occurrence"),
        CandidateBranch::DenseAnn,
        CandidateSourceKind::Hnsw,
        ScoreOrder::LowerIsBetter,
        SourceAuthority::DerivedArtifact,
    )
    .with_generation(GenerationId::new(12).expect("non-zero generation"))
    .with_configuration(ConfigurationRevision::new(13).expect("non-zero configuration"))
    .with_profile(ProfileId::new(14).expect("non-zero profile"))
    .with_source_version(SourceVersion::new(15).expect("non-zero source version"));
    let diagnostics = CandidateDiagnostics::new(3, 21);
    let candidate = Candidate::new(PointId::new(7), 0.25, provenance)?
        .with_exact_score(0.2)?
        .with_diagnostics(diagnostics);

    assert_eq!(candidate.approximate_score(), 0.25);
    assert_eq!(candidate.exact_score(), Some(0.2));
    assert_eq!(candidate.provenance().occurrence_id().get(), 11);
    assert_eq!(
        candidate.provenance().generation().map(GenerationId::get),
        Some(12)
    );
    assert_eq!(
        candidate
            .provenance()
            .configuration()
            .map(ConfigurationRevision::get),
        Some(13)
    );
    assert_eq!(
        candidate.provenance().profile().map(ProfileId::get),
        Some(14)
    );
    assert_eq!(
        candidate
            .provenance()
            .source_version()
            .map(SourceVersion::get),
        Some(15)
    );
    assert_eq!(candidate.diagnostics(), diagnostics);
    assert_eq!(candidate.diagnostics().source_rank(), 3);
    assert_eq!(candidate.diagnostics().work_units(), 21);
    Ok(())
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
            ScoreOrder::LowerIsBetter,
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

#[test]
fn weighted_rrf_requires_a_finite_positive_total_weight() {
    let weighted = |weight| {
        QueryIr::new(
            QueryKind::Weighted {
                query: Box::new(
                    QueryIr::nearest(None, vec![1.0, 0.0], ScoreOrder::HigherIsBetter, None, 2)
                        .expect("leaf query"),
                ),
                weight,
            },
            ScoreOrder::HigherIsBetter,
            None,
            2,
        )
        .expect("individual weight")
    };

    for branches in [
        vec![weighted(0.0), weighted(0.0)],
        vec![weighted(f64::MAX), weighted(f64::MAX)],
    ] {
        assert!(matches!(
            QueryIr::new(
                QueryKind::Prefetch {
                    branches,
                    fusion: Fusion::WeightedRrf { rank_constant: 60 },
                },
                ScoreOrder::HigherIsBetter,
                None,
                2,
            ),
            Err(QueryError::InvalidInput {
                field: "branches",
                ..
            })
        ));
    }
}
