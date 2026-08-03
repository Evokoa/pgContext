//! Emits the frozen Phase 8 composite-executor manifest.

#![allow(clippy::print_stdout)]

use context_test::{
    P8_DEFAULT_COMPARISONS, P8_DEFAULT_ELAPSED_MICROS, P8_DEFAULT_HYDRATION_BYTES,
    P8_DEFAULT_MEMORY_BYTES, P8_MAX_CANDIDATES, P8_MAX_COMPARISONS, P8_MAX_ELAPSED_MICROS,
    P8_MAX_EXPANSIONS, P8_MAX_HYDRATION_BYTES, P8_MAX_MEMORY_BYTES, P8_MAX_QUERY_DEPTH,
    P8_MAX_QUERY_NODES, P8_MAX_RESULTS, P8_MAX_STAGES, P8_STAGE_KINDS, p8_composite_manifest_hash,
};

fn main() {
    println!("manifest_hash\t{:016x}", p8_composite_manifest_hash());
    println!("query_depth\t{P8_MAX_QUERY_DEPTH}");
    println!("query_nodes\t{P8_MAX_QUERY_NODES}");
    println!("stages\t{P8_MAX_STAGES}");
    println!("candidates\t{P8_MAX_CANDIDATES}");
    println!("default_comparisons\t{P8_DEFAULT_COMPARISONS}");
    println!("max_comparisons\t{P8_MAX_COMPARISONS}");
    println!("expansions\t{P8_MAX_EXPANSIONS}");
    println!("default_memory_bytes\t{P8_DEFAULT_MEMORY_BYTES}");
    println!("max_memory_bytes\t{P8_MAX_MEMORY_BYTES}");
    println!("default_hydration_bytes\t{P8_DEFAULT_HYDRATION_BYTES}");
    println!("max_hydration_bytes\t{P8_MAX_HYDRATION_BYTES}");
    println!("default_elapsed_micros\t{P8_DEFAULT_ELAPSED_MICROS}");
    println!("max_elapsed_micros\t{P8_MAX_ELAPSED_MICROS}");
    println!("results\t{P8_MAX_RESULTS}");
    println!("stage_kinds\t{}", P8_STAGE_KINDS.join(","));
}
