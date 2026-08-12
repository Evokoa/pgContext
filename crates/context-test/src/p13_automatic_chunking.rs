//! Frozen Phase 13 automatic-document-chunking certification manifest.

/// Certified parser revisions.
pub const P13_PARSER_REVISIONS: [&str; 3] = ["plain_text_v1", "markdown_v1", "html_v1"];
/// Certified deterministic tokenizer revision.
pub const P13_TOKENIZER_REVISION: &str = "unicode_words_v1";
/// Versioned worker input contract.
pub const P13_WORKER_INPUT_CONTRACT: &str = "chunk_worker_request_v1";
/// Versioned worker output contract.
pub const P13_WORKER_OUTPUT_CONTRACT: &str = "chunk_worker_response_v1";
/// Versioned content-free worker failure contract.
pub const P13_WORKER_FAILURE_CONTRACT: &str = "chunk_worker_failure_v1";
/// Versioned 28-column user-owned projection contract.
pub const P13_PROJECTION_CONTRACT: &str = "document_chunk_projection_v1";
/// Versioned current/shadow/fallback profile-alias lifecycle.
pub const P13_PROFILE_ALIAS_CONTRACT: &str = "chunk_profile_alias_v1";
/// Target tokens per chunk.
pub const P13_CHUNK_TARGET_TOKENS: usize = 384;
/// Maximum tokens per chunk.
pub const P13_MAX_CHUNK_TOKENS: usize = 512;
/// Minimum useful tokens per non-final chunk.
pub const P13_MIN_CHUNK_TOKENS: usize = 32;
/// Maximum overlap between adjacent chunks.
pub const P13_MAX_OVERLAP_TOKENS: usize = 64;
/// Maximum raw document bytes admitted by the first certified profiles.
pub const P13_MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
/// Maximum extracted Unicode scalar values.
pub const P13_MAX_EXTRACTED_CHARS: usize = 8 * 1024 * 1024;
/// Maximum tokens extracted from one document.
pub const P13_MAX_DOCUMENT_TOKENS: usize = 2_000_000;
/// Maximum chunks in one document generation.
pub const P13_MAX_CHUNKS_PER_DOCUMENT: usize = 16 * 1024;
/// Maximum parser structure nesting.
pub const P13_MAX_STRUCTURE_DEPTH: usize = 32;
/// Maximum bytes in one parser structure-path segment.
pub const P13_MAX_STRUCTURE_SEGMENT_BYTES: usize = 512;
/// Maximum bytes across staged chunk text, structure, and lineage.
pub const P13_MAX_STAGING_BYTES: usize = 32 * 1024 * 1024;
/// Maximum JSONB iterator tokens accepted in one staged worker response.
pub const P13_MAX_STAGING_JSON_NODES: usize = 1_000_000;
/// Maximum encoded worker frame.
pub const P13_MAX_WORKER_FRAME_BYTES: usize = 40 * 1024 * 1024;
/// Maximum source keys or jobs admitted by one public batch.
pub const P13_MAX_BATCH_ITEMS: usize = 256;
/// Maximum encoded source-key bytes.
pub const P13_MAX_SOURCE_KEY_BYTES: usize = 1024;
/// Maximum public profile/source/worker identifier bytes.
pub const P13_MAX_NAME_BYTES: usize = 128;
/// Maximum progress units in one checkpoint.
pub const P13_MAX_CHECKPOINT_UNITS: usize = 1_000_000;
/// Maximum content-free failure-code bytes.
pub const P13_MAX_FAILURE_CODE_BYTES: usize = 64;
/// Maximum draining predecessors retained by one chunking-profile alias.
pub const P13_MAX_RETAINED_PROFILES: usize = 8;
/// Maximum nesting admitted by the PostgreSQL JSONB boundary.
pub const P13_MAX_JSON_DEPTH: usize = 64;
/// Maximum contextual prefix tokens.
pub const P13_MAX_CONTEXT_PREFIX_TOKENS: usize = 96;
/// Maximum lease extension.
pub const P13_MAX_LEASE_MILLIS: u64 = 60_000;
/// Maximum total lease acquisitions for one generation.
pub const P13_MAX_ATTEMPTS: usize = 3;
/// Maximum complete worker request duration.
pub const P13_MAX_ELAPSED_MICROS: u64 = 120_000_000;
/// Maximum worker RSS attributed to the certified lane.
pub const P13_MAX_RSS_BYTES: usize = 256 * 1024 * 1024;
/// Minimum sustained plain-text token throughput.
pub const P13_MIN_TOKENS_PER_SECOND: usize = 100_000;
/// Minimum sustained published chunk throughput.
pub const P13_MIN_CHUNKS_PER_SECOND: usize = 1_000;
/// Maximum atomic publication transaction duration.
pub const P13_MAX_PUBLICATION_MICROS: u64 = 250_000;
/// Frozen corpus generator identity.
pub const P13_DATASET_REVISION: &str = "p13-source-linked-documents-v1";
/// Digest of the exact deterministic dataset-generator specification.
pub const P13_DATASET_GENERATOR_HASH: &str = "42a353a799a23a4c3716577ea6eb6a85";
/// Frozen parser/chunker workload identity.
pub const P13_WORKLOAD_REVISION: &str = "p13-plain-markdown-html-v1";
/// Digest of the exact 256-document blocking workload.
pub const P13_WORKLOAD_HASH: &str = "d7cbec1c397316e16ccab456356821d8";
/// Rows required in the blocking scale lane.
pub const P13_REQUIRED_DATASET_ROWS: usize = 1_000_000;
/// Documents published by the bounded blocking workload in each scale lane.
pub const P13_PUBLICATION_SAMPLE_DOCUMENTS: usize = 256;
/// PostgreSQL majors required for phase evidence.
pub const P13_REQUIRED_PG_MAJORS: [u16; 2] = [17, 18];
/// Required report markers.
pub const P13_REPORT_MARKERS: [&str; 6] = [
    "automatic_chunking_sample",
    "automatic_chunking_throughput",
    "automatic_chunking_memory",
    "automatic_chunking_publication",
    "automatic_chunking_security",
    "automatic_chunking_environment",
];

/// One frozen automatic-chunking scale lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct P13AutomaticChunkingGate {
    /// Source document rows.
    pub rows: usize,
    /// Required or scheduled status.
    pub status: &'static str,
    /// Reproducible command with PostgreSQL-major placeholders.
    pub command: &'static str,
    /// Required aggregate report marker.
    pub report_marker: &'static str,
}

/// Required 1M and scheduled 10M command/report contracts.
pub const P13_AUTOMATIC_CHUNKING_GATES: [P13AutomaticChunkingGate; 2] = [
    P13AutomaticChunkingGate {
        rows: 1_000_000,
        status: "required_pg17_pg18",
        command: "PG_VERSION=pg{major} PG_FEATURE=pg{major} PGPORT={port} ROW_COUNT=1000000 DBNAME=pgcontext_p13_1m_pg{major} ./tests/heavy/document_chunking_worker.sh",
        report_marker: "automatic_chunking_throughput",
    },
    P13AutomaticChunkingGate {
        rows: 10_000_000,
        status: "scheduled_release_scale",
        command: "PG_VERSION=pg{major} PG_FEATURE=pg{major} PGPORT={port} ROW_COUNT=10000000 DBNAME=pgcontext_p13_10m_pg{major} ./tests/heavy/document_chunking_worker.sh",
        report_marker: "automatic_chunking_throughput",
    },
];

/// Returns a stable FNV-1a identity over every frozen P13 field.
#[must_use]
pub fn p13_automatic_chunking_manifest_hash() -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for value in [
        P13_CHUNK_TARGET_TOKENS,
        P13_MAX_CHUNK_TOKENS,
        P13_MIN_CHUNK_TOKENS,
        P13_MAX_OVERLAP_TOKENS,
        P13_MAX_DOCUMENT_BYTES,
        P13_MAX_EXTRACTED_CHARS,
        P13_MAX_DOCUMENT_TOKENS,
        P13_MAX_CHUNKS_PER_DOCUMENT,
        P13_MAX_STRUCTURE_DEPTH,
        P13_MAX_STRUCTURE_SEGMENT_BYTES,
        P13_MAX_STAGING_BYTES,
        P13_MAX_STAGING_JSON_NODES,
        P13_MAX_WORKER_FRAME_BYTES,
        P13_MAX_BATCH_ITEMS,
        P13_MAX_SOURCE_KEY_BYTES,
        P13_MAX_NAME_BYTES,
        P13_MAX_CHECKPOINT_UNITS,
        P13_MAX_FAILURE_CODE_BYTES,
        P13_MAX_RETAINED_PROFILES,
        P13_MAX_JSON_DEPTH,
        P13_MAX_CONTEXT_PREFIX_TOKENS,
        P13_MAX_ATTEMPTS,
        P13_MAX_RSS_BYTES,
        P13_MIN_TOKENS_PER_SECOND,
        P13_MIN_CHUNKS_PER_SECOND,
        P13_REQUIRED_DATASET_ROWS,
        P13_PUBLICATION_SAMPLE_DOCUMENTS,
    ] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    for value in [
        P13_MAX_LEASE_MILLIS,
        P13_MAX_ELAPSED_MICROS,
        P13_MAX_PUBLICATION_MICROS,
    ] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    for value in [
        P13_TOKENIZER_REVISION,
        P13_WORKER_INPUT_CONTRACT,
        P13_WORKER_OUTPUT_CONTRACT,
        P13_WORKER_FAILURE_CONTRACT,
        P13_PROJECTION_CONTRACT,
        P13_PROFILE_ALIAS_CONTRACT,
        P13_DATASET_REVISION,
        P13_DATASET_GENERATOR_HASH,
        P13_WORKLOAD_REVISION,
        P13_WORKLOAD_HASH,
    ] {
        hash = fnv1a(hash, value.as_bytes());
    }
    for parser in P13_PARSER_REVISIONS {
        hash = fnv1a(hash, parser.as_bytes());
    }
    for marker in P13_REPORT_MARKERS {
        hash = fnv1a(hash, marker.as_bytes());
    }
    for major in P13_REQUIRED_PG_MAJORS {
        hash = fnv1a(hash, &major.to_le_bytes());
    }
    for gate in P13_AUTOMATIC_CHUNKING_GATES {
        hash = fnv1a(hash, &gate.rows.to_le_bytes());
        hash = fnv1a(hash, gate.status.as_bytes());
        hash = fnv1a(hash, gate.command.as_bytes());
        hash = fnv1a(hash, gate.report_marker.as_bytes());
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
