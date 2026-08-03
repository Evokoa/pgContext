//! Phase 8 composite-executor manifest contract.

use context_core::policy::{
    MAX_QUERY_EXPANSIONS, MAX_QUERY_STAGES, MAX_RECALL_CHECK_POINT_IDS, MAX_SEARCH_LIMIT,
};
use context_query::{
    DEFAULT_QUERY_COMPARISONS, DEFAULT_QUERY_ELAPSED_MICROS, DEFAULT_QUERY_HYDRATION_BYTES,
    DEFAULT_QUERY_MEMORY_BYTES, MAX_QUERY_DEPTH, MAX_QUERY_NODES,
};
use context_test::{
    P8_DEFAULT_COMPARISONS, P8_DEFAULT_ELAPSED_MICROS, P8_DEFAULT_HYDRATION_BYTES,
    P8_DEFAULT_MEMORY_BYTES, P8_MAX_CANDIDATES, P8_MAX_COMPARISONS, P8_MAX_ELAPSED_MICROS,
    P8_MAX_EXPANSIONS, P8_MAX_HYDRATION_BYTES, P8_MAX_MEMORY_BYTES, P8_MAX_QUERY_DEPTH,
    P8_MAX_QUERY_NODES, P8_MAX_RESULTS, P8_MAX_STAGES, P8_STAGE_KINDS, p8_composite_manifest_hash,
};

#[test]
fn p8_manifest_freezes_global_executor_limits_and_stage_registry() {
    assert_eq!(P8_MAX_QUERY_DEPTH, 32);
    assert_eq!(P8_MAX_QUERY_DEPTH, MAX_QUERY_DEPTH);
    assert_eq!(P8_MAX_QUERY_NODES, 256);
    assert_eq!(P8_MAX_QUERY_NODES, MAX_QUERY_NODES);
    assert_eq!(P8_MAX_STAGES, 256);
    assert_eq!(P8_MAX_STAGES, MAX_QUERY_STAGES);
    assert_eq!(P8_MAX_CANDIDATES, 10_000);
    assert_eq!(P8_MAX_CANDIDATES, MAX_RECALL_CHECK_POINT_IDS);
    assert_eq!(P8_DEFAULT_COMPARISONS, 1_000_000);
    assert_eq!(P8_MAX_COMPARISONS, 10_000_000);
    assert_eq!(P8_MAX_EXPANSIONS, 64);
    assert_eq!(P8_MAX_EXPANSIONS, MAX_QUERY_EXPANSIONS);
    assert_eq!(P8_DEFAULT_MEMORY_BYTES, 16 * 1024 * 1024);
    assert_eq!(P8_MAX_MEMORY_BYTES, 256 * 1024 * 1024);
    assert_eq!(P8_DEFAULT_HYDRATION_BYTES, 8 * 1024 * 1024);
    assert_eq!(P8_MAX_HYDRATION_BYTES, 64 * 1024 * 1024);
    assert_eq!(P8_DEFAULT_ELAPSED_MICROS, 500_000);
    assert_eq!(P8_MAX_ELAPSED_MICROS, 60_000_000);
    assert_eq!(P8_MAX_RESULTS, 10_000);
    assert_eq!(P8_MAX_RESULTS, MAX_SEARCH_LIMIT);
    assert_eq!(P8_DEFAULT_COMPARISONS, DEFAULT_QUERY_COMPARISONS);
    assert_eq!(P8_DEFAULT_MEMORY_BYTES, DEFAULT_QUERY_MEMORY_BYTES);
    assert_eq!(P8_DEFAULT_HYDRATION_BYTES, DEFAULT_QUERY_HYDRATION_BYTES);
    assert_eq!(P8_DEFAULT_ELAPSED_MICROS, DEFAULT_QUERY_ELAPSED_MICROS);
    assert_eq!(
        P8_STAGE_KINDS,
        [
            "readiness",
            "filter_candidates",
            "candidates",
            "source_recheck",
            "topology_expansion",
            "fusion",
            "score_transform",
            "external_rerank",
            "rerank",
        ]
    );
    assert_ne!(p8_composite_manifest_hash(), 0);
}

#[test]
fn p8_composite_manifest_hash_is_stable() {
    assert_eq!(p8_composite_manifest_hash(), 0x0344_820f_df23_6e70);
}
