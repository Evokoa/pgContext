//! Frozen Phase 16 provider-neutral virtual-beam certification manifest.

/// Versioned pure beam contract.
pub const P16_BEAM_CONTRACT: &str = "virtual_beam_vector_v1";
/// Stable state transitions supported by the vector-only kernel.
pub const P16_TRANSITIONS: [&str; 2] = ["seed", "vector"];
/// Stable terminal reasons.
pub const P16_TERMINATIONS: [&str; 9] = [
    "exhausted",
    "cancelled",
    "admitted_states",
    "visited_keys",
    "vector_expansions",
    "exact_reranks",
    "parent_bytes",
    "retained_bytes",
    "elapsed",
];
/// Stable pruning counters.
pub const P16_PRUNING_REASONS: [&str; 5] = ["duplicate", "dominated", "cycle", "beam_width", "hop"];
/// Default live frontier width.
pub const P16_DEFAULT_BEAM_WIDTH: usize = context_query::DEFAULT_BEAM_WIDTH;
/// Maximum live frontier width.
pub const P16_MAX_BEAM_WIDTH: usize = context_query::MAX_BEAM_WIDTH;
/// Default provider expansion batch.
pub const P16_DEFAULT_EXPANSION_BATCH: usize = context_query::DEFAULT_BEAM_EXPANSION_BATCH;
/// Maximum provider expansion batch.
pub const P16_MAX_EXPANSION_BATCH: usize = context_query::MAX_BEAM_EXPANSION_BATCH;
/// Maximum admitted states.
pub const P16_MAX_ADMITTED_STATES: usize = context_query::MAX_BEAM_ADMITTED_STATES;
/// Maximum dominance keys.
pub const P16_MAX_VISITED_KEYS: usize = context_query::MAX_BEAM_VISITED_KEYS;
/// Maximum vector expansion work.
pub const P16_MAX_VECTOR_EXPANSIONS: usize = context_query::MAX_BEAM_VECTOR_EXPANSIONS;
/// Maximum exact rerank work.
pub const P16_MAX_EXACT_RERANKS: usize = context_query::MAX_BEAM_EXACT_RERANKS;
/// Maximum reconstructed parent depth.
pub const P16_MAX_HOPS: u16 = context_query::MAX_BEAM_HOPS;
/// Maximum arena parent bytes.
pub const P16_MAX_PARENT_BYTES: usize = context_query::MAX_BEAM_PARENT_BYTES;
/// Maximum total beam-retained bytes.
pub const P16_MAX_RETAINED_BYTES: usize = context_query::MAX_BEAM_RETAINED_BYTES;
/// Maximum elapsed allowance in microseconds.
pub const P16_MAX_ELAPSED_MICROS: u64 = context_query::MAX_BEAM_ELAPSED_MICROS;
/// Maximum final result count.
pub const P16_MAX_RESULTS: usize = context_core::policy::MAX_SEARCH_LIMIT;
/// Frozen graph-off fixture seed count.
pub const P16_FIXTURE_SEEDS: usize = 10;
/// Frozen held-out query count.
pub const P16_QUERY_COUNT: usize = 128;
/// Frozen graph-off result count.
pub const P16_TOP_K: usize = 10;
/// Alternating release measurements per query.
pub const P16_TIMING_REPEATS: usize = 11;
/// Maximum beam/direct P15 graph-off p50 ratio in basis points.
pub const P16_MAX_P50_LATENCY_RATIO_BPS: u16 = 11_000;
/// Maximum beam/direct P15 graph-off retained-byte ratio in basis points.
pub const P16_MAX_RETAINED_MEMORY_RATIO_BPS: u16 = 11_000;
/// Maximum standalone certification resident set size.
pub const P16_MAX_RSS_BYTES: usize = 256 * 1024 * 1024;
/// Aggregate ordered result/score/work hash from the independent P15 graph-off oracle.
///
/// This is frozen after the independent correctness oracle and before release
/// timing is executed.
pub const P16_GRAPH_OFF_ORACLE_FNV64: u64 = 0x36e3_f0bf_52dd_bbac;
/// Independent expanded-beam reference traversal hash.
pub const P16_EXPANDED_BEAM_ORACLE_FNV64: u64 = 0x0d87_b67f_96b8_8dd5;
/// Deterministic fixture revision.
pub const P16_DATASET_REVISION: &str = "p16-virtual-beam-fixture-v1";
/// Canonical fixture generator specification.
pub const P16_DATASET_SPEC: &str = "p16_graph_off_fixture_v1|p15_dataset_sha256=fa376272155f9777cd232d064daf424e1f77aca0f71292411bc7502d63009b34|rows=10000|dimensions=32|queries=128|cursor_batch=32|beam_seeds=10|top_k=10|score=-l2|authorization=1|ordered=score_desc,occurrence_asc";
/// SHA-256 of [`P16_DATASET_SPEC`].
pub const P16_DATASET_SHA256: &str =
    "7a1b53b991f63284627939db38d44d777615c8a55965d0ccff038fd535d811b6";
/// Canonical certification workload revision.
pub const P16_WORKLOAD_REVISION: &str = "p16-virtual-beam-workload-v3";
/// Canonical correctness and overhead workload.
pub const P16_WORKLOAD_SPEC: &str = "p16_workload_v3|manifest|independent_graph_off_oracle|independent_expanded_reference|arena_reconstruction|ties|provider_order|dominance|duplicates|cycles|hub|occurrence_point_identity|overreturn|unknown_parent|topology_reject|malformed_score|rerank_accounting|post_provider_elapsed|provider_error|cancel|transient_memory_boundaries|exact_budget_boundaries|p15_graph_off_order_score_work_parity|timing_repeats=11|rss";
/// SHA-256 of [`P16_WORKLOAD_SPEC`].
pub const P16_WORKLOAD_SHA256: &str =
    "53a4248d0275fd1ef5aa62681327c527dc0c0b208126765c2ce5c8b1c6973302";
/// Required report markers.
pub const P16_REPORT_MARKERS: [&str; 12] = [
    "virtual_beam_manifest",
    "virtual_beam_oracle",
    "virtual_beam_expanded_oracle",
    "virtual_beam_properties",
    "virtual_beam_provider_contract",
    "virtual_beam_budget",
    "virtual_beam_graph_off_parity",
    "virtual_beam_resources",
    "virtual_beam_samples",
    "virtual_beam_rss",
    "virtual_beam_environment",
    "virtual_beam_decision",
];

/// Returns a stable FNV-1a identity over every frozen P16 field.
#[must_use]
pub fn p16_virtual_beam_manifest_hash() -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for value in [
        P16_DEFAULT_BEAM_WIDTH,
        P16_MAX_BEAM_WIDTH,
        P16_DEFAULT_EXPANSION_BATCH,
        P16_MAX_EXPANSION_BATCH,
        P16_MAX_ADMITTED_STATES,
        P16_MAX_VISITED_KEYS,
        P16_MAX_VECTOR_EXPANSIONS,
        P16_MAX_EXACT_RERANKS,
        usize::from(P16_MAX_HOPS),
        P16_MAX_PARENT_BYTES,
        P16_MAX_RETAINED_BYTES,
        P16_MAX_RESULTS,
        P16_FIXTURE_SEEDS,
        P16_QUERY_COUNT,
        P16_TOP_K,
        P16_TIMING_REPEATS,
        P16_MAX_RSS_BYTES,
    ] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    hash = fnv1a(hash, &P16_MAX_ELAPSED_MICROS.to_le_bytes());
    hash = fnv1a(hash, &P16_GRAPH_OFF_ORACLE_FNV64.to_le_bytes());
    hash = fnv1a(hash, &P16_EXPANDED_BEAM_ORACLE_FNV64.to_le_bytes());
    for value in [
        P16_MAX_P50_LATENCY_RATIO_BPS,
        P16_MAX_RETAINED_MEMORY_RATIO_BPS,
    ] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    for value in [
        P16_BEAM_CONTRACT,
        P16_DATASET_REVISION,
        P16_DATASET_SPEC,
        P16_DATASET_SHA256,
        P16_WORKLOAD_REVISION,
        P16_WORKLOAD_SPEC,
        P16_WORKLOAD_SHA256,
    ] {
        hash = fnv1a(hash, value.as_bytes());
    }
    for value in P16_TRANSITIONS
        .into_iter()
        .chain(P16_TERMINATIONS)
        .chain(P16_PRUNING_REASONS)
        .chain(P16_REPORT_MARKERS)
    {
        hash = fnv1a(hash, value.as_bytes());
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
