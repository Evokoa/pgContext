//! Phase 13 automatic-chunking certification manifest contract.

use context_test::{
    P13_CHUNK_TARGET_TOKENS, P13_DATASET_GENERATOR_HASH, P13_MAX_ATTEMPTS, P13_MAX_BATCH_ITEMS,
    P13_MAX_CHECKPOINT_UNITS, P13_MAX_CHUNKS_PER_DOCUMENT, P13_MAX_DOCUMENT_BYTES,
    P13_MAX_DOCUMENT_TOKENS, P13_MAX_ELAPSED_MICROS, P13_MAX_FAILURE_CODE_BYTES,
    P13_MAX_JSON_DEPTH, P13_MAX_LEASE_MILLIS, P13_MAX_NAME_BYTES, P13_MAX_OVERLAP_TOKENS,
    P13_MAX_RETAINED_PROFILES, P13_MAX_RSS_BYTES, P13_MAX_SOURCE_KEY_BYTES, P13_MAX_STAGING_BYTES,
    P13_MAX_STRUCTURE_SEGMENT_BYTES, P13_PARSER_REVISIONS, P13_PROFILE_ALIAS_CONTRACT,
    P13_PROJECTION_CONTRACT, P13_PUBLICATION_SAMPLE_DOCUMENTS, P13_REPORT_MARKERS,
    P13_REQUIRED_PG_MAJORS, P13_TOKENIZER_REVISION, P13_WORKER_FAILURE_CONTRACT,
    P13_WORKER_INPUT_CONTRACT, P13_WORKER_OUTPUT_CONTRACT, P13_WORKLOAD_HASH,
    p13_automatic_chunking_manifest_hash,
};

#[test]
fn p13_manifest_freezes_parser_tokenizer_and_document_bounds() {
    assert_eq!(
        P13_PARSER_REVISIONS,
        ["plain_text_v1", "markdown_v1", "html_v1"]
    );
    assert_eq!(P13_TOKENIZER_REVISION, "unicode_words_v1");
    assert_eq!(P13_WORKER_INPUT_CONTRACT, "chunk_worker_request_v1");
    assert_eq!(P13_WORKER_OUTPUT_CONTRACT, "chunk_worker_response_v1");
    assert_eq!(P13_WORKER_FAILURE_CONTRACT, "chunk_worker_failure_v1");
    assert_eq!(P13_PROJECTION_CONTRACT, "document_chunk_projection_v1");
    assert_eq!(P13_PROFILE_ALIAS_CONTRACT, "chunk_profile_alias_v1");
    assert_eq!(P13_MAX_STRUCTURE_SEGMENT_BYTES, 512);
    assert_eq!(P13_CHUNK_TARGET_TOKENS, 384);
    assert_eq!(P13_MAX_OVERLAP_TOKENS, 64);
    assert_eq!(P13_MAX_DOCUMENT_BYTES, 8 * 1024 * 1024);
    assert_eq!(P13_MAX_DOCUMENT_TOKENS, 2_000_000);
    assert_eq!(P13_MAX_CHUNKS_PER_DOCUMENT, 16 * 1024);
    assert_eq!(P13_MAX_STAGING_BYTES, 32 * 1024 * 1024);
    assert_eq!(P13_MAX_BATCH_ITEMS, 256);
    assert_eq!(P13_MAX_SOURCE_KEY_BYTES, 1024);
    assert_eq!(P13_MAX_NAME_BYTES, 128);
    assert_eq!(P13_MAX_CHECKPOINT_UNITS, 1_000_000);
    assert_eq!(P13_MAX_FAILURE_CODE_BYTES, 64);
    assert_eq!(P13_MAX_RETAINED_PROFILES, 8);
    assert_eq!(P13_MAX_JSON_DEPTH, 64);
}

#[test]
fn p13_manifest_freezes_lifecycle_and_promotion_evidence() {
    assert_eq!(P13_MAX_LEASE_MILLIS, 60_000);
    assert_eq!(P13_MAX_ATTEMPTS, 3);
    assert_eq!(P13_MAX_ELAPSED_MICROS, 120_000_000);
    assert_eq!(P13_MAX_RSS_BYTES, 256 * 1024 * 1024);
    assert_eq!(P13_REQUIRED_PG_MAJORS, [17, 18]);
    assert_eq!(P13_PUBLICATION_SAMPLE_DOCUMENTS, 256);
    assert_eq!(P13_REPORT_MARKERS.len(), 6);
    assert_eq!(
        P13_DATASET_GENERATOR_HASH,
        "42a353a799a23a4c3716577ea6eb6a85"
    );
    assert_eq!(P13_WORKLOAD_HASH, "d7cbec1c397316e16ccab456356821d8");
}

#[test]
fn p13_manifest_hash_detects_contract_drift() {
    assert_eq!(
        p13_automatic_chunking_manifest_hash(),
        0xadd5_aad5_de27_e867
    );
}
