//! Phase 12 semantic-rerank certification manifest contract.

use context_query::{
    MAX_RERANK_CANDIDATES, MAX_RERANK_QUERY_BYTES, MAX_RERANK_REQUEST_BYTES, MAX_RERANK_TEXT_BYTES,
};

use context_test::{
    P12_ADAPTER, P12_ARTIFACT_BYTES, P12_ARTIFACT_SHA256, P12_BREAKER_COOLDOWN_MICROS,
    P12_BREAKER_FAILURES, P12_CANDIDATE_COUNT, P12_COLD_START_MICROS, P12_DATASET_REVISION,
    P12_DISTRIBUTION, P12_FAILURE_CONTRACT, P12_HELD_OUT_QUERY_COUNT, P12_INPUT_CONTRACT,
    P12_LICENSE_SPDX, P12_MAX_COST_MICRODOLLARS_PER_1K, P12_MAX_DOCUMENT_TOKENS,
    P12_MAX_ELAPSED_MICROS, P12_MAX_QUERY_TOKENS, P12_MAX_REQUEST_BYTES, P12_MAX_RETRIES,
    P12_MAX_RSS_BYTES, P12_MAX_WIRE_BYTES, P12_MIN_NDCG_LIFT, P12_MODEL_NAME, P12_MODEL_NUMBER,
    P12_MODEL_REVISION, P12_OUTPUT_CONTRACT, P12_P50_MICROS, P12_P95_MICROS, P12_REPORT_MARKERS,
    P12_REQUIRED_DATASET_ROWS, P12_REQUIRED_DATASET_SHA256, P12_REQUIRED_PG_MAJORS,
    P12_REQUIRED_PLATFORMS, P12_SCORE_CONTRACT, P12_SEMANTIC_RERANK_GATES, P12_TOKENIZER_REVISION,
    P12_TOP_K, P12_WORKLOAD_REVISION, P12_WORKLOAD_SHA256, p12_semantic_rerank_manifest_hash,
};

const _: () = assert!(P12_CANDIDATE_COUNT <= MAX_RERANK_CANDIDATES);

#[test]
fn p12_manifest_freezes_quality_latency_cost_and_resilience_before_measurement() {
    assert_eq!(P12_HELD_OUT_QUERY_COUNT, 8);
    assert_eq!(P12_CANDIDATE_COUNT, 32);
    assert_eq!(P12_TOP_K, 10);
    assert_eq!(P12_MIN_NDCG_LIFT, 0.05);
    assert_eq!((P12_P50_MICROS, P12_P95_MICROS), (25_000, 75_000));
    assert_eq!(P12_COLD_START_MICROS, 250_000);
    assert_eq!(P12_MAX_RSS_BYTES, 128 * 1024 * 1024);
    assert_eq!(P12_MAX_COST_MICRODOLLARS_PER_1K, 0);
    assert_eq!(P12_MAX_RETRIES, 1);
    assert_eq!(P12_BREAKER_FAILURES, 3);
    assert_eq!(P12_BREAKER_COOLDOWN_MICROS, 5_000_000);
    assert_eq!(P12_DATASET_REVISION, "p12-held-out-rerank-v1");
    assert_eq!(P12_WORKLOAD_REVISION, "p12-eight-query-judgments-v2");
    assert_eq!(P12_REQUIRED_DATASET_ROWS, 1_000_000);
    assert_eq!(P12_REQUIRED_DATASET_SHA256.len(), 64);
    assert_eq!(P12_WORKLOAD_SHA256.len(), 64);
    assert_eq!(P12_MODEL_REVISION, "linear_pair_v1-operator-artifact-v1");
    assert_eq!(P12_REQUIRED_PG_MAJORS, [17, 18]);
    assert_eq!(P12_REQUIRED_PLATFORMS.len(), 4);
    assert_eq!(P12_REPORT_MARKERS.len(), 5);
}

#[test]
fn p12_manifest_freezes_the_worker_wire_artifact_and_token_contract() {
    assert_eq!(P12_ADAPTER, "linear_pair_v1");
    assert_eq!((P12_MODEL_NAME, P12_MODEL_NUMBER), ("linear-pair-v1", 7));
    assert_eq!(P12_TOKENIZER_REVISION, "ascii_tokens_v1");
    assert_eq!(P12_INPUT_CONTRACT, "rerank_envelope_v3");
    assert_eq!(P12_OUTPUT_CONTRACT, "rerank_response_v3");
    assert_eq!(P12_FAILURE_CONTRACT, "rerank_failure_v1");
    assert_eq!(P12_SCORE_CONTRACT, "higher_is_better_unit_interval");
    assert_eq!(P12_MAX_REQUEST_BYTES, MAX_RERANK_REQUEST_BYTES);
    assert_eq!(P12_MAX_WIRE_BYTES, context_query::MAX_RERANK_WIRE_BYTES);
    assert_eq!((P12_MAX_QUERY_TOKENS, P12_MAX_DOCUMENT_TOKENS), (8, 16));
    assert_eq!(P12_MAX_ELAPSED_MICROS, P12_P95_MICROS);
    assert_eq!(P12_ARTIFACT_BYTES, 40);
    assert_eq!(P12_ARTIFACT_SHA256.len(), 64);
    assert_eq!(P12_LICENSE_SPDX, "Apache-2.0");
    assert_eq!(P12_DISTRIBUTION, "operator_provided_only");
}

#[test]
fn p12_manifest_is_inside_the_query_owned_envelope_bounds() {
    assert_eq!(MAX_RERANK_QUERY_BYTES, 64 * 1024);
    assert_eq!(MAX_RERANK_TEXT_BYTES, 32 * 1024);
    assert_eq!(MAX_RERANK_REQUEST_BYTES, 4 * 1024 * 1024);
}

#[test]
fn p12_manifest_freezes_required_1m_and_scheduled_10m_commands() {
    assert_eq!(
        P12_SEMANTIC_RERANK_GATES.map(|gate| gate.rows),
        [1_000_000, 10_000_000]
    );
    assert_eq!(P12_SEMANTIC_RERANK_GATES[0].status, "required_pg17_pg18");
    assert_eq!(
        P12_SEMANTIC_RERANK_GATES[1].status,
        "scheduled_release_scale"
    );
    for gate in P12_SEMANTIC_RERANK_GATES {
        assert!(gate.command.contains("PG_VERSION=pg{major}"));
        assert!(gate.command.contains("PG_FEATURE=pg{major}"));
        assert!(gate.command.contains("PGPORT={port}"));
        assert!(gate.command.ends_with("semantic_rerank_contract.sh"));
        assert_eq!(gate.report_marker, "semantic_rerank_quality");
    }
}

#[test]
fn p12_manifest_hash_detects_any_certification_contract_drift() {
    assert_eq!(p12_semantic_rerank_manifest_hash(), 0x363f_ff47_3bfe_a786);
}
