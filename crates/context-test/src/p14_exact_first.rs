//! Frozen Phase 14 exact-first-readiness certification manifest.

/// Versioned normalized registration contract.
pub const P14_REGISTRATION_CONTRACT: &str = "exact_first_registration_v1";
/// Versioned advisor evidence and decision contract.
pub const P14_ADVISOR_CONTRACT: &str = "exact_first_advisor_v1";
/// Versioned supervised optimization plan contract.
pub const P14_BUILD_PLAN_CONTRACT: &str = "exact_first_build_plan_v1";
/// Frozen five-state lifecycle labels.
pub const P14_READINESS_STATES: [&str; 5] =
    ["exact_only", "building", "indexed", "stale", "degraded"];
/// Explicit mutation policies.
pub const P14_APPLY_POLICIES: [&str; 4] = [
    "recommend_only",
    "exact_only",
    "enqueue",
    "apply_foreground",
];
/// Supported source discovery families.
pub const P14_SOURCE_FAMILIES: [&str; 13] = [
    "vector",
    "halfvec",
    "sparsevec",
    "int8vec",
    "uint8vec",
    "bitvec",
    "vector_array",
    "text",
    "tsvector",
    "scalar",
    "jsonb",
    "temporal_uuid",
    "optional_postgis",
];
/// Optimization families with complete P14 apply and validation adapters.
pub const P14_OPTIMIZATION_FAMILIES: [&str; 3] = ["exact", "hnsw", "ivfflat"];
/// Maximum user columns inspected in one relation.
pub const P14_MAX_COLUMNS: usize = context_core::EXACT_FIRST_MAX_COLUMNS;
/// Maximum indexes inspected for one relation.
pub const P14_MAX_INDEXES: usize = context_core::EXACT_FIRST_MAX_INDEXES;
/// Maximum content-free operational failure-code bytes.
pub const P14_MAX_ERROR_CODE_BYTES: usize = context_core::EXACT_FIRST_MAX_ERROR_CODE_BYTES;
/// Maximum encoded registration specification.
pub const P14_MAX_SPEC_BYTES: usize = context_core::EXACT_FIRST_MAX_SPEC_BYTES;
/// Maximum encoded advisor objectives.
pub const P14_MAX_OBJECTIVES_BYTES: usize = context_core::EXACT_FIRST_MAX_OBJECTIVES_BYTES;
/// Maximum JSON iterator nodes in one input.
pub const P14_MAX_JSON_NODES: usize = context_core::EXACT_FIRST_MAX_JSON_NODES;
/// Maximum JSON nesting depth.
pub const P14_MAX_JSON_DEPTH: usize = context_core::EXACT_FIRST_MAX_JSON_DEPTH;
/// Maximum public identifier bytes.
pub const P14_MAX_NAME_BYTES: usize = context_core::EXACT_FIRST_MAX_NAME_BYTES;
/// Maximum generated DDL bytes in one plan.
pub const P14_MAX_DDL_BYTES: usize = context_core::EXACT_FIRST_MAX_DDL_BYTES;
/// Maximum retained immutable plan revisions per registration.
pub const P14_MAX_PLAN_REVISIONS: i64 = context_core::EXACT_FIRST_MAX_PLAN_REVISIONS;
/// Maximum invalid samples retained per registration.
pub const P14_MAX_INVALID_SAMPLES: usize = 64;
/// Maximum canonical source-key bytes.
pub const P14_MAX_SOURCE_KEY_BYTES: usize = 1024;
/// Maximum rows admitted by one backfill statement.
pub const P14_MAX_BATCH_ROWS: usize = 4096;
/// Maximum logical source bytes admitted by one backfill statement.
pub const P14_MAX_BATCH_BYTES: usize = 32 * 1024 * 1024;
/// Maximum attempts for one frozen optimization plan.
pub const P14_MAX_ATTEMPTS: i32 = context_core::EXACT_FIRST_MAX_ATTEMPTS;
/// Maximum simultaneous jobs per registration.
pub const P14_MAX_ACTIVE_JOBS: usize = 1;
/// Maximum retained current/retired optimization targets.
pub const P14_MAX_TARGETS: i64 = context_core::EXACT_FIRST_MAX_TARGETS;
/// Maximum worker-attributed RSS.
pub const P14_MAX_RSS_BYTES: usize = 512 * 1024 * 1024;
/// Maximum retained temporary bytes for one job.
pub const P14_MAX_TEMP_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Maximum WAL bytes attributed to the frozen required workload.
pub const P14_MAX_WAL_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Maximum storage bytes attributed to one full-precision optimization.
pub const P14_MAX_STORAGE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// Maximum lock acquisitions in one bounded backfill/publication step.
pub const P14_MAX_LOCKS_PER_STEP: usize = 64;
/// Maximum exclusive publication-lock duration.
pub const P14_MAX_PUBLICATION_LOCK_MICROS: u64 = 250_000;
/// Maximum SQL statements in one bounded backfill step.
pub const P14_MAX_STATEMENTS_PER_STEP: usize = 8;
/// Maximum complete required-lane elapsed time.
pub const P14_MAX_ELAPSED_MICROS: u64 = 4 * 60 * 60 * 1_000_000;
/// Maximum cancellation acknowledgement time.
pub const P14_MAX_CANCEL_MICROS: u64 = 5_000_000;
/// Maximum build lease duration.
pub const P14_MAX_LEASE_MILLIS: i32 = context_core::EXACT_FIRST_MAX_LEASE_MILLIS;
/// Maximum publication convergence time after source writes stop.
pub const P14_MAX_CONVERGENCE_MICROS: u64 = 60_000_000;
/// Minimum rows before ANN advice is eligible.
pub const P14_MIN_ANN_ROWS: u64 = context_core::EXACT_FIRST_MIN_ANN_ROWS;
/// Minimum rows before IVFFlat advice is eligible.
pub const P14_MIN_IVF_ROWS: u64 = context_core::EXACT_FIRST_MIN_IVF_ROWS;
/// Churn threshold favoring HNSW, in thousandths of an update per second.
pub const P14_HIGH_CHURN_MILLIHERTZ: u64 = context_core::EXACT_FIRST_HIGH_CHURN_MILLIHERTZ;
/// Selectivity threshold favoring HNSW candidate widening.
pub const P14_SELECTIVE_FILTER_BPS: u16 = context_core::EXACT_FIRST_SELECTIVE_FILTER_BPS;
/// Minimum declared build window for IVFFlat.
pub const P14_MIN_IVF_BUILD_WINDOW_SECONDS: u64 =
    context_core::EXACT_FIRST_MIN_IVF_BUILD_WINDOW_SECONDS;
/// Required exact-oracle equality marker.
pub const P14_EXACT_ORACLE_CONTRACT: &str = "bit_exact_scores_and_membership_v1";
/// Minimum recall in basis points for a promoted ANN recommendation.
pub const P14_MIN_RECALL_BPS: u16 = 9_500;
/// Minimum required-lane set-based backfill throughput.
pub const P14_MIN_BACKFILL_ROWS_PER_SECOND: usize = 5_000;
/// Maximum exact-query p95 during an active build.
pub const P14_MAX_BUILDING_QUERY_P95_MICROS: u64 = 250_000;
/// Maximum indexed-query p95 after publication.
pub const P14_MAX_INDEXED_QUERY_P95_MICROS: u64 = 100_000;
/// Maximum postings admitted by one required-lane IVFFlat verification query.
pub const P14_INDEXED_CANDIDATE_BUDGET: usize = context_core::policy::MAX_IVFFLAT_CANDIDATE_BUDGET;
/// Frozen required dataset rows.
pub const P14_REQUIRED_DATASET_ROWS: usize = 10_000_000;
/// Deterministic dataset generator revision.
pub const P14_DATASET_REVISION: &str = "p14-10m-exact-first-v1";
/// Canonical dataset generator specification hashed by every heavy lane.
pub const P14_DATASET_GENERATOR_SPEC: &str = "p14_dataset_v1|rows=manifest|required_columns=id:bigint,tenant:int,embedding:vector(2),body:text|embedding=[id%1000,id/1000]|tenant=id%8|ordered=id";
/// SHA-256 of the frozen dataset generator specification.
pub const P14_DATASET_GENERATOR_SHA256: &str =
    "058f9a889dfdad39c5f1abfe242c1b713b64e2b6bb9cbf06ef80b96335bb06bf";
/// Deterministic concurrent workload revision.
pub const P14_WORKLOAD_REVISION: &str = "p14-exact-first-workload-v1";
/// Canonical lifecycle workload specification hashed by every heavy lane.
pub const P14_WORKLOAD_SPEC: &str = "p14_workload_v1|register|exact_samples=8|advisor_concurrency|enqueue|claim|top_level_cic|concurrent_copy_insert_update_key_update_delete_query|publish|indexed_score_oracle|acl_rls|cancel|immediate_restart|retry|fail";
/// SHA-256 of the frozen concurrent workload specification.
pub const P14_WORKLOAD_SHA256: &str =
    "80ffb9cf92e26d502fc89e898b8ea8e05ad12f743517b00d95c0884981b17abb";
/// PostgreSQL majors required for promotion.
pub const P14_REQUIRED_PG_MAJORS: [u16; 2] = [17, 18];
/// Required retained report markers.
pub const P14_REPORT_MARKERS: [&str; 10] = [
    "exact_first_environment",
    "exact_first_manifest",
    "exact_first_exact_oracle",
    "exact_first_state_samples",
    "exact_first_backfill",
    "exact_first_concurrency",
    "exact_first_resources",
    "exact_first_recovery",
    "exact_first_security",
    "exact_first_decision",
];

/// One frozen exact-first certification lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct P14ExactFirstGate {
    /// Source rows.
    pub rows: usize,
    /// Required lane disposition.
    pub status: &'static str,
    /// Reproducible command with a PostgreSQL-major placeholder.
    pub command: &'static str,
    /// Required decision marker.
    pub report_marker: &'static str,
}

/// Blocking 10M lanes on both required PostgreSQL majors.
pub const P14_EXACT_FIRST_GATES: [P14ExactFirstGate; 2] = [
    P14ExactFirstGate {
        rows: P14_REQUIRED_DATASET_ROWS,
        status: "required_pg17",
        command: "PG_VERSION=pg17 PG_FEATURE=pg17 ROW_COUNT=10000000 DBNAME=pgcontext_p14_10m_pg17 ./tests/heavy/exact_first_readiness.sh",
        report_marker: "exact_first_decision",
    },
    P14ExactFirstGate {
        rows: P14_REQUIRED_DATASET_ROWS,
        status: "required_pg18",
        command: "PG_VERSION=pg18 PG_FEATURE=pg18 ROW_COUNT=10000000 DBNAME=pgcontext_p14_10m_pg18 ./tests/heavy/exact_first_readiness.sh",
        report_marker: "exact_first_decision",
    },
];

/// Returns a stable FNV-1a identity over every frozen P14 field.
#[must_use]
pub fn p14_exact_first_manifest_hash() -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for value in [
        P14_MAX_COLUMNS,
        P14_MAX_INDEXES,
        P14_MAX_ERROR_CODE_BYTES,
        P14_MAX_SPEC_BYTES,
        P14_MAX_OBJECTIVES_BYTES,
        P14_MAX_JSON_NODES,
        P14_MAX_JSON_DEPTH,
        P14_MAX_NAME_BYTES,
        P14_MAX_DDL_BYTES,
        P14_MAX_INVALID_SAMPLES,
        P14_MAX_SOURCE_KEY_BYTES,
        P14_MAX_BATCH_ROWS,
        P14_MAX_BATCH_BYTES,
        P14_MAX_ACTIVE_JOBS,
        P14_MAX_RSS_BYTES,
        P14_MAX_LOCKS_PER_STEP,
        P14_MAX_STATEMENTS_PER_STEP,
        P14_MIN_BACKFILL_ROWS_PER_SECOND,
        P14_INDEXED_CANDIDATE_BUDGET,
        P14_REQUIRED_DATASET_ROWS,
    ] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    for value in [
        P14_MAX_TEMP_BYTES,
        P14_MAX_WAL_BYTES,
        P14_MAX_STORAGE_BYTES,
        P14_MAX_PUBLICATION_LOCK_MICROS,
        P14_MAX_ELAPSED_MICROS,
        P14_MAX_CANCEL_MICROS,
        P14_MAX_CONVERGENCE_MICROS,
        P14_MIN_ANN_ROWS,
        P14_MIN_IVF_ROWS,
        P14_HIGH_CHURN_MILLIHERTZ,
        P14_MIN_IVF_BUILD_WINDOW_SECONDS,
        P14_MAX_BUILDING_QUERY_P95_MICROS,
        P14_MAX_INDEXED_QUERY_P95_MICROS,
    ] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    hash = fnv1a(hash, &P14_MAX_PLAN_REVISIONS.to_le_bytes());
    hash = fnv1a(hash, &P14_MAX_TARGETS.to_le_bytes());
    for value in [P14_MAX_ATTEMPTS, P14_MAX_LEASE_MILLIS] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    for value in [P14_SELECTIVE_FILTER_BPS, P14_MIN_RECALL_BPS] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    for value in [
        P14_REGISTRATION_CONTRACT,
        P14_ADVISOR_CONTRACT,
        P14_BUILD_PLAN_CONTRACT,
        P14_EXACT_ORACLE_CONTRACT,
        P14_DATASET_REVISION,
        P14_DATASET_GENERATOR_SPEC,
        P14_DATASET_GENERATOR_SHA256,
        P14_WORKLOAD_REVISION,
        P14_WORKLOAD_SPEC,
        P14_WORKLOAD_SHA256,
    ] {
        hash = fnv1a(hash, value.as_bytes());
    }
    for value in P14_READINESS_STATES
        .into_iter()
        .chain(P14_APPLY_POLICIES)
        .chain(P14_SOURCE_FAMILIES)
        .chain(P14_OPTIMIZATION_FAMILIES)
        .chain(P14_REPORT_MARKERS)
    {
        hash = fnv1a(hash, value.as_bytes());
    }
    for major in P14_REQUIRED_PG_MAJORS {
        hash = fnv1a(hash, &major.to_le_bytes());
    }
    for gate in P14_EXACT_FIRST_GATES {
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
