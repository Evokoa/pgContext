//! Prints the frozen Phase 16 virtual-beam manifest.

#![allow(clippy::print_stdout)]

use context_test::*;

fn main() {
    println!("manifest_hash\t{:016x}", p16_virtual_beam_manifest_hash());
    println!("beam_contract\t{P16_BEAM_CONTRACT}");
    println!("transitions\t{}", P16_TRANSITIONS.join(","));
    println!("terminations\t{}", P16_TERMINATIONS.join(","));
    println!("pruning_reasons\t{}", P16_PRUNING_REASONS.join(","));
    println!("default_beam_width\t{P16_DEFAULT_BEAM_WIDTH}");
    println!("max_beam_width\t{P16_MAX_BEAM_WIDTH}");
    println!("default_expansion_batch\t{P16_DEFAULT_EXPANSION_BATCH}");
    println!("max_expansion_batch\t{P16_MAX_EXPANSION_BATCH}");
    println!("max_admitted_states\t{P16_MAX_ADMITTED_STATES}");
    println!("max_visited_keys\t{P16_MAX_VISITED_KEYS}");
    println!("max_vector_expansions\t{P16_MAX_VECTOR_EXPANSIONS}");
    println!("max_exact_reranks\t{P16_MAX_EXACT_RERANKS}");
    println!("max_hops\t{P16_MAX_HOPS}");
    println!("max_parent_bytes\t{P16_MAX_PARENT_BYTES}");
    println!("max_retained_bytes\t{P16_MAX_RETAINED_BYTES}");
    println!("max_elapsed_micros\t{P16_MAX_ELAPSED_MICROS}");
    println!("max_results\t{P16_MAX_RESULTS}");
    println!("fixture_seeds\t{P16_FIXTURE_SEEDS}");
    println!("query_count\t{P16_QUERY_COUNT}");
    println!("top_k\t{P16_TOP_K}");
    println!("timing_repeats\t{P16_TIMING_REPEATS}");
    println!("graph_off_oracle_fnv64\t{P16_GRAPH_OFF_ORACLE_FNV64:016x}");
    println!("expanded_beam_oracle_fnv64\t{P16_EXPANDED_BEAM_ORACLE_FNV64:016x}");
    println!("max_p50_latency_ratio_bps\t{P16_MAX_P50_LATENCY_RATIO_BPS}");
    println!("max_retained_memory_ratio_bps\t{P16_MAX_RETAINED_MEMORY_RATIO_BPS}");
    println!("max_rss_bytes\t{P16_MAX_RSS_BYTES}");
    println!("dataset_revision\t{P16_DATASET_REVISION}");
    println!("dataset_spec\t{P16_DATASET_SPEC}");
    println!("dataset_sha256\t{P16_DATASET_SHA256}");
    println!("workload_revision\t{P16_WORKLOAD_REVISION}");
    println!("workload_spec\t{P16_WORKLOAD_SPEC}");
    println!("workload_sha256\t{P16_WORKLOAD_SHA256}");
    println!("report_markers\t{}", P16_REPORT_MARKERS.join(","));
}
