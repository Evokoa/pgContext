//! Emits the frozen Phase 11 mixed-profile certification manifest.

#![allow(clippy::print_stdout)]

use context_test::{
    P11_CANDIDATE_BUDGET, P11_DATASET_REVISION, P11_FUSED_BRANCH_LIMIT, P11_HELD_OUT_QUERY_COUNT,
    P11_MIN_FUSED_RECALL_DELTA, P11_MULTI_MODEL_GATES, P11_PROFILE_COUNT, P11_PROFILE_WEIGHTS,
    P11_QUALITY_CURVES, P11_REPORT_MARKERS, P11_REQUIRED_PG_MAJORS, P11_RRF_K,
    P11_SINGLE_BRANCH_LIMIT, P11_TOP_K, P11_WORKLOAD_REVISION, p11_multi_model_manifest_hash,
};

fn main() {
    println!("manifest_hash\t{:016x}", p11_multi_model_manifest_hash());
    println!("profile_count\t{P11_PROFILE_COUNT}");
    println!("top_k\t{P11_TOP_K}");
    println!("held_out_queries\t{P11_HELD_OUT_QUERY_COUNT}");
    println!("rrf_k\t{P11_RRF_K}");
    println!("fused_branch_limit\t{P11_FUSED_BRANCH_LIMIT}");
    println!("single_branch_limit\t{P11_SINGLE_BRANCH_LIMIT}");
    println!("candidate_budget\t{P11_CANDIDATE_BUDGET}");
    println!(
        "profile_weights\t{}",
        P11_PROFILE_WEIGHTS
            .map(|weight| weight.to_string())
            .join(",")
    );
    println!("minimum_fused_recall_delta\t{P11_MIN_FUSED_RECALL_DELTA}");
    println!("dataset_revision\t{P11_DATASET_REVISION}");
    println!("workload_revision\t{P11_WORKLOAD_REVISION}");
    println!("quality_curves\t{}", P11_QUALITY_CURVES.join(","));
    println!("report_markers\t{}", P11_REPORT_MARKERS.join(","));
    println!(
        "required_pg_majors\t{}",
        P11_REQUIRED_PG_MAJORS
            .map(|major| major.to_string())
            .join(",")
    );
    for gate in P11_MULTI_MODEL_GATES {
        println!(
            "gate\trows={} status={} report_marker={} command={}",
            gate.rows, gate.status, gate.report_marker, gate.command
        );
    }
}
