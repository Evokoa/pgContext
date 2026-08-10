//! Emits the frozen Phase 10 adaptive-dimension no-go manifest.

#![allow(clippy::print_stdout)]

use context_test::{
    P10_ADAPTIVE_GATES, P10_CANDIDATE_BUDGET, P10_COMPARISON_BUDGET, P10_EXPECTED_TERMINATION,
    P10_FULL_DIMENSIONS, P10_PREFIX_DIMENSIONS, P10_RECHECK_BUDGET, P10_TOP_K,
    p10_adaptive_manifest_hash,
};

fn main() {
    println!("manifest_hash\t{:016x}", p10_adaptive_manifest_hash());
    println!("full_dimensions\t{P10_FULL_DIMENSIONS}");
    println!(
        "prefix_dimensions\t{}",
        P10_PREFIX_DIMENSIONS
            .map(|value| value.to_string())
            .join(",")
    );
    println!("top_k\t{P10_TOP_K}");
    println!("candidate_budget\t{P10_CANDIDATE_BUDGET}");
    println!("comparison_budget\t{P10_COMPARISON_BUDGET}");
    println!("recheck_budget\t{P10_RECHECK_BUDGET}");
    println!("expected_termination\t{P10_EXPECTED_TERMINATION}");
    for gate in P10_ADAPTIVE_GATES {
        println!(
            "gate\trows={} queries={} decision={} serving_path={}",
            gate.rows, gate.queries, gate.decision, gate.serving_path
        );
    }
}
