//! Frozen Phase 5 quantized-HNSW certification thresholds.

/// Dataset rows required by the Phase 5 promotion gate.
pub const P5_CODEC_ROWS: usize = 1_000_000;
/// Dimensions used by the bounded release workload.
pub const P5_CODEC_DIMENSIONS: usize = 32;
/// Deterministic source generator seed.
pub const P5_CODEC_SEED: u64 = 0x5035_434f_4445_4331;
/// Revision of the independent-dimension source-vector generator.
pub const P5_CODEC_GENERATOR_REVISION: u32 = 2;
/// Frozen source rows used as exact-oracle queries.
pub const P5_CODEC_QUERY_IDS: [u64; 5] = [17, 200_003, 400_009, 700_001, 999_983];
/// Exact top-k width used for recall comparisons.
pub const P5_CODEC_TOP_K: usize = 10;
/// Query count used by the release workload.
pub const P5_CODEC_QUERY_COUNT: usize = 5;
/// Maximum rows used to train any one immutable codec generation.
pub const P5_CODEC_TRAINING_SAMPLE_ROWS: usize = 4_096;

/// One frozen codec gate within the Phase 5 manifest.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct P5CodecGate {
    /// SQL reloption value.
    pub mode: &'static str,
    /// Minimum exact-oracle top-k recall after source rerank.
    pub minimum_recall: f64,
    /// Maximum warm-query p95 latency in milliseconds.
    pub maximum_p95_latency_ms: u64,
    /// Maximum resident bytes admitted for one served generation.
    pub maximum_resident_bytes: u64,
    /// Maximum on-disk index bytes.
    pub maximum_index_bytes: u64,
    /// Maximum build duration in seconds.
    pub maximum_build_seconds: u64,
}

/// Frozen per-codec thresholds in deterministic reporting order.
pub const P5_CODEC_GATES: [P5CodecGate; 3] = [
    P5CodecGate {
        mode: "binary",
        minimum_recall: 0.70,
        maximum_p95_latency_ms: 5_000,
        maximum_resident_bytes: 2 * 1024 * 1024 * 1024,
        maximum_index_bytes: 4 * 1024 * 1024 * 1024,
        maximum_build_seconds: 7_200,
    },
    P5CodecGate {
        mode: "scalar",
        minimum_recall: 0.95,
        maximum_p95_latency_ms: 5_000,
        maximum_resident_bytes: 2 * 1024 * 1024 * 1024,
        maximum_index_bytes: 4 * 1024 * 1024 * 1024,
        maximum_build_seconds: 7_200,
    },
    P5CodecGate {
        mode: "pq",
        minimum_recall: 0.90,
        maximum_p95_latency_ms: 5_000,
        maximum_resident_bytes: 2 * 1024 * 1024 * 1024,
        maximum_index_bytes: 4 * 1024 * 1024 * 1024,
        maximum_build_seconds: 7_200,
    },
];

/// Returns a stable FNV-1a identity over every frozen Phase 5 field.
#[must_use]
pub fn p5_codec_manifest_hash() -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for bytes in [
        P5_CODEC_ROWS.to_le_bytes().as_slice(),
        P5_CODEC_DIMENSIONS.to_le_bytes().as_slice(),
        P5_CODEC_SEED.to_le_bytes().as_slice(),
        P5_CODEC_GENERATOR_REVISION.to_le_bytes().as_slice(),
        P5_CODEC_TOP_K.to_le_bytes().as_slice(),
        P5_CODEC_QUERY_COUNT.to_le_bytes().as_slice(),
        P5_CODEC_TRAINING_SAMPLE_ROWS.to_le_bytes().as_slice(),
    ] {
        hash = fnv1a(hash, bytes);
    }
    for query_id in P5_CODEC_QUERY_IDS {
        hash = fnv1a(hash, &query_id.to_le_bytes());
    }
    for gate in P5_CODEC_GATES {
        hash = fnv1a(hash, gate.mode.as_bytes());
        hash = fnv1a(hash, &gate.minimum_recall.to_bits().to_le_bytes());
        hash = fnv1a(hash, &gate.maximum_p95_latency_ms.to_le_bytes());
        hash = fnv1a(hash, &gate.maximum_resident_bytes.to_le_bytes());
        hash = fnv1a(hash, &gate.maximum_index_bytes.to_le_bytes());
        hash = fnv1a(hash, &gate.maximum_build_seconds.to_le_bytes());
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
