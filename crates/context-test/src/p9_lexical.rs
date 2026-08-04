//! Frozen Phase 9 PostgreSQL-native lexical certification manifest.

/// Maximum registered fields in one lexical document.
pub const P9_MAX_FIELDS: usize = 16;
/// Maximum path components for one JSON/JSONB lexical field.
pub const P9_MAX_JSON_PATH_DEPTH: usize = 16;
/// Maximum UTF-8 bytes in one lexical query input.
pub const P9_MAX_QUERY_BYTES: usize = 4_096;
/// Maximum typed terms or clauses in one lexical query.
pub const P9_MAX_QUERY_NODES: usize = 256;
/// Maximum point IDs accepted by one headline hydration call.
pub const P9_MAX_HEADLINE_POINTS: usize = 1_000;
/// Maximum source-document bytes admitted by one headline call.
pub const P9_MAX_HEADLINE_SOURCE_BYTES: usize = 8 * 1024 * 1024;
/// Maximum returned headline bytes across one call.
pub const P9_MAX_HEADLINE_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
/// Maximum headline option bytes.
pub const P9_MAX_HEADLINE_OPTIONS_BYTES: usize = 4_096;
/// Frozen lexical query-form registry.
pub const P9_QUERY_FORMS: [&str; 9] = [
    "plain",
    "structured",
    "phrase",
    "web_search",
    "prefix",
    "distance",
    "boolean",
    "weight_restricted",
    "registered_tsquery",
];
/// Frozen native ranker registry.
pub const P9_RANKERS: [&str; 2] = ["ts_rank", "ts_rank_cd"];
/// Frozen lexical and fuzzy serving-strategy registry.
pub const P9_INDEX_STRATEGIES: [&str; 6] = [
    "lexical_exact",
    "lexical_gin",
    "lexical_gist",
    "fuzzy_exact",
    "fuzzy_gin",
    "fuzzy_gist",
];

/// One frozen scale/quality gate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct P9LexicalGate {
    /// Corpus row count.
    pub rows: usize,
    /// Deterministic held-out query count.
    pub queries: usize,
    /// Ranked result depth.
    pub top_k: usize,
    /// Minimum recall against the exact PostgreSQL oracle.
    pub minimum_recall: f64,
    /// Minimum normalized discounted cumulative gain.
    pub minimum_ndcg: f64,
    /// Minimum mean reciprocal rank.
    pub minimum_mrr: f64,
    /// Maximum p95 end-to-end latency in microseconds.
    pub maximum_p95_micros: u64,
    /// Maximum admitted candidates per query.
    pub maximum_candidates: usize,
}

/// Frozen 1M and 10M lexical/hybrid certification gates.
pub const P9_LEXICAL_GATES: [P9LexicalGate; 2] = [
    P9LexicalGate {
        rows: 1_000_000,
        queries: 1_000,
        top_k: 10,
        minimum_recall: 0.99,
        minimum_ndcg: 0.98,
        minimum_mrr: 0.98,
        maximum_p95_micros: 250_000,
        maximum_candidates: 10_000,
    },
    P9LexicalGate {
        rows: 10_000_000,
        queries: 1_000,
        top_k: 10,
        minimum_recall: 0.99,
        minimum_ndcg: 0.98,
        minimum_mrr: 0.98,
        maximum_p95_micros: 1_000_000,
        maximum_candidates: 10_000,
    },
];

/// Returns a stable FNV-1a identity over every frozen Phase 9 field.
#[must_use]
pub fn p9_lexical_manifest_hash() -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for value in [
        P9_MAX_FIELDS,
        P9_MAX_JSON_PATH_DEPTH,
        P9_MAX_QUERY_BYTES,
        P9_MAX_QUERY_NODES,
        P9_MAX_HEADLINE_POINTS,
        P9_MAX_HEADLINE_SOURCE_BYTES,
        P9_MAX_HEADLINE_OUTPUT_BYTES,
        P9_MAX_HEADLINE_OPTIONS_BYTES,
    ] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    for value in P9_QUERY_FORMS
        .into_iter()
        .chain(P9_RANKERS)
        .chain(P9_INDEX_STRATEGIES)
    {
        hash = fnv1a(hash, value.as_bytes());
    }
    for gate in P9_LEXICAL_GATES {
        for value in [gate.rows, gate.queries, gate.top_k, gate.maximum_candidates] {
            hash = fnv1a(hash, &value.to_le_bytes());
        }
        for value in [gate.minimum_recall, gate.minimum_ndcg, gate.minimum_mrr] {
            hash = fnv1a(hash, &value.to_bits().to_le_bytes());
        }
        hash = fnv1a(hash, &gate.maximum_p95_micros.to_le_bytes());
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
