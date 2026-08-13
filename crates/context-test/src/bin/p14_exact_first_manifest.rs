//! Emits the frozen Phase 14 exact-first-readiness certification manifest.

#![allow(clippy::print_stdout)]

use context_test::*;

fn main() {
    println!("manifest_hash\t{:016x}", p14_exact_first_manifest_hash());
    println!("registration_contract\t{P14_REGISTRATION_CONTRACT}");
    println!("advisor_contract\t{P14_ADVISOR_CONTRACT}");
    println!("build_plan_contract\t{P14_BUILD_PLAN_CONTRACT}");
    println!("readiness_states\t{}", P14_READINESS_STATES.join(","));
    println!("apply_policies\t{}", P14_APPLY_POLICIES.join(","));
    println!("source_families\t{}", P14_SOURCE_FAMILIES.join(","));
    println!(
        "optimization_families\t{}",
        P14_OPTIMIZATION_FAMILIES.join(",")
    );
    println!("max_columns\t{P14_MAX_COLUMNS}");
    println!("max_indexes\t{P14_MAX_INDEXES}");
    println!("max_spec_bytes\t{P14_MAX_SPEC_BYTES}");
    println!("max_objectives_bytes\t{P14_MAX_OBJECTIVES_BYTES}");
    println!("max_json_nodes\t{P14_MAX_JSON_NODES}");
    println!("max_json_depth\t{P14_MAX_JSON_DEPTH}");
    println!("max_name_bytes\t{P14_MAX_NAME_BYTES}");
    println!("max_error_code_bytes\t{P14_MAX_ERROR_CODE_BYTES}");
    println!("max_ddl_bytes\t{P14_MAX_DDL_BYTES}");
    println!("max_plan_revisions\t{P14_MAX_PLAN_REVISIONS}");
    println!("max_invalid_samples\t{P14_MAX_INVALID_SAMPLES}");
    println!("max_source_key_bytes\t{P14_MAX_SOURCE_KEY_BYTES}");
    println!("max_batch_rows\t{P14_MAX_BATCH_ROWS}");
    println!("max_batch_bytes\t{P14_MAX_BATCH_BYTES}");
    println!("max_attempts\t{P14_MAX_ATTEMPTS}");
    println!("max_active_jobs\t{P14_MAX_ACTIVE_JOBS}");
    println!("max_targets\t{P14_MAX_TARGETS}");
    println!("max_rss_bytes\t{P14_MAX_RSS_BYTES}");
    println!("max_temp_bytes\t{P14_MAX_TEMP_BYTES}");
    println!("max_wal_bytes\t{P14_MAX_WAL_BYTES}");
    println!("max_storage_bytes\t{P14_MAX_STORAGE_BYTES}");
    println!("max_locks_per_step\t{P14_MAX_LOCKS_PER_STEP}");
    println!("max_publication_lock_micros\t{P14_MAX_PUBLICATION_LOCK_MICROS}");
    println!("max_statements_per_step\t{P14_MAX_STATEMENTS_PER_STEP}");
    println!("max_elapsed_micros\t{P14_MAX_ELAPSED_MICROS}");
    println!("max_cancel_micros\t{P14_MAX_CANCEL_MICROS}");
    println!("max_lease_millis\t{P14_MAX_LEASE_MILLIS}");
    println!("max_convergence_micros\t{P14_MAX_CONVERGENCE_MICROS}");
    println!("min_ann_rows\t{P14_MIN_ANN_ROWS}");
    println!("min_ivf_rows\t{P14_MIN_IVF_ROWS}");
    println!("high_churn_millihertz\t{P14_HIGH_CHURN_MILLIHERTZ}");
    println!("selective_filter_bps\t{P14_SELECTIVE_FILTER_BPS}");
    println!("min_ivf_build_window_seconds\t{P14_MIN_IVF_BUILD_WINDOW_SECONDS}");
    println!("exact_oracle_contract\t{P14_EXACT_ORACLE_CONTRACT}");
    println!("min_recall_bps\t{P14_MIN_RECALL_BPS}");
    println!("min_backfill_rows_per_second\t{P14_MIN_BACKFILL_ROWS_PER_SECOND}");
    println!("max_building_query_p95_micros\t{P14_MAX_BUILDING_QUERY_P95_MICROS}");
    println!("max_indexed_query_p95_micros\t{P14_MAX_INDEXED_QUERY_P95_MICROS}");
    println!("indexed_candidate_budget\t{P14_INDEXED_CANDIDATE_BUDGET}");
    println!("required_dataset_rows\t{P14_REQUIRED_DATASET_ROWS}");
    println!("dataset_revision\t{P14_DATASET_REVISION}");
    println!("dataset_generator_spec\t{P14_DATASET_GENERATOR_SPEC}");
    println!("dataset_generator_sha256\t{P14_DATASET_GENERATOR_SHA256}");
    println!("workload_revision\t{P14_WORKLOAD_REVISION}");
    println!("workload_spec\t{P14_WORKLOAD_SPEC}");
    println!("workload_sha256\t{P14_WORKLOAD_SHA256}");
    println!(
        "required_pg_majors\t{}",
        P14_REQUIRED_PG_MAJORS
            .map(|major| major.to_string())
            .join(",")
    );
    println!("report_markers\t{}", P14_REPORT_MARKERS.join(","));
    for gate in P14_EXACT_FIRST_GATES {
        println!(
            "gate\trows={} status={} report_marker={} command={}",
            gate.rows, gate.status, gate.report_marker, gate.command
        );
    }
}
