//! Typed contract tests for registered lexical and fuzzy query leaves.

#![allow(clippy::expect_used)]

use context_query::{
    FuzzyMode, FuzzyQuery, FuzzySourceName, FuzzyThreshold, LexicalBooleanOperator,
    LexicalNormalization, LexicalPrefixTerm, LexicalQuery, LexicalRankWeights, LexicalRanker,
    LexicalSourceName, LexicalText, LexicalWeight, LexicalWeightSet, MAX_LEXICAL_BOOLEAN_CLAUSES,
    MAX_LEXICAL_NAME_BYTES, MAX_LEXICAL_NORMALIZATION, MAX_LEXICAL_PHRASE_DISTANCE,
    MAX_LEXICAL_QUERY_DEPTH, MAX_LEXICAL_TEXT_BYTES, QueryError, QueryIr, QueryKind,
    RegisteredTsQueryName, ScoreOrder, parse_query_plan,
};
use serde_json::{Value, json};

fn text(value: &str) -> LexicalText {
    LexicalText::new(value).expect("bounded lexical text")
}

fn source() -> LexicalSourceName {
    LexicalSourceName::new("body").expect("registered lexical source")
}

#[test]
fn registered_identifiers_reject_non_identifier_input() {
    assert!(LexicalSourceName::new("body").is_ok());
    assert!(LexicalSourceName::new("_body_2").is_ok());
    assert!(FuzzySourceName::new("body_trgm").is_ok());
    assert!(RegisteredTsQueryName::new("saved_query").is_ok());

    for rejected in ["", "Body", "body-1", "1body", "body;drop", "body space"] {
        assert!(
            LexicalSourceName::new(rejected).is_err(),
            "identifier {rejected:?} must be rejected"
        );
    }
    assert!(LexicalSourceName::new("a".repeat(MAX_LEXICAL_NAME_BYTES)).is_ok());
    assert!(LexicalSourceName::new("a".repeat(MAX_LEXICAL_NAME_BYTES + 1)).is_err());
}

#[test]
fn lexical_text_enforces_exact_byte_and_content_bounds() {
    assert!(LexicalText::new("a".repeat(MAX_LEXICAL_TEXT_BYTES)).is_ok());
    assert!(LexicalText::new("a".repeat(MAX_LEXICAL_TEXT_BYTES + 1)).is_err());
    assert!(LexicalText::new("").is_err());
    assert!(LexicalText::new("   ").is_err());
    assert!(LexicalText::new("post\0gres").is_err());
    assert!(LexicalText::new("post\u{7}gres").is_err());
    assert!(LexicalText::new("multi\nline").is_ok());
}

#[test]
fn prefix_terms_reject_every_tsquery_metacharacter() {
    assert!(LexicalPrefixTerm::new("postgre").is_ok());
    assert!(LexicalPrefixTerm::new("posts_2").is_ok());
    assert!(LexicalPrefixTerm::new("ünicode").is_ok());
    for rejected in [
        "", "a b", "a&b", "a|b", "a!b", "a:b", "a'b", "a\\b", "a<b", "a(b",
    ] {
        assert!(
            LexicalPrefixTerm::new(rejected).is_err(),
            "prefix term {rejected:?} must be rejected"
        );
    }
}

#[test]
fn boolean_constructors_enforce_native_arity() {
    let clause = LexicalQuery::Plain(text("postgres"));
    assert!(
        LexicalQuery::boolean(LexicalBooleanOperator::And, vec![clause.clone()]).is_err(),
        "and requires at least two clauses"
    );
    assert!(
        LexicalQuery::boolean(LexicalBooleanOperator::Or, vec![clause.clone()]).is_err(),
        "or requires at least two clauses"
    );
    assert!(
        LexicalQuery::boolean(
            LexicalBooleanOperator::Not,
            vec![clause.clone(), clause.clone()]
        )
        .is_err(),
        "not requires exactly one clause"
    );
    assert!(LexicalQuery::boolean(LexicalBooleanOperator::Not, vec![clause.clone()]).is_ok());
    assert!(
        LexicalQuery::boolean(
            LexicalBooleanOperator::And,
            vec![clause.clone(), clause.clone()]
        )
        .is_ok()
    );

    let saturated = vec![clause.clone(); MAX_LEXICAL_BOOLEAN_CLAUSES];
    assert!(LexicalQuery::boolean(LexicalBooleanOperator::Or, saturated).is_ok());
    let oversized = vec![clause; MAX_LEXICAL_BOOLEAN_CLAUSES + 1];
    assert!(LexicalQuery::boolean(LexicalBooleanOperator::Or, oversized).is_err());
}

#[test]
fn distance_queries_enforce_the_native_phrase_bound() {
    assert!(LexicalQuery::distance(text("left"), text("right"), 0).is_err());
    assert!(LexicalQuery::distance(text("left"), text("right"), 1).is_ok());
    assert!(
        LexicalQuery::distance(text("left"), text("right"), MAX_LEXICAL_PHRASE_DISTANCE).is_ok()
    );
    assert!(
        LexicalQuery::distance(text("left"), text("right"), MAX_LEXICAL_PHRASE_DISTANCE + 1)
            .is_err()
    );
}

#[test]
fn weight_restriction_is_only_valid_at_the_query_root() {
    let weights =
        LexicalWeightSet::new(&[LexicalWeight::A, LexicalWeight::B]).expect("nonempty weight set");
    let restricted =
        LexicalQuery::weight_restricted(LexicalQuery::Plain(text("postgres")), weights)
            .expect("root weight restriction");
    assert_eq!(restricted.form_name(), "weight_restricted");

    let nested = LexicalQuery::boolean(
        LexicalBooleanOperator::And,
        vec![restricted, LexicalQuery::Plain(text("rust"))],
    );
    assert!(
        matches!(
            nested,
            Err(QueryError::InvalidInput {
                field: "lexical_weights",
                ..
            })
        ),
        "nested weight restriction must be rejected"
    );
}

#[test]
fn weight_sets_reject_empty_and_duplicated_weights() {
    assert!(LexicalWeightSet::new(&[]).is_err());
    assert!(LexicalWeightSet::new(&[LexicalWeight::A, LexicalWeight::A]).is_err());
    let set = LexicalWeightSet::new(&[LexicalWeight::D, LexicalWeight::A]).expect("weight set");
    assert_eq!(set.weights(), vec![LexicalWeight::A, LexicalWeight::D]);
    assert!(set.contains(LexicalWeight::A));
    assert!(!set.contains(LexicalWeight::B));
}

#[test]
fn lexical_depth_bound_rejects_over_deep_boolean_nesting() {
    let mut query = LexicalQuery::Plain(text("leaf"));
    for _ in 0..MAX_LEXICAL_QUERY_DEPTH {
        query = LexicalQuery::Boolean {
            operator: LexicalBooleanOperator::Not,
            clauses: vec![query],
        };
    }
    assert!(matches!(
        query.validate(),
        Err(QueryError::InvalidInput {
            field: "lexical_query",
            ..
        })
    ));
}

#[test]
fn rankers_normalization_and_rank_weights_validate_registered_values() {
    assert_eq!(LexicalRanker::parse("ts_rank"), Ok(LexicalRanker::TsRank));
    assert_eq!(
        LexicalRanker::parse("ts_rank_cd"),
        Ok(LexicalRanker::TsRankCd)
    );
    assert!(LexicalRanker::parse("bm25").is_err());
    assert_eq!(
        LexicalRanker::TsRankCd.function_name(),
        "pg_catalog.ts_rank_cd"
    );

    assert!(LexicalNormalization::new(MAX_LEXICAL_NORMALIZATION).is_ok());
    assert!(LexicalNormalization::new(MAX_LEXICAL_NORMALIZATION + 1).is_err());
    assert_eq!(LexicalNormalization::NONE.get(), 0);

    assert_eq!(
        LexicalRankWeights::DEFAULT.as_array(),
        [0.1_f32, 0.2, 0.4, 1.0]
    );
    assert!(LexicalRankWeights::new(0.0, 0.5, 0.5, 1.0).is_ok());
    assert!(LexicalRankWeights::new(-0.1, 0.5, 0.5, 1.0).is_err());
    assert!(LexicalRankWeights::new(0.1, 0.5, 0.5, 1.1).is_err());
    assert!(LexicalRankWeights::new(f32::NAN, 0.5, 0.5, 1.0).is_err());
}

#[test]
fn fuzzy_thresholds_and_modes_validate_native_ranges() {
    assert!(FuzzyThreshold::new(0.0).is_err());
    assert!(FuzzyThreshold::new(-0.1).is_err());
    assert!(FuzzyThreshold::new(1.0).is_ok());
    assert!(FuzzyThreshold::new(1.000_001).is_err());
    assert!(FuzzyThreshold::new(f64::NAN).is_err());
    assert!(FuzzyThreshold::new(f64::INFINITY).is_err());

    assert_eq!(FuzzyMode::parse("similarity"), Ok(FuzzyMode::Similarity));
    assert_eq!(
        FuzzyMode::parse("strict_word_similarity"),
        Ok(FuzzyMode::StrictWordSimilarity)
    );
    assert!(FuzzyMode::parse("levenshtein").is_err());
    assert_eq!(
        FuzzyMode::WordSimilarity.threshold_setting(),
        "pg_trgm.word_similarity_threshold"
    );
}

#[test]
fn every_lexical_form_round_trips_through_canonical_json() {
    let weights =
        LexicalWeightSet::new(&[LexicalWeight::A, LexicalWeight::C]).expect("nonempty weight set");
    let forms = [
        LexicalQuery::Plain(text("postgres")),
        LexicalQuery::Structured(text("postgres & rust")),
        LexicalQuery::Phrase(text("postgres rust")),
        LexicalQuery::WebSearch(text("\"postgres rust\" -java")),
        LexicalQuery::Prefix(LexicalPrefixTerm::new("postgr").expect("prefix term")),
        LexicalQuery::distance(text("postgres"), text("rust"), 3).expect("distance"),
        LexicalQuery::boolean(
            LexicalBooleanOperator::And,
            vec![
                LexicalQuery::Plain(text("postgres")),
                LexicalQuery::boolean(
                    LexicalBooleanOperator::Not,
                    vec![LexicalQuery::Plain(text("java"))],
                )
                .expect("not clause"),
            ],
        )
        .expect("and clause"),
        LexicalQuery::weight_restricted(LexicalQuery::Plain(text("postgres")), weights)
            .expect("weight restriction"),
        LexicalQuery::RegisteredTsQuery(
            RegisteredTsQueryName::new("saved_query").expect("registered name"),
        ),
    ];
    let mut names = Vec::with_capacity(forms.len());
    for form in forms {
        names.push(form.form_name());
        let encoded = form.to_json();
        let decoded = LexicalQuery::from_json(&encoded).expect("canonical JSON round trip");
        assert_eq!(decoded, form);
        assert_eq!(decoded.to_json(), encoded);
    }
    assert_eq!(
        names,
        [
            "plain",
            "structured",
            "phrase",
            "web_search",
            "prefix",
            "distance",
            "boolean",
            "weight_restricted",
            "registered_tsquery",
        ]
    );
}

#[test]
fn lexical_json_rejects_unknown_fields_and_unsupported_forms() {
    assert!(LexicalQuery::from_json(&json!({"form": "plain", "text": "x", "extra": 1})).is_err());
    assert!(LexicalQuery::from_json(&json!({"form": "bm25", "text": "x"})).is_err());
    assert!(LexicalQuery::from_json(&json!({"text": "x"})).is_err());
    assert!(LexicalQuery::from_json(&Value::from("plain")).is_err());
    assert!(
        LexicalQuery::from_json(&json!({
            "form": "distance",
            "left": "a",
            "right": "b",
            "distance": 0
        }))
        .is_err()
    );
    assert!(
        LexicalQuery::from_json(&json!({
            "form": "weight_restricted",
            "weights": [],
            "query": {"form": "plain", "text": "x"}
        }))
        .is_err()
    );
    assert!(
        LexicalQuery::from_json(&json!({
            "form": "weight_restricted",
            "weights": ["e"],
            "query": {"form": "plain", "text": "x"}
        }))
        .is_err()
    );
}

#[test]
fn lexical_plan_json_parses_the_canonical_leaf() {
    let plan = json!({
        "kind": "lexical",
        "source": "body",
        "query": {"form": "plain", "text": "postgres"},
        "filter": null,
        "limit": 10
    });
    let query = parse_query_plan(&plan).expect("lexical plan");
    assert_eq!(query.limit(), 10);
    assert_eq!(query.score_order(), ScoreOrder::HigherIsBetter);
    let QueryKind::Lexical {
        source,
        query: lexical,
    } = query.kind()
    else {
        unreachable!("lexical plan must produce a lexical leaf");
    };
    assert_eq!(source.as_str(), "body");
    assert_eq!(lexical, &LexicalQuery::Plain(text("postgres")));
    assert!(query.filter().is_none());
}

#[test]
fn fuzzy_plan_json_parses_the_canonical_leaf() {
    let plan = json!({
        "kind": "fuzzy",
        "source": "body_trgm",
        "query": "postgrs",
        "mode": "similarity",
        "threshold": 0.35,
        "filter": null,
        "limit": 10
    });
    let query = parse_query_plan(&plan).expect("fuzzy plan");
    assert_eq!(query.limit(), 10);
    assert_eq!(query.score_order(), ScoreOrder::HigherIsBetter);
    let QueryKind::Fuzzy {
        source,
        query: fuzzy,
    } = query.kind()
    else {
        unreachable!("fuzzy plan must produce a fuzzy leaf");
    };
    assert_eq!(source.as_str(), "body_trgm");
    assert_eq!(fuzzy.text().as_str(), "postgrs");
    assert_eq!(fuzzy.mode(), FuzzyMode::Similarity);
    assert!((fuzzy.threshold().get() - 0.35).abs() < f64::EPSILON);
}

#[test]
fn lexical_and_fuzzy_leaves_accept_filters_and_reject_unknown_plan_fields() {
    let filtered = parse_query_plan(&json!({
        "kind": "lexical",
        "source": "body",
        "query": {"form": "plain", "text": "postgres"},
        "filter": {"must": [{"key": "tenant", "match": {"value": "a"}}]},
        "limit": 4
    }))
    .expect("filtered lexical plan");
    assert!(filtered.filter().is_some());
    assert!(filtered.has_filter_in_subtree());

    assert!(
        parse_query_plan(&json!({
            "kind": "lexical",
            "source": "body",
            "query": {"form": "plain", "text": "postgres"},
            "filter": null,
            "limit": 4,
            "text_column": "body"
        }))
        .is_err()
    );
    assert!(
        parse_query_plan(&json!({
            "kind": "fuzzy",
            "source": "body_trgm",
            "query": "postgrs",
            "mode": "similarity",
            "threshold": 0.0,
            "filter": null,
            "limit": 4
        }))
        .is_err()
    );
}

#[test]
fn direct_lexical_ir_rejects_contradictory_score_order() {
    assert!(matches!(
        QueryIr::new(
            QueryKind::Lexical {
                source: source(),
                query: LexicalQuery::Plain(text("postgres")),
            },
            ScoreOrder::LowerIsBetter,
            None,
            1,
        ),
        Err(QueryError::InvalidInput {
            field: "score_order",
            ..
        })
    ));
    assert!(matches!(
        QueryIr::new(
            QueryKind::Fuzzy {
                source: FuzzySourceName::new("body_trgm").expect("fuzzy source"),
                query: FuzzyQuery::new(
                    text("postgrs"),
                    FuzzyMode::Similarity,
                    FuzzyThreshold::new(0.3).expect("threshold"),
                ),
            },
            ScoreOrder::LowerIsBetter,
            None,
            1,
        ),
        Err(QueryError::InvalidInput {
            field: "score_order",
            ..
        })
    ));
}

#[test]
fn direct_lexical_ir_revalidates_structurally_invalid_trees() {
    assert!(matches!(
        QueryIr::new(
            QueryKind::Lexical {
                source: source(),
                query: LexicalQuery::Boolean {
                    operator: LexicalBooleanOperator::And,
                    clauses: vec![LexicalQuery::Plain(text("postgres"))],
                },
            },
            ScoreOrder::HigherIsBetter,
            None,
            1,
        ),
        Err(QueryError::InvalidInput {
            field: "lexical_clauses",
            ..
        })
    ));
}
