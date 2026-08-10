//! Frozen Phase 10 adaptive-dimension promotion/no-go manifest.

/// Full dimension of the frozen Matryoshka fixture.
pub const P10_FULL_DIMENSIONS: usize = 768;
/// Declared prefix dimensions for the frozen fixture.
pub const P10_PREFIX_DIMENSIONS: [usize; 3] = [128, 256, 512];
/// Ranked result depth.
pub const P10_TOP_K: usize = 10;
/// Hard candidate admissions available to one exact nearest leaf.
pub const P10_CANDIDATE_BUDGET: usize = 10_000;
/// Hard prefix comparisons available to one request.
pub const P10_COMPARISON_BUDGET: usize = 1_000_000;
/// Hard authoritative rechecks available to one exact nearest leaf.
pub const P10_RECHECK_BUDGET: usize = 10_000;
/// Expected scheduler termination at both frozen scale points.
pub const P10_EXPECTED_TERMINATION: &str = "recheck_budget";

/// One frozen scale decision for the scan-based adaptive adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct P10AdaptiveGate {
    /// Invoker-visible corpus rows.
    pub rows: usize,
    /// Held-out query count retained by a future indexed implementation.
    pub queries: usize,
    /// Promotion decision for the current scan-based adapter.
    pub decision: &'static str,
    /// Serving path required by the hard-budget preflight.
    pub serving_path: &'static str,
}

/// Frozen 1M/10M no-go decisions.
///
/// These are not latency measurements. Both corpora exceed the authoritative
/// recheck ceiling, so the scheduler must choose full-vector exact search
/// before prefix work. A future indexed or compressed prefix implementation
/// must replace this manifest with measured quality and latency evidence before
/// promotion.
pub const P10_ADAPTIVE_GATES: [P10AdaptiveGate; 2] = [
    P10AdaptiveGate {
        rows: 1_000_000,
        queries: 1_000,
        decision: "no_go",
        serving_path: "full_vector_exact_fallback",
    },
    P10AdaptiveGate {
        rows: 10_000_000,
        queries: 1_000,
        decision: "no_go",
        serving_path: "full_vector_exact_fallback",
    },
];

/// Returns a stable FNV-1a identity over every frozen Phase 10 field.
#[must_use]
pub fn p10_adaptive_manifest_hash() -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for value in [
        P10_FULL_DIMENSIONS,
        P10_TOP_K,
        P10_CANDIDATE_BUDGET,
        P10_COMPARISON_BUDGET,
        P10_RECHECK_BUDGET,
    ] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    for value in P10_PREFIX_DIMENSIONS {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    hash = fnv1a(hash, P10_EXPECTED_TERMINATION.as_bytes());
    for gate in P10_ADAPTIVE_GATES {
        hash = fnv1a(hash, &gate.rows.to_le_bytes());
        hash = fnv1a(hash, &gate.queries.to_le_bytes());
        hash = fnv1a(hash, gate.decision.as_bytes());
        hash = fnv1a(hash, gate.serving_path.as_bytes());
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
