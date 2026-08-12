//! Emits the frozen Phase 12 semantic-rerank certification manifest.

#![allow(clippy::print_stdout)]

use context_test::{
    P12_ADAPTER, P12_ARTIFACT_BYTES, P12_ARTIFACT_SHA256, P12_BREAKER_COOLDOWN_MICROS,
    P12_BREAKER_FAILURES, P12_CANDIDATE_COUNT, P12_COLD_START_MICROS, P12_DATASET_REVISION,
    P12_DISTRIBUTION, P12_FAILURE_CONTRACT, P12_HELD_OUT_QUERY_COUNT, P12_INPUT_CONTRACT,
    P12_LICENSE_SPDX, P12_LICENSE_URL, P12_MAX_CANDIDATE_JSON_BYTES, P12_MAX_CANDIDATE_JSON_NODES,
    P12_MAX_COST_MICRODOLLARS_PER_1K, P12_MAX_DOCUMENT_TOKENS, P12_MAX_ELAPSED_MICROS,
    P12_MAX_FILTER_JSON_BYTES, P12_MAX_FILTER_JSON_NODES, P12_MAX_JSON_DEPTH, P12_MAX_QUERY_TOKENS,
    P12_MAX_REQUEST_BYTES, P12_MAX_RESPONSE_JSON_BYTES, P12_MAX_RESPONSE_JSON_NODES,
    P12_MAX_RETRIES, P12_MAX_RSS_BYTES, P12_MAX_WIRE_BYTES, P12_MIN_NDCG_LIFT, P12_MODEL_NAME,
    P12_MODEL_NUMBER, P12_MODEL_REVISION, P12_OUTPUT_CONTRACT, P12_P50_MICROS, P12_P95_MICROS,
    P12_REPORT_MARKERS, P12_REQUIRED_DATASET_ROWS, P12_REQUIRED_DATASET_SHA256,
    P12_REQUIRED_PG_MAJORS, P12_REQUIRED_PLATFORMS, P12_SCORE_CONTRACT, P12_SEMANTIC_RERANK_GATES,
    P12_TOKENIZER_REVISION, P12_TOP_K, P12_WORKLOAD_REVISION, P12_WORKLOAD_SHA256,
    p12_semantic_rerank_manifest_hash,
};

fn main() {
    println!(
        "manifest_hash\t{:016x}",
        p12_semantic_rerank_manifest_hash()
    );
    println!("held_out_queries\t{P12_HELD_OUT_QUERY_COUNT}");
    println!("candidate_count\t{P12_CANDIDATE_COUNT}");
    println!("top_k\t{P12_TOP_K}");
    println!("minimum_ndcg_lift\t{P12_MIN_NDCG_LIFT}");
    println!("p50_micros\t{P12_P50_MICROS}");
    println!("p95_micros\t{P12_P95_MICROS}");
    println!("cold_start_micros\t{P12_COLD_START_MICROS}");
    println!("max_rss_bytes\t{P12_MAX_RSS_BYTES}");
    println!("max_cost_microdollars_per_1k\t{P12_MAX_COST_MICRODOLLARS_PER_1K}");
    println!("max_retries\t{P12_MAX_RETRIES}");
    println!("breaker_failures\t{P12_BREAKER_FAILURES}");
    println!("breaker_cooldown_micros\t{P12_BREAKER_COOLDOWN_MICROS}");
    println!("adapter\t{P12_ADAPTER}");
    println!("model_name\t{P12_MODEL_NAME}");
    println!("model_number\t{P12_MODEL_NUMBER}");
    println!("tokenizer_revision\t{P12_TOKENIZER_REVISION}");
    println!("input_contract\t{P12_INPUT_CONTRACT}");
    println!("output_contract\t{P12_OUTPUT_CONTRACT}");
    println!("failure_contract\t{P12_FAILURE_CONTRACT}");
    println!("score_contract\t{P12_SCORE_CONTRACT}");
    println!("max_request_bytes\t{P12_MAX_REQUEST_BYTES}");
    println!("max_wire_bytes\t{P12_MAX_WIRE_BYTES}");
    println!("max_query_tokens\t{P12_MAX_QUERY_TOKENS}");
    println!("max_document_tokens\t{P12_MAX_DOCUMENT_TOKENS}");
    println!("max_elapsed_micros\t{P12_MAX_ELAPSED_MICROS}");
    println!("max_candidate_json_bytes\t{P12_MAX_CANDIDATE_JSON_BYTES}");
    println!("max_candidate_json_nodes\t{P12_MAX_CANDIDATE_JSON_NODES}");
    println!("max_json_depth\t{P12_MAX_JSON_DEPTH}");
    println!("max_filter_json_bytes\t{P12_MAX_FILTER_JSON_BYTES}");
    println!("max_filter_json_nodes\t{P12_MAX_FILTER_JSON_NODES}");
    println!("max_response_json_bytes\t{P12_MAX_RESPONSE_JSON_BYTES}");
    println!("max_response_json_nodes\t{P12_MAX_RESPONSE_JSON_NODES}");
    println!("artifact_bytes\t{P12_ARTIFACT_BYTES}");
    println!("artifact_sha256\t{P12_ARTIFACT_SHA256}");
    println!("license_spdx\t{P12_LICENSE_SPDX}");
    println!("license_url\t{P12_LICENSE_URL}");
    println!("distribution\t{P12_DISTRIBUTION}");
    println!("required_platforms\t{}", P12_REQUIRED_PLATFORMS.join(","));
    println!("dataset_revision\t{P12_DATASET_REVISION}");
    println!("workload_revision\t{P12_WORKLOAD_REVISION}");
    println!("required_dataset_rows\t{P12_REQUIRED_DATASET_ROWS}");
    println!("required_dataset_sha256\t{P12_REQUIRED_DATASET_SHA256}");
    println!("workload_sha256\t{P12_WORKLOAD_SHA256}");
    println!("model_revision\t{P12_MODEL_REVISION}");
    println!("report_markers\t{}", P12_REPORT_MARKERS.join(","));
    println!(
        "required_pg_majors\t{}",
        P12_REQUIRED_PG_MAJORS
            .map(|major| major.to_string())
            .join(",")
    );
    for gate in P12_SEMANTIC_RERANK_GATES {
        println!(
            "gate\trows={} status={} report_marker={} command={}",
            gate.rows, gate.status, gate.report_marker, gate.command
        );
    }
}
