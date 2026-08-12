//! Emits the frozen Phase 13 automatic-chunking certification manifest.

#![allow(clippy::print_stdout)]

use context_test::{
    P13_AUTOMATIC_CHUNKING_GATES, P13_CHUNK_TARGET_TOKENS, P13_DATASET_GENERATOR_HASH,
    P13_DATASET_REVISION, P13_MAX_ATTEMPTS, P13_MAX_BATCH_ITEMS, P13_MAX_CHECKPOINT_UNITS,
    P13_MAX_CHUNK_TOKENS, P13_MAX_CHUNKS_PER_DOCUMENT, P13_MAX_CONTEXT_PREFIX_TOKENS,
    P13_MAX_DOCUMENT_BYTES, P13_MAX_DOCUMENT_TOKENS, P13_MAX_ELAPSED_MICROS,
    P13_MAX_EXTRACTED_CHARS, P13_MAX_FAILURE_CODE_BYTES, P13_MAX_JSON_DEPTH, P13_MAX_LEASE_MILLIS,
    P13_MAX_NAME_BYTES, P13_MAX_OVERLAP_TOKENS, P13_MAX_PUBLICATION_MICROS,
    P13_MAX_RETAINED_PROFILES, P13_MAX_RSS_BYTES, P13_MAX_SOURCE_KEY_BYTES, P13_MAX_STAGING_BYTES,
    P13_MAX_STAGING_JSON_NODES, P13_MAX_STRUCTURE_DEPTH, P13_MAX_STRUCTURE_SEGMENT_BYTES,
    P13_MAX_WORKER_FRAME_BYTES, P13_MIN_CHUNK_TOKENS, P13_MIN_CHUNKS_PER_SECOND,
    P13_MIN_TOKENS_PER_SECOND, P13_PARSER_REVISIONS, P13_PROFILE_ALIAS_CONTRACT,
    P13_PROJECTION_CONTRACT, P13_PUBLICATION_SAMPLE_DOCUMENTS, P13_REPORT_MARKERS,
    P13_REQUIRED_DATASET_ROWS, P13_REQUIRED_PG_MAJORS, P13_TOKENIZER_REVISION,
    P13_WORKER_FAILURE_CONTRACT, P13_WORKER_INPUT_CONTRACT, P13_WORKER_OUTPUT_CONTRACT,
    P13_WORKLOAD_HASH, P13_WORKLOAD_REVISION, p13_automatic_chunking_manifest_hash,
};

fn main() {
    println!(
        "manifest_hash\t{:016x}",
        p13_automatic_chunking_manifest_hash()
    );
    println!("parsers\t{}", P13_PARSER_REVISIONS.join(","));
    println!("tokenizer_revision\t{P13_TOKENIZER_REVISION}");
    println!("input_contract\t{P13_WORKER_INPUT_CONTRACT}");
    println!("output_contract\t{P13_WORKER_OUTPUT_CONTRACT}");
    println!("failure_contract\t{P13_WORKER_FAILURE_CONTRACT}");
    println!("projection_contract\t{P13_PROJECTION_CONTRACT}");
    println!("profile_alias_contract\t{P13_PROFILE_ALIAS_CONTRACT}");
    println!("target_tokens\t{P13_CHUNK_TARGET_TOKENS}");
    println!("max_chunk_tokens\t{P13_MAX_CHUNK_TOKENS}");
    println!("min_chunk_tokens\t{P13_MIN_CHUNK_TOKENS}");
    println!("max_overlap_tokens\t{P13_MAX_OVERLAP_TOKENS}");
    println!("max_document_bytes\t{P13_MAX_DOCUMENT_BYTES}");
    println!("max_extracted_chars\t{P13_MAX_EXTRACTED_CHARS}");
    println!("max_document_tokens\t{P13_MAX_DOCUMENT_TOKENS}");
    println!("max_chunks_per_document\t{P13_MAX_CHUNKS_PER_DOCUMENT}");
    println!("max_structure_depth\t{P13_MAX_STRUCTURE_DEPTH}");
    println!("max_structure_segment_bytes\t{P13_MAX_STRUCTURE_SEGMENT_BYTES}");
    println!("max_staging_bytes\t{P13_MAX_STAGING_BYTES}");
    println!("max_staging_json_nodes\t{P13_MAX_STAGING_JSON_NODES}");
    println!("max_worker_frame_bytes\t{P13_MAX_WORKER_FRAME_BYTES}");
    println!("max_batch_items\t{P13_MAX_BATCH_ITEMS}");
    println!("max_source_key_bytes\t{P13_MAX_SOURCE_KEY_BYTES}");
    println!("max_name_bytes\t{P13_MAX_NAME_BYTES}");
    println!("max_checkpoint_units\t{P13_MAX_CHECKPOINT_UNITS}");
    println!("max_failure_code_bytes\t{P13_MAX_FAILURE_CODE_BYTES}");
    println!("max_retained_profiles\t{P13_MAX_RETAINED_PROFILES}");
    println!("max_json_depth\t{P13_MAX_JSON_DEPTH}");
    println!("max_context_prefix_tokens\t{P13_MAX_CONTEXT_PREFIX_TOKENS}");
    println!("max_lease_millis\t{P13_MAX_LEASE_MILLIS}");
    println!("max_attempts\t{P13_MAX_ATTEMPTS}");
    println!("max_elapsed_micros\t{P13_MAX_ELAPSED_MICROS}");
    println!("max_rss_bytes\t{P13_MAX_RSS_BYTES}");
    println!("min_tokens_per_second\t{P13_MIN_TOKENS_PER_SECOND}");
    println!("min_chunks_per_second\t{P13_MIN_CHUNKS_PER_SECOND}");
    println!("max_publication_micros\t{P13_MAX_PUBLICATION_MICROS}");
    println!("dataset_revision\t{P13_DATASET_REVISION}");
    println!("dataset_generator_hash\t{P13_DATASET_GENERATOR_HASH}");
    println!("workload_revision\t{P13_WORKLOAD_REVISION}");
    println!("workload_hash\t{P13_WORKLOAD_HASH}");
    println!("required_dataset_rows\t{P13_REQUIRED_DATASET_ROWS}");
    println!("publication_sample_documents\t{P13_PUBLICATION_SAMPLE_DOCUMENTS}");
    println!(
        "required_pg_majors\t{}",
        P13_REQUIRED_PG_MAJORS
            .map(|major| major.to_string())
            .join(",")
    );
    println!("report_markers\t{}", P13_REPORT_MARKERS.join(","));
    for gate in P13_AUTOMATIC_CHUNKING_GATES {
        println!(
            "gate\trows={} status={} report_marker={} command={}",
            gate.rows, gate.status, gate.report_marker, gate.command
        );
    }
}
