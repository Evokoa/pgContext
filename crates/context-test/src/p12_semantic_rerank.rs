//! Frozen Phase 12 provider-neutral semantic-rerank certification manifest.

/// Held-out queries evaluated before promotion.
pub const P12_HELD_OUT_QUERY_COUNT: usize = 8;
/// Candidates released for each held-out rerank.
pub const P12_CANDIDATE_COUNT: usize = 32;
/// Final result depth scored by the quality oracle.
pub const P12_TOP_K: usize = 10;
/// Minimum absolute nDCG@10 lift over the fused input ordering.
pub const P12_MIN_NDCG_LIFT: f64 = 0.05;
/// Warm median latency ceiling in microseconds.
pub const P12_P50_MICROS: u64 = 25_000;
/// Warm p95 latency ceiling in microseconds.
pub const P12_P95_MICROS: u64 = 75_000;
/// Cold artifact-load plus first-score latency ceiling in microseconds.
pub const P12_COLD_START_MICROS: u64 = 250_000;
/// Maximum resident worker memory attributed to the fixture lane.
pub const P12_MAX_RSS_BYTES: usize = 128 * 1024 * 1024;
/// Maximum provider cost per thousand requests; the private fixture is local.
pub const P12_MAX_COST_MICRODOLLARS_PER_1K: u64 = 0;
/// Maximum retries after the original bounded attempt.
pub const P12_MAX_RETRIES: usize = 1;
/// Consecutive failures that open the worker circuit breaker.
pub const P12_BREAKER_FAILURES: usize = 3;
/// Circuit-breaker cooldown.
pub const P12_BREAKER_COOLDOWN_MICROS: u64 = 5_000_000;
/// Frozen private worker adapter identifier.
pub const P12_ADAPTER: &str = "linear_pair_v1";
/// Frozen model identity carried on every request.
pub const P12_MODEL_NAME: &str = "linear-pair-v1";
/// Frozen numeric model revision carried on every request.
pub const P12_MODEL_NUMBER: u64 = 7;
/// Exact tokenizer implementation used by the certified fixture.
pub const P12_TOKENIZER_REVISION: &str = "ascii_tokens_v1";
/// Versioned provider-neutral request wire contract.
pub const P12_INPUT_CONTRACT: &str = "rerank_envelope_v3";
/// Versioned successful response wire contract.
pub const P12_OUTPUT_CONTRACT: &str = "rerank_response_v3";
/// Versioned operational failure wire contract.
pub const P12_FAILURE_CONTRACT: &str = "rerank_failure_v1";
/// Frozen score meaning and direction.
pub const P12_SCORE_CONTRACT: &str = "higher_is_better_unit_interval";
/// Maximum decoded query-owned request projection.
pub const P12_MAX_REQUEST_BYTES: usize = 4 * 1024 * 1024;
/// Maximum encoded newline-delimited worker frame.
pub const P12_MAX_WIRE_BYTES: usize = 6 * 1024 * 1024;
/// Fixture query-token ceiling.
pub const P12_MAX_QUERY_TOKENS: usize = 8;
/// Fixture document-token ceiling.
pub const P12_MAX_DOCUMENT_TOKENS: usize = 16;
/// Absolute worker request deadline ceiling.
pub const P12_MAX_ELAPSED_MICROS: u64 = 75_000;
/// Maximum raw PostgreSQL candidates JSONB bytes.
pub const P12_MAX_CANDIDATE_JSON_BYTES: usize = 4 * 1024 * 1024;
/// Maximum PostgreSQL candidates JSONB iterator tokens.
pub const P12_MAX_CANDIDATE_JSON_NODES: usize = 100_000;
/// Maximum PostgreSQL JSONB nesting depth at the semantic-rerank boundary.
pub const P12_MAX_JSON_DEPTH: usize = 64;
/// Maximum raw PostgreSQL filter JSONB bytes.
pub const P12_MAX_FILTER_JSON_BYTES: usize = 256 * 1024;
/// Maximum PostgreSQL filter JSONB iterator tokens.
pub const P12_MAX_FILTER_JSON_NODES: usize = 1_024;
/// Maximum raw PostgreSQL response JSONB bytes.
pub const P12_MAX_RESPONSE_JSON_BYTES: usize = 256 * 1024;
/// Maximum PostgreSQL response JSONB iterator tokens.
pub const P12_MAX_RESPONSE_JSON_NODES: usize = 4_096;
/// Frozen operator-provided fixture artifact length.
pub const P12_ARTIFACT_BYTES: usize = 40;
/// Frozen operator-provided fixture artifact digest.
pub const P12_ARTIFACT_SHA256: &str =
    "6c95d5116ac4173156e8f71439220a10e41fe1391a60e3d105a67eda345aa8d3";
/// Frozen fixture artifact license identifier.
pub const P12_LICENSE_SPDX: &str = "Apache-2.0";
/// Frozen fixture artifact license URL.
pub const P12_LICENSE_URL: &str = "https://www.apache.org/licenses/LICENSE-2.0";
/// Frozen fixture artifact distribution policy.
pub const P12_DISTRIBUTION: &str = "operator_provided_only";
/// Worker platforms required by the provider-neutral binary contract.
pub const P12_REQUIRED_PLATFORMS: [&str; 4] = [
    "darwin-aarch64",
    "darwin-x86_64",
    "linux-aarch64",
    "linux-x86_64",
];
/// Frozen dataset identity.
pub const P12_DATASET_REVISION: &str = "p12-held-out-rerank-v1";
/// Frozen query/judgment workload identity.
pub const P12_WORKLOAD_REVISION: &str = "p12-eight-query-judgments-v2";
/// Required corpus size whose exact generated rows are frozen below.
pub const P12_REQUIRED_DATASET_ROWS: usize = 1_000_000;
/// SHA-256 over the required 1M `id|tenant|body|source_version` row stream.
pub const P12_REQUIRED_DATASET_SHA256: &str =
    "e9477749c02a3a4005c9792f27f2028c759071811b8545d157cae3f7bbf656b1";
/// SHA-256 over the eight frozen query/candidate/judgment rows.
pub const P12_WORKLOAD_SHA256: &str =
    "d513054a5c372a2f6a327c74c5e001bef0320304d395f7c4dc41366092017a71";
/// Frozen private adapter and operator-artifact contract identity.
pub const P12_MODEL_REVISION: &str = "linear_pair_v1-operator-artifact-v1";
/// Required report markers.
pub const P12_REPORT_MARKERS: [&str; 5] = [
    "semantic_rerank_sample",
    "semantic_rerank_quality",
    "semantic_rerank_latency_cost",
    "semantic_rerank_security",
    "semantic_rerank_environment",
];
/// PostgreSQL majors required for promotion evidence.
pub const P12_REQUIRED_PG_MAJORS: [u16; 2] = [17, 18];

/// One frozen P12 scale lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct P12SemanticRerankGate {
    /// Source corpus rows.
    pub rows: usize,
    /// Whether the lane is required now or retained for scheduled releases.
    pub status: &'static str,
    /// Reproducible command with PostgreSQL major and port placeholders.
    pub command: &'static str,
    /// Aggregate report marker required from the command.
    pub report_marker: &'static str,
}

/// Required 1M and scheduled 10M command/report contracts.
pub const P12_SEMANTIC_RERANK_GATES: [P12SemanticRerankGate; 2] = [
    P12SemanticRerankGate {
        rows: 1_000_000,
        status: "required_pg17_pg18",
        command: "PG_VERSION=pg{major} PG_FEATURE=pg{major} PGPORT={port} ROW_COUNT=1000000 DBNAME=pgcontext_p12_1m_pg{major} ./tests/heavy/semantic_rerank_contract.sh",
        report_marker: "semantic_rerank_quality",
    },
    P12SemanticRerankGate {
        rows: 10_000_000,
        status: "scheduled_release_scale",
        command: "PG_VERSION=pg{major} PG_FEATURE=pg{major} PGPORT={port} ROW_COUNT=10000000 DBNAME=pgcontext_p12_10m_pg{major} ./tests/heavy/semantic_rerank_contract.sh",
        report_marker: "semantic_rerank_quality",
    },
];

/// Returns a stable FNV-1a identity over every frozen P12 field.
#[must_use]
pub fn p12_semantic_rerank_manifest_hash() -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for value in [
        P12_HELD_OUT_QUERY_COUNT,
        P12_CANDIDATE_COUNT,
        P12_TOP_K,
        P12_MAX_RSS_BYTES,
        P12_MAX_RETRIES,
        P12_BREAKER_FAILURES,
        P12_MAX_REQUEST_BYTES,
        P12_MAX_WIRE_BYTES,
        P12_MAX_QUERY_TOKENS,
        P12_MAX_DOCUMENT_TOKENS,
        P12_MAX_CANDIDATE_JSON_BYTES,
        P12_MAX_CANDIDATE_JSON_NODES,
        P12_MAX_JSON_DEPTH,
        P12_MAX_FILTER_JSON_BYTES,
        P12_MAX_FILTER_JSON_NODES,
        P12_MAX_RESPONSE_JSON_BYTES,
        P12_MAX_RESPONSE_JSON_NODES,
        P12_ARTIFACT_BYTES,
        P12_REQUIRED_DATASET_ROWS,
    ] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    hash = fnv1a(hash, &P12_MIN_NDCG_LIFT.to_bits().to_le_bytes());
    for value in [
        P12_P50_MICROS,
        P12_P95_MICROS,
        P12_COLD_START_MICROS,
        P12_MAX_COST_MICRODOLLARS_PER_1K,
        P12_BREAKER_COOLDOWN_MICROS,
        P12_MODEL_NUMBER,
        P12_MAX_ELAPSED_MICROS,
    ] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    for value in [
        P12_DATASET_REVISION,
        P12_WORKLOAD_REVISION,
        P12_MODEL_REVISION,
        P12_ADAPTER,
        P12_MODEL_NAME,
        P12_TOKENIZER_REVISION,
        P12_INPUT_CONTRACT,
        P12_OUTPUT_CONTRACT,
        P12_FAILURE_CONTRACT,
        P12_SCORE_CONTRACT,
        P12_ARTIFACT_SHA256,
        P12_LICENSE_SPDX,
        P12_LICENSE_URL,
        P12_DISTRIBUTION,
        P12_REQUIRED_DATASET_SHA256,
        P12_WORKLOAD_SHA256,
    ] {
        hash = fnv1a(hash, value.as_bytes());
    }
    for marker in P12_REPORT_MARKERS {
        hash = fnv1a(hash, marker.as_bytes());
    }
    for major in P12_REQUIRED_PG_MAJORS {
        hash = fnv1a(hash, &major.to_le_bytes());
    }
    for platform in P12_REQUIRED_PLATFORMS {
        hash = fnv1a(hash, platform.as_bytes());
    }
    for gate in P12_SEMANTIC_RERANK_GATES {
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
