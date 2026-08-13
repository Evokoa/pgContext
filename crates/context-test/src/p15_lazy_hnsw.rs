//! Frozen Phase 15 lazy-HNSW-cursor certification manifest.

/// Versioned statement-local cursor contract.
pub const P15_CURSOR_CONTRACT: &str = "hnsw_lazy_cursor_v1";
/// Cursor operations whose semantics are frozen by Phase 15.
pub const P15_CURSOR_OPERATIONS: [&str; 6] =
    ["seed", "peek", "pop", "advance", "finish", "exhausted"];
/// Stable terminal reasons shared with the query contract.
pub const P15_TERMINATIONS: [&str; 7] = [
    "exhausted",
    "cancelled",
    "comparison_budget",
    "expansion_budget",
    "edge_budget",
    "memory_budget",
    "adapter_error",
];
/// Distance metrics that must preserve eager/cursor parity.
pub const P15_METRICS: [&str; 6] = [
    "l2",
    "negative_inner_product",
    "cosine",
    "l1",
    "hamming",
    "jaccard",
];
/// Graph-read sources that keep the eager compatibility surface.
pub const P15_GRAPH_SOURCES: [&str; 5] = [
    "page",
    "mapped_full_precision",
    "mapped_quantized",
    "segmented",
    "delta_overlay",
];
/// Default provider expansions per advance.
pub const P15_DEFAULT_ADVANCE_BATCH: usize = context_query::DEFAULT_LAZY_CURSOR_BATCH;
/// Maximum provider expansions per advance.
pub const P15_MAX_ADVANCE_BATCH: usize = context_query::MAX_LAZY_CURSOR_BATCH;
/// Query-wide comparison ceiling inherited from the composite executor.
pub const P15_MAX_COMPARISONS: usize = context_query::MAX_QUERY_COMPARISONS;
/// Cursor-local node-pop ceiling, distinct from P8 provider-call expansions.
pub const P15_MAX_NODE_EXPANSIONS: usize = 10_000_000;
/// Explicit adjacency-entry ceiling for one cursor.
pub const P15_MAX_EDGES: usize = 10_000_000;
/// Default cursor-owned allocation allowance.
pub const P15_DEFAULT_MEMORY_BYTES: usize = context_query::DEFAULT_QUERY_MEMORY_BYTES;
/// Hard cursor-owned allocation allowance.
pub const P15_MAX_MEMORY_BYTES: usize = context_query::MAX_QUERY_MEMORY_BYTES;
/// Maximum resident set size for the standalone certification process.
pub const P15_MAX_RSS_BYTES: usize = 256 * 1024 * 1024;
/// Deterministic differential fixture rows.
pub const P15_FIXTURE_ROWS: usize = 10_000;
/// Vector dimensions in the differential fixture.
pub const P15_FIXTURE_DIMENSIONS: usize = 32;
/// Held-out deterministic queries.
pub const P15_QUERY_COUNT: usize = 128;
/// Alternating measurements summarized into each per-query timing sample.
pub const P15_TIMING_REPEATS: usize = 11;
/// Required result count per held-out query.
pub const P15_TOP_K: usize = 10;
/// Aggregate result, bitwise-score, and work hash from the pre-P15 traversal.
pub const P15_PRE_P15_ORACLE_FNV64: u64 = 0x9518_1835_e370_3256;
/// Maximum cursor/eager p50 latency ratio in basis points.
pub const P15_MAX_P50_LATENCY_RATIO_BPS: u16 = 11_000;
/// Maximum cursor/eager retained-memory ratio in basis points.
pub const P15_MAX_RETAINED_MEMORY_RATIO_BPS: u16 = 10_500;
/// Deterministic differential fixture revision.
pub const P15_DATASET_REVISION: &str = "p15-10k-lazy-hnsw-v1";
/// Canonical fixture specification.
pub const P15_DATASET_SPEC: &str = "p15_fixture_v1|rows=10000|dimensions=32|seed=0x7031355f63757273|tenant=id%16|vectors=splitmix64_f32|topology=bidirectional_chain|queries=128_rotating_rows|ordered=id";
/// SHA-256 of [`P15_DATASET_SPEC`].
pub const P15_DATASET_SHA256: &str =
    "fa376272155f9777cd232d064daf424e1f77aca0f71292411bc7502d63009b34";
/// Differential workload revision.
pub const P15_WORKLOAD_REVISION: &str = "p15-lazy-hnsw-workload-v2";
/// Canonical eager/cursor differential workload.
pub const P15_WORKLOAD_SPEC: &str = "p15_workload_v2|frozen_pre_p15_oracle|seed|peek|pop|advance=1,7,32,256|finish|mask|mapped|page|segment|delta|cancel_before_adapter|comparison_budget|node_expansion_budget|edge_budget|exact_memory_growth|exact_recheck|timing_repeats=11";
/// SHA-256 of [`P15_WORKLOAD_SPEC`].
pub const P15_WORKLOAD_SHA256: &str =
    "8077c5e18becfa1a6e5eff6cf7664a58c2deb88d2edd92c373c5ca923bd1a84d";
/// PostgreSQL majors required for adapter parity.
pub const P15_REQUIRED_PG_MAJORS: [u16; 2] = [17, 18];
/// Required retained report markers.
pub const P15_REPORT_MARKERS: [&str; 11] = [
    "lazy_hnsw_manifest",
    "lazy_hnsw_oracle",
    "lazy_hnsw_equivalence",
    "lazy_hnsw_batch_invariance",
    "lazy_hnsw_budget",
    "lazy_hnsw_standalone_scope",
    "lazy_hnsw_resources",
    "lazy_hnsw_samples",
    "lazy_hnsw_rss",
    "lazy_hnsw_environment",
    "lazy_hnsw_decision",
];

/// Returns a stable FNV-1a identity over every frozen Phase 15 field.
#[must_use]
pub fn p15_lazy_hnsw_manifest_hash() -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for value in [
        P15_DEFAULT_ADVANCE_BATCH,
        P15_MAX_ADVANCE_BATCH,
        P15_MAX_COMPARISONS,
        P15_MAX_NODE_EXPANSIONS,
        P15_MAX_EDGES,
        P15_DEFAULT_MEMORY_BYTES,
        P15_MAX_MEMORY_BYTES,
        P15_MAX_RSS_BYTES,
        P15_FIXTURE_ROWS,
        P15_FIXTURE_DIMENSIONS,
        P15_QUERY_COUNT,
        P15_TIMING_REPEATS,
        P15_TOP_K,
    ] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    hash = fnv1a(hash, &P15_PRE_P15_ORACLE_FNV64.to_le_bytes());
    for value in [
        P15_MAX_P50_LATENCY_RATIO_BPS,
        P15_MAX_RETAINED_MEMORY_RATIO_BPS,
    ] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    for value in [
        P15_CURSOR_CONTRACT,
        P15_DATASET_REVISION,
        P15_DATASET_SPEC,
        P15_DATASET_SHA256,
        P15_WORKLOAD_REVISION,
        P15_WORKLOAD_SPEC,
        P15_WORKLOAD_SHA256,
    ] {
        hash = fnv1a(hash, value.as_bytes());
    }
    for value in P15_CURSOR_OPERATIONS
        .into_iter()
        .chain(P15_TERMINATIONS)
        .chain(P15_METRICS)
        .chain(P15_GRAPH_SOURCES)
        .chain(P15_REPORT_MARKERS)
    {
        hash = fnv1a(hash, value.as_bytes());
    }
    for major in P15_REQUIRED_PG_MAJORS {
        hash = fnv1a(hash, &major.to_le_bytes());
    }
    hash
}

fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}
