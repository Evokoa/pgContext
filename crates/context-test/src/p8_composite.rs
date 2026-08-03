//! Frozen Phase 8 composite-executor certification manifest.

/// Maximum validated query depth.
pub const P8_MAX_QUERY_DEPTH: usize = context_query::MAX_QUERY_DEPTH;
/// Maximum validated query nodes.
pub const P8_MAX_QUERY_NODES: usize = context_query::MAX_QUERY_NODES;
/// Maximum stages in one execution.
pub const P8_MAX_STAGES: usize = context_core::policy::MAX_QUERY_STAGES;
/// Maximum materialized candidates or authoritative rechecks.
pub const P8_MAX_CANDIDATES: usize = context_core::policy::MAX_RECALL_CHECK_POINT_IDS;
/// Default score/filter/formula comparison allowance.
pub const P8_DEFAULT_COMPARISONS: usize = context_query::DEFAULT_QUERY_COMPARISONS;
/// Hard maximum score/filter/formula comparison allowance.
pub const P8_MAX_COMPARISONS: usize = context_query::MAX_QUERY_COMPARISONS;
/// Maximum adaptive or topology expansion steps.
pub const P8_MAX_EXPANSIONS: usize = context_core::policy::MAX_QUERY_EXPANSIONS;
/// Default accounted transient-memory allowance.
pub const P8_DEFAULT_MEMORY_BYTES: usize = context_query::DEFAULT_QUERY_MEMORY_BYTES;
/// Hard maximum accounted transient-memory allowance.
pub const P8_MAX_MEMORY_BYTES: usize = context_query::MAX_QUERY_MEMORY_BYTES;
/// Default hydrated source-key byte allowance.
pub const P8_DEFAULT_HYDRATION_BYTES: usize = context_query::DEFAULT_QUERY_HYDRATION_BYTES;
/// Hard maximum hydrated source-key byte allowance.
pub const P8_MAX_HYDRATION_BYTES: usize = context_query::MAX_QUERY_HYDRATION_BYTES;
/// Default elapsed orchestration allowance.
pub const P8_DEFAULT_ELAPSED_MICROS: u64 = context_query::DEFAULT_QUERY_ELAPSED_MICROS;
/// Hard maximum elapsed orchestration allowance.
pub const P8_MAX_ELAPSED_MICROS: u64 = context_query::MAX_QUERY_ELAPSED_MICROS;
/// Maximum final results.
pub const P8_MAX_RESULTS: usize = context_core::policy::MAX_SEARCH_LIMIT;
/// Stable stage-kind registry order used by manifests and diagnostics.
///
/// Recursive plans execute these kinds in tree order, so this is not a
/// universal temporal execution sequence.
pub const P8_STAGE_KINDS: [&str; 9] = [
    "readiness",
    "filter_candidates",
    "candidates",
    "source_recheck",
    "topology_expansion",
    "fusion",
    "score_transform",
    "external_rerank",
    "rerank",
];

/// Returns a stable FNV-1a identity over every frozen Phase 8 field.
#[must_use]
pub fn p8_composite_manifest_hash() -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for value in [
        P8_MAX_QUERY_DEPTH,
        P8_MAX_QUERY_NODES,
        P8_MAX_STAGES,
        P8_MAX_CANDIDATES,
        P8_DEFAULT_COMPARISONS,
        P8_MAX_COMPARISONS,
        P8_MAX_EXPANSIONS,
        P8_DEFAULT_MEMORY_BYTES,
        P8_MAX_MEMORY_BYTES,
        P8_DEFAULT_HYDRATION_BYTES,
        P8_MAX_HYDRATION_BYTES,
        P8_MAX_RESULTS,
    ] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    hash = fnv1a(hash, &P8_DEFAULT_ELAPSED_MICROS.to_le_bytes());
    hash = fnv1a(hash, &P8_MAX_ELAPSED_MICROS.to_le_bytes());
    for stage in P8_STAGE_KINDS {
        hash = fnv1a(hash, stage.as_bytes());
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
