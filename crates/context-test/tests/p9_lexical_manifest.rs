//! Phase 9 lexical-retrieval manifest contract.

#![allow(clippy::expect_used)]

use context_test::{
    P9_INDEX_STRATEGIES, P9_LEXICAL_GATES, P9_MAX_FIELDS, P9_MAX_HEADLINE_OPTIONS_BYTES,
    P9_MAX_HEADLINE_OUTPUT_BYTES, P9_MAX_HEADLINE_POINTS, P9_MAX_HEADLINE_SOURCE_BYTES,
    P9_MAX_JSON_PATH_DEPTH, P9_MAX_QUERY_BYTES, P9_MAX_QUERY_NODES, P9_QUERY_FORMS, P9_RANKERS,
    p9_lexical_manifest_hash,
};

#[test]
fn p9_manifest_freezes_query_hydration_and_scale_gates() {
    assert_eq!(P9_MAX_FIELDS, 16);
    assert_eq!(P9_MAX_JSON_PATH_DEPTH, 16);
    assert_eq!(P9_MAX_QUERY_BYTES, 4_096);
    assert_eq!(P9_MAX_QUERY_NODES, 256);
    assert_eq!(P9_MAX_HEADLINE_POINTS, 1_000);
    assert_eq!(P9_MAX_HEADLINE_SOURCE_BYTES, 8 * 1024 * 1024);
    assert_eq!(P9_MAX_HEADLINE_OUTPUT_BYTES, 2 * 1024 * 1024);
    assert_eq!(P9_MAX_HEADLINE_OPTIONS_BYTES, 4_096);
    assert_eq!(P9_QUERY_FORMS.len(), 9);
    assert_eq!(P9_RANKERS, ["ts_rank", "ts_rank_cd"]);
    assert_eq!(P9_INDEX_STRATEGIES.len(), 6);
    assert_eq!(
        P9_LEXICAL_GATES.map(|gate| gate.rows),
        [1_000_000, 10_000_000]
    );
    assert!(P9_LEXICAL_GATES.iter().all(|gate| {
        gate.minimum_recall >= 0.99
            && gate.minimum_ndcg >= 0.98
            && gate.minimum_mrr >= 0.98
            && gate.maximum_candidates == 10_000
    }));
    assert_ne!(p9_lexical_manifest_hash(), 0);
}

#[test]
fn p9_manifest_matches_the_installed_lexical_contract() {
    assert_eq!(P9_MAX_FIELDS, context_query::MAX_LEXICAL_FIELDS);
    assert_eq!(
        P9_MAX_JSON_PATH_DEPTH,
        context_query::MAX_LEXICAL_JSON_PATH_DEPTH
    );
    assert_eq!(P9_MAX_QUERY_BYTES, context_query::MAX_LEXICAL_TEXT_BYTES);
    assert_eq!(P9_MAX_QUERY_NODES, context_query::MAX_LEXICAL_QUERY_NODES);
    assert_eq!(
        P9_MAX_HEADLINE_POINTS,
        context_query::MAX_LEXICAL_HEADLINE_POINTS
    );
    assert_eq!(
        P9_MAX_HEADLINE_SOURCE_BYTES,
        context_query::MAX_LEXICAL_HEADLINE_SOURCE_BYTES
    );
    assert_eq!(
        P9_MAX_HEADLINE_OUTPUT_BYTES,
        context_query::MAX_LEXICAL_HEADLINE_OUTPUT_BYTES
    );
    assert_eq!(
        P9_MAX_HEADLINE_OPTIONS_BYTES,
        context_query::MAX_LEXICAL_HEADLINE_OPTIONS_BYTES
    );

    let forms = [
        context_query::LexicalQuery::Plain(lexical_text()),
        context_query::LexicalQuery::Structured(lexical_text()),
        context_query::LexicalQuery::Phrase(lexical_text()),
        context_query::LexicalQuery::WebSearch(lexical_text()),
        context_query::LexicalQuery::Prefix(
            context_query::LexicalPrefixTerm::new("term").expect("prefix term"),
        ),
        context_query::LexicalQuery::Distance {
            left: lexical_text(),
            right: lexical_text(),
            distance: 1,
        },
        context_query::LexicalQuery::Boolean {
            operator: context_query::LexicalBooleanOperator::Not,
            clauses: vec![context_query::LexicalQuery::Plain(lexical_text())],
        },
        context_query::LexicalQuery::WeightRestricted {
            query: Box::new(context_query::LexicalQuery::Plain(lexical_text())),
            weights: context_query::LexicalWeightSet::new(&[context_query::LexicalWeight::A])
                .expect("weight set"),
        },
        context_query::LexicalQuery::RegisteredTsQuery(
            context_query::RegisteredTsQueryName::new("saved").expect("registered name"),
        ),
    ];
    assert_eq!(
        forms.map(|form| form.form_name()).to_vec(),
        P9_QUERY_FORMS.to_vec()
    );
    assert_eq!(
        [
            context_query::LexicalRanker::TsRank.stable_name(),
            context_query::LexicalRanker::TsRankCd.stable_name(),
        ],
        P9_RANKERS
    );
}

fn lexical_text() -> context_query::LexicalText {
    context_query::LexicalText::new("postgres").expect("bounded lexical text")
}
