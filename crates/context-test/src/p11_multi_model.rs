//! Frozen Phase 11 mixed-profile certification manifest.

/// Number of independently scored embedding profiles.
pub const P11_PROFILE_COUNT: usize = 2;
/// Final result depth used for every held-out quality comparison.
pub const P11_TOP_K: usize = 20;
/// Number of deterministic held-out queries required in every scale lane.
pub const P11_HELD_OUT_QUERY_COUNT: usize = 8;
/// Weighted reciprocal-rank constant.
pub const P11_RRF_K: u32 = 60;
/// Per-profile candidate limit in the fused request.
pub const P11_FUSED_BRANCH_LIMIT: usize = 50;
/// Candidate limit for either single-profile baseline.
pub const P11_SINGLE_BRANCH_LIMIT: usize = 101;
/// Global candidate allowance, including one completeness probe per branch.
pub const P11_CANDIDATE_BUDGET: usize = 102;
/// Frozen equal profile weights, declared before any quality result is read.
pub const P11_PROFILE_WEIGHTS: [u32; P11_PROFILE_COUNT] = [1, 1];
/// Minimum permitted fused hit delta from the stronger single baseline.
pub const P11_MIN_FUSED_RECALL_DELTA: i32 = 0;
/// Deterministic mixed-coverage/model-space dataset contract revision.
pub const P11_DATASET_REVISION: &str = "p11-mixed-model-spaces-v2";
/// Deterministic held-out workload contract revision.
pub const P11_WORKLOAD_REVISION: &str = "p11-held-out-eight-v2";
/// Curves that every report must contain.
pub const P11_QUALITY_CURVES: [&str; 5] = ["a_only", "b_only", "fused", "partial", "degraded"];
/// Stable markers required from every executed scale report.
pub const P11_REPORT_MARKERS: [&str; 4] = [
    "multi_model_sample",
    "multi_model_quality",
    "multi_model_latency_cost",
    "multi_model_environment",
];
/// PostgreSQL majors required for the one-million-row promotion lane.
pub const P11_REQUIRED_PG_MAJORS: [u16; 2] = [17, 18];

/// One frozen P11 scale lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct P11MultiModelGate {
    /// Source corpus rows.
    pub rows: usize,
    /// Whether this lane is required for promotion or scheduled release evidence.
    pub status: &'static str,
    /// Reproducible command contract, with the PostgreSQL major substituted.
    pub command: &'static str,
    /// Stable aggregate quality marker.
    pub report_marker: &'static str,
}

/// Required 1M evidence and preserved 10M command/report contract.
pub const P11_MULTI_MODEL_GATES: [P11MultiModelGate; 2] = [
    P11MultiModelGate {
        rows: 1_000_000,
        status: "required_pg17_pg18",
        command: "PG_VERSION=pg{major} PG_FEATURE=pg{major} PGPORT={port} ROW_COUNT=1000000 DBNAME=pgcontext_p11_1m_pg{major} ./tests/heavy/multi_model_coverage.sh",
        report_marker: "multi_model_quality",
    },
    P11MultiModelGate {
        rows: 10_000_000,
        status: "scheduled_release_scale",
        command: "PG_VERSION=pg{major} PG_FEATURE=pg{major} PGPORT={port} ROW_COUNT=10000000 DBNAME=pgcontext_p11_10m_pg{major} ./tests/heavy/multi_model_coverage.sh",
        report_marker: "multi_model_quality",
    },
];

/// Returns a stable FNV-1a identity over every frozen P11 field.
#[must_use]
pub fn p11_multi_model_manifest_hash() -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for value in [
        P11_PROFILE_COUNT,
        P11_TOP_K,
        P11_HELD_OUT_QUERY_COUNT,
        P11_FUSED_BRANCH_LIMIT,
        P11_SINGLE_BRANCH_LIMIT,
        P11_CANDIDATE_BUDGET,
    ] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    hash = fnv1a(hash, &P11_RRF_K.to_le_bytes());
    for weight in P11_PROFILE_WEIGHTS {
        hash = fnv1a(hash, &weight.to_le_bytes());
    }
    hash = fnv1a(hash, &P11_MIN_FUSED_RECALL_DELTA.to_le_bytes());
    hash = fnv1a(hash, P11_DATASET_REVISION.as_bytes());
    hash = fnv1a(hash, P11_WORKLOAD_REVISION.as_bytes());
    for curve in P11_QUALITY_CURVES {
        hash = fnv1a(hash, curve.as_bytes());
    }
    for marker in P11_REPORT_MARKERS {
        hash = fnv1a(hash, marker.as_bytes());
    }
    for major in P11_REQUIRED_PG_MAJORS {
        hash = fnv1a(hash, &major.to_le_bytes());
    }
    for gate in P11_MULTI_MODEL_GATES {
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
