//! Phase 15 lazy-HNSW certification manifest contract.

use context_query::LazyCursorTermination;
use context_test::*;

#[test]
fn p15_manifest_freezes_cursor_operations_and_terminations() {
    assert_eq!(
        P15_CURSOR_OPERATIONS,
        ["seed", "peek", "pop", "advance", "finish", "exhausted"]
    );
    let terminations = [
        LazyCursorTermination::Exhausted,
        LazyCursorTermination::Cancelled,
        LazyCursorTermination::ComparisonBudget,
        LazyCursorTermination::ExpansionBudget,
        LazyCursorTermination::EdgeBudget,
        LazyCursorTermination::MemoryBudget,
        LazyCursorTermination::AdapterError,
    ];
    assert_eq!(
        terminations.map(LazyCursorTermination::stable_name),
        P15_TERMINATIONS
    );
}

#[test]
fn p15_manifest_freezes_resource_and_regression_bounds() {
    assert_eq!(P15_DEFAULT_ADVANCE_BATCH, 32);
    assert_eq!(P15_MAX_ADVANCE_BATCH, 256);
    assert_eq!(
        P15_MAX_ADVANCE_BATCH,
        context_index::MAX_HNSW_CURSOR_ADVANCE
    );
    assert_eq!(P15_MAX_COMPARISONS, 10_000_000);
    assert_eq!(P15_MAX_NODE_EXPANSIONS, 10_000_000);
    assert_eq!(P15_MAX_EDGES, 10_000_000);
    assert_eq!(P15_DEFAULT_MEMORY_BYTES, 16 * 1024 * 1024);
    assert_eq!(P15_MAX_MEMORY_BYTES, 256 * 1024 * 1024);
    assert_eq!(P15_MAX_RSS_BYTES, 256 * 1024 * 1024);
    assert_eq!(P15_TIMING_REPEATS, 11);
    assert_eq!(
        P15_METRICS,
        [
            "l2",
            "negative_inner_product",
            "cosine",
            "l1",
            "hamming",
            "jaccard"
        ]
    );
    assert_eq!(
        P15_GRAPH_SOURCES,
        [
            "page",
            "mapped_full_precision",
            "mapped_quantized",
            "segmented",
            "delta_overlay"
        ]
    );
    assert_eq!(P15_MAX_P50_LATENCY_RATIO_BPS, 11_000);
    assert_eq!(P15_MAX_RETAINED_MEMORY_RATIO_BPS, 10_500);
}

#[test]
fn p15_manifest_freezes_differential_identity_and_hash() {
    assert_eq!(P15_DATASET_SHA256.len(), 64);
    assert_eq!(P15_WORKLOAD_SHA256.len(), 64);
    assert_eq!(P15_REQUIRED_PG_MAJORS, [17, 18]);
    assert_eq!(P15_PRE_P15_ORACLE_FNV64, 0x9518_1835_e370_3256);
    assert_eq!(P15_REPORT_MARKERS.len(), 11);
    assert_eq!(p15_lazy_hnsw_manifest_hash(), 0xa6ea_b0b1_0f88_9282);
}
