//! Emits the frozen Phase 15 lazy-HNSW certification manifest.

#![allow(clippy::print_stdout)]

use context_test::*;

fn main() {
    println!("manifest_hash\t{:016x}", p15_lazy_hnsw_manifest_hash());
    println!("cursor_contract\t{P15_CURSOR_CONTRACT}");
    println!("cursor_operations\t{}", P15_CURSOR_OPERATIONS.join(","));
    println!("terminations\t{}", P15_TERMINATIONS.join(","));
    println!("metrics\t{}", P15_METRICS.join(","));
    println!("graph_sources\t{}", P15_GRAPH_SOURCES.join(","));
    println!("default_advance_batch\t{P15_DEFAULT_ADVANCE_BATCH}");
    println!("max_advance_batch\t{P15_MAX_ADVANCE_BATCH}");
    println!("max_comparisons\t{P15_MAX_COMPARISONS}");
    println!("max_node_expansions\t{P15_MAX_NODE_EXPANSIONS}");
    println!("max_edges\t{P15_MAX_EDGES}");
    println!("default_memory_bytes\t{P15_DEFAULT_MEMORY_BYTES}");
    println!("max_memory_bytes\t{P15_MAX_MEMORY_BYTES}");
    println!("max_rss_bytes\t{P15_MAX_RSS_BYTES}");
    println!("fixture_rows\t{P15_FIXTURE_ROWS}");
    println!("fixture_dimensions\t{P15_FIXTURE_DIMENSIONS}");
    println!("query_count\t{P15_QUERY_COUNT}");
    println!("timing_repeats\t{P15_TIMING_REPEATS}");
    println!("top_k\t{P15_TOP_K}");
    println!("pre_p15_oracle_fnv64\t{P15_PRE_P15_ORACLE_FNV64:016x}");
    println!("max_p50_latency_ratio_bps\t{P15_MAX_P50_LATENCY_RATIO_BPS}");
    println!("max_retained_memory_ratio_bps\t{P15_MAX_RETAINED_MEMORY_RATIO_BPS}");
    println!("dataset_revision\t{P15_DATASET_REVISION}");
    println!("dataset_spec\t{P15_DATASET_SPEC}");
    println!("dataset_sha256\t{P15_DATASET_SHA256}");
    println!("workload_revision\t{P15_WORKLOAD_REVISION}");
    println!("workload_spec\t{P15_WORKLOAD_SPEC}");
    println!("workload_sha256\t{P15_WORKLOAD_SHA256}");
    println!(
        "required_pg_majors\t{}",
        P15_REQUIRED_PG_MAJORS
            .map(|major| major.to_string())
            .join(",")
    );
    println!("report_markers\t{}", P15_REPORT_MARKERS.join(","));
}
