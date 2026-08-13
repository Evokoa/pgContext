//! Exact-first optimization readiness and advisor policy.

/// Maximum user columns inspected or registered for one source.
pub const EXACT_FIRST_MAX_COLUMNS: usize = 256;
/// Maximum source indexes admitted by one introspection pass.
pub const EXACT_FIRST_MAX_INDEXES: usize = 256;
/// Maximum encoded registration specification bytes.
pub const EXACT_FIRST_MAX_SPEC_BYTES: usize = 1024 * 1024;
/// Maximum encoded advisor/inspection objective bytes.
pub const EXACT_FIRST_MAX_OBJECTIVES_BYTES: usize = 256 * 1024;
/// Maximum structural nodes in one admitted JSON input.
pub const EXACT_FIRST_MAX_JSON_NODES: usize = 16 * 1024;
/// Maximum nesting depth in one admitted JSON input.
pub const EXACT_FIRST_MAX_JSON_DEPTH: usize = 64;
/// Maximum public identifier or worker identifier bytes.
pub const EXACT_FIRST_MAX_NAME_BYTES: usize = 128;
/// Maximum generated DDL bytes in one frozen plan.
pub const EXACT_FIRST_MAX_DDL_BYTES: usize = 64 * 1024;
/// Maximum retained immutable plan revisions per registration.
pub const EXACT_FIRST_MAX_PLAN_REVISIONS: i64 = 16;
/// Maximum retained optimization targets per registration.
///
/// This matches the immutable plan-revision ceiling so catalog retention never
/// discards the identity of a physical index that still exists.
pub const EXACT_FIRST_MAX_TARGETS: i64 = EXACT_FIRST_MAX_PLAN_REVISIONS;
/// Maximum build attempts for one operational plan run.
pub const EXACT_FIRST_MAX_ATTEMPTS: i32 = 3;
/// Maximum build lease duration.
pub const EXACT_FIRST_MAX_LEASE_MILLIS: i32 = 60_000;
/// Maximum content-free operational error-code bytes.
pub const EXACT_FIRST_MAX_ERROR_CODE_BYTES: usize = 64;
/// Minimum corpus size at which ANN advice is eligible.
pub const EXACT_FIRST_MIN_ANN_ROWS: u64 = 10_000;
/// Minimum corpus size at which IVFFlat advice is eligible.
pub const EXACT_FIRST_MIN_IVF_ROWS: u64 = 1_000_000;
/// Churn threshold favoring HNSW, in thousandths of an update per second.
pub const EXACT_FIRST_HIGH_CHURN_MILLIHERTZ: u64 = 10_000;
/// Selectivity threshold favoring HNSW candidate widening.
pub const EXACT_FIRST_SELECTIVE_FILTER_BPS: u16 = 500;
/// Minimum declared build window for IVFFlat.
pub const EXACT_FIRST_MIN_IVF_BUILD_WINDOW_SECONDS: u64 = 3_600;

/// Collection-level optimization readiness.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExactFirstState {
    /// Complete exact serving is available without a published optimization.
    ExactOnly,
    /// Complete exact serving remains available while optimization work runs.
    Building,
    /// A validated current optimization may serve compatible query shapes.
    Indexed,
    /// The registered source contract drifted and must be repaired.
    Stale,
    /// Exact serving remains available after an operational optimization failure.
    Degraded,
}

impl ExactFirstState {
    /// Parses the stable catalog label.
    #[must_use]
    pub const fn from_catalog(value: &str) -> Option<Self> {
        match value.as_bytes() {
            b"exact_only" => Some(Self::ExactOnly),
            b"building" => Some(Self::Building),
            b"indexed" => Some(Self::Indexed),
            b"stale" => Some(Self::Stale),
            b"degraded" => Some(Self::Degraded),
            _ => None,
        }
    }

    /// Returns the stable catalog label.
    #[must_use]
    pub const fn as_catalog(self) -> &'static str {
        match self {
            Self::ExactOnly => "exact_only",
            Self::Building => "building",
            Self::Indexed => "indexed",
            Self::Stale => "stale",
            Self::Degraded => "degraded",
        }
    }
}

/// Content-free reason paired with an exact-first readiness state.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExactFirstReason {
    /// The current source is safe for exact serving but has no optimization.
    CurrentExactPath,
    /// A current-revision optimization job is active.
    BuildActive,
    /// A compatible validated optimization is published.
    OptimizationReady,
    /// The registered relation identity changed or disappeared.
    SourceRelationChanged,
    /// The source-key contract changed.
    SourceKeyChanged,
    /// A registered source column changed.
    SourceColumnChanged,
    /// Metric, profile, text configuration, or normalized specification changed.
    ConfigurationChanged,
    /// No complete safe exact path exists for the current registration.
    ExactPathUnavailable,
    /// Optimization work failed operationally.
    BuildFailed,
    /// Optimization work was cancelled.
    BuildCancelled,
    /// A published or staged optimization failed structural verification.
    OptimizationCorrupt,
    /// A resource admission bound rejected optimization work.
    ResourceLimit,
    /// Recall validation rejected the optimization.
    RecallRejected,
}

impl ExactFirstReason {
    /// Parses the stable catalog label.
    #[must_use]
    pub const fn from_catalog(value: &str) -> Option<Self> {
        match value.as_bytes() {
            b"current_exact_path" => Some(Self::CurrentExactPath),
            b"build_active" => Some(Self::BuildActive),
            b"optimization_ready" => Some(Self::OptimizationReady),
            b"source_relation_changed" => Some(Self::SourceRelationChanged),
            b"source_key_changed" => Some(Self::SourceKeyChanged),
            b"source_column_changed" => Some(Self::SourceColumnChanged),
            b"configuration_changed" => Some(Self::ConfigurationChanged),
            b"exact_path_unavailable" => Some(Self::ExactPathUnavailable),
            b"build_failed" => Some(Self::BuildFailed),
            b"build_cancelled" => Some(Self::BuildCancelled),
            b"optimization_corrupt" => Some(Self::OptimizationCorrupt),
            b"resource_limit" => Some(Self::ResourceLimit),
            b"recall_rejected" => Some(Self::RecallRejected),
            _ => None,
        }
    }

    /// Returns the stable catalog label.
    #[must_use]
    pub const fn as_catalog(self) -> &'static str {
        match self {
            Self::CurrentExactPath => "current_exact_path",
            Self::BuildActive => "build_active",
            Self::OptimizationReady => "optimization_ready",
            Self::SourceRelationChanged => "source_relation_changed",
            Self::SourceKeyChanged => "source_key_changed",
            Self::SourceColumnChanged => "source_column_changed",
            Self::ConfigurationChanged => "configuration_changed",
            Self::ExactPathUnavailable => "exact_path_unavailable",
            Self::BuildFailed => "build_failed",
            Self::BuildCancelled => "build_cancelled",
            Self::OptimizationCorrupt => "optimization_corrupt",
            Self::ResourceLimit => "resource_limit",
            Self::RecallRejected => "recall_rejected",
        }
    }
}

/// Explicit mutation policy for one frozen exact-first plan.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExactFirstApplyPolicy {
    /// Return evidence and DDL without enqueueing or applying it.
    RecommendOnly,
    /// Register exact serving and intentionally create no optimization.
    ExactOnly,
    /// Enqueue the frozen plan in the supervised job system.
    Enqueue,
    /// Apply one frozen plan synchronously under its publication checks.
    ApplyForeground,
}

impl ExactFirstApplyPolicy {
    /// Parses the stable SQL label.
    #[must_use]
    pub const fn from_catalog(value: &str) -> Option<Self> {
        match value.as_bytes() {
            b"recommend_only" => Some(Self::RecommendOnly),
            b"exact_only" => Some(Self::ExactOnly),
            b"enqueue" => Some(Self::Enqueue),
            b"apply_foreground" => Some(Self::ApplyForeground),
            _ => None,
        }
    }

    /// Returns the stable SQL label.
    #[must_use]
    pub const fn as_catalog(self) -> &'static str {
        match self {
            Self::RecommendOnly => "recommend_only",
            Self::ExactOnly => "exact_only",
            Self::Enqueue => "enqueue",
            Self::ApplyForeground => "apply_foreground",
        }
    }
}

/// Operational failure that leaves authoritative exact serving available.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExactFirstFailure {
    /// The build attempt failed.
    BuildFailed,
    /// The build was cancelled.
    BuildCancelled,
    /// Validation found corrupt derived state.
    OptimizationCorrupt,
    /// Resource admission rejected work.
    ResourceLimit,
    /// Exact-oracle recall validation failed.
    RecallRejected,
}

impl ExactFirstFailure {
    const fn reason(self) -> ExactFirstReason {
        match self {
            Self::BuildFailed => ExactFirstReason::BuildFailed,
            Self::BuildCancelled => ExactFirstReason::BuildCancelled,
            Self::OptimizationCorrupt => ExactFirstReason::OptimizationCorrupt,
            Self::ResourceLimit => ExactFirstReason::ResourceLimit,
            Self::RecallRejected => ExactFirstReason::RecallRejected,
        }
    }
}

/// Facts used to derive one exact-first readiness result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactFirstReadinessFacts {
    /// The relation binding still matches.
    pub relation_current: bool,
    /// The source-key binding still matches.
    pub source_key_current: bool,
    /// Every registered source-column binding still matches.
    pub source_columns_current: bool,
    /// Metric/profile/configuration bindings still match.
    pub configuration_current: bool,
    /// A complete invoker-authoritative exact path is safe.
    pub exact_path_available: bool,
    /// A current-revision optimization job is active.
    pub build_active: bool,
    /// A current compatible optimization passed publication checks.
    pub optimization_ready: bool,
    /// Most recent current-revision operational failure.
    pub failure: Option<ExactFirstFailure>,
}

/// Derived exact-first readiness and bounded diagnostic reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactFirstReadiness {
    /// Five-state readiness value.
    pub state: ExactFirstState,
    /// Stable content-free reason.
    pub reason: ExactFirstReason,
}

/// Derives readiness with source safety taking precedence over optimization state.
#[must_use]
pub const fn derive_exact_first_readiness(facts: ExactFirstReadinessFacts) -> ExactFirstReadiness {
    if !facts.relation_current {
        return readiness(
            ExactFirstState::Stale,
            ExactFirstReason::SourceRelationChanged,
        );
    }
    if !facts.source_key_current {
        return readiness(ExactFirstState::Stale, ExactFirstReason::SourceKeyChanged);
    }
    if !facts.source_columns_current {
        return readiness(
            ExactFirstState::Stale,
            ExactFirstReason::SourceColumnChanged,
        );
    }
    if !facts.configuration_current {
        return readiness(
            ExactFirstState::Stale,
            ExactFirstReason::ConfigurationChanged,
        );
    }
    if !facts.exact_path_available {
        return readiness(
            ExactFirstState::Stale,
            ExactFirstReason::ExactPathUnavailable,
        );
    }
    if facts.build_active {
        return readiness(ExactFirstState::Building, ExactFirstReason::BuildActive);
    }
    if let Some(failure) = facts.failure {
        return readiness(ExactFirstState::Degraded, failure.reason());
    }
    if facts.optimization_ready {
        return readiness(
            ExactFirstState::Indexed,
            ExactFirstReason::OptimizationReady,
        );
    }
    readiness(
        ExactFirstState::ExactOnly,
        ExactFirstReason::CurrentExactPath,
    )
}

const fn readiness(state: ExactFirstState, reason: ExactFirstReason) -> ExactFirstReadiness {
    ExactFirstReadiness { state, reason }
}

/// Recommended acceleration family.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExactFirstIndexRecommendation {
    /// Keep the authoritative exact path only.
    ExactOnly,
    /// Build a pgContext HNSW index.
    Hnsw,
    /// Build a pgContext IVFFlat index.
    IvfFlat,
}

impl ExactFirstIndexRecommendation {
    /// Returns the stable catalog label.
    #[must_use]
    pub const fn as_catalog(self) -> &'static str {
        match self {
            Self::ExactOnly => "exact",
            Self::Hnsw => "hnsw",
            Self::IvfFlat => "ivfflat",
        }
    }
}

/// Precision policy selected for an ANN recommendation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExactFirstPrecisionRecommendation {
    /// Preserve the source representation without a derived codec.
    Full,
    /// Use a previously certified scalar/SQ8 codec.
    ScalarQuantized,
    /// Use a previously certified product codec.
    ProductQuantized,
    /// Use a certified Matryoshka prefix for candidate generation.
    Prefix,
}

impl ExactFirstPrecisionRecommendation {
    /// Returns the stable catalog label.
    #[must_use]
    pub const fn as_catalog(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::ScalarQuantized => "scalar_quantized",
            Self::ProductQuantized => "product_quantized",
            Self::Prefix => "prefix",
        }
    }
}

/// Stable reason supporting an advisor decision.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExactFirstAdvisorReason {
    /// The corpus is below the minimum useful ANN size.
    SmallCorpus,
    /// High churn favors bounded HNSW maintenance.
    HighChurn,
    /// Selective filters favor HNSW candidate widening.
    SelectiveFilters,
    /// Large low-churn data fits the IVF build window.
    LargeLowChurnCorpus,
    /// Full-precision retained bytes fit the declared memory budget.
    FullPrecisionFits,
    /// A certified prefix meets the requested memory policy.
    CertifiedPrefix,
    /// A certified scalar codec meets the requested memory policy.
    CertifiedScalarCodec,
    /// A certified product codec is required to meet the memory policy.
    CertifiedProductCodec,
    /// No certified optimization fits the declared objectives and resources.
    InsufficientResources,
}

impl ExactFirstAdvisorReason {
    /// Returns the stable catalog label.
    #[must_use]
    pub const fn as_catalog(self) -> &'static str {
        match self {
            Self::SmallCorpus => "small_corpus",
            Self::HighChurn => "high_churn",
            Self::SelectiveFilters => "selective_filters",
            Self::LargeLowChurnCorpus => "large_low_churn_corpus",
            Self::FullPrecisionFits => "full_precision_fits",
            Self::CertifiedPrefix => "certified_prefix",
            Self::CertifiedScalarCodec => "certified_scalar_codec",
            Self::CertifiedProductCodec => "certified_product_codec",
            Self::InsufficientResources => "insufficient_resources",
        }
    }
}

/// Bounded evidence consumed by the pure advisor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactFirstAdvisorInput {
    /// Estimated authoritative rows.
    pub rows: u64,
    /// Vector dimensions, or zero for a non-vector source.
    pub dimensions: u32,
    /// Source mutations per second in thousandths.
    pub update_millihertz: u64,
    /// Most selective common filter in basis points of rows retained.
    pub filter_selectivity_bps: u16,
    /// Retained-memory objective.
    pub memory_budget_bytes: u64,
    /// Allowed build window.
    pub build_window_seconds: u64,
    /// Whether a scalar codec passed the representation/metric quality gate.
    pub scalar_codec_certified: bool,
    /// Whether a product codec passed the representation/metric quality gate.
    pub product_codec_certified: bool,
    /// Whether a Matryoshka prefix passed its quality gate.
    pub prefix_certified: bool,
}

/// Manifest-owned advisor thresholds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactFirstAdvisorPolicy {
    /// Minimum rows before ANN is recommended.
    pub min_ann_rows: u64,
    /// Minimum rows before IVF may be recommended.
    pub min_ivf_rows: u64,
    /// Churn threshold favoring HNSW, in thousandths of an update per second.
    pub high_churn_millihertz: u64,
    /// Filter selectivity at or below which HNSW is favored.
    pub selective_filter_bps: u16,
    /// Minimum declared build window for IVF.
    pub min_ivf_build_window_seconds: u64,
}

/// Advisor output with no generated SQL or PostgreSQL identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExactFirstAdvisorDecision {
    /// Selected index family.
    pub index: ExactFirstIndexRecommendation,
    /// Selected precision policy.
    pub precision: ExactFirstPrecisionRecommendation,
    /// Primary evidence-backed reason.
    pub reason: ExactFirstAdvisorReason,
}

/// Returns a conservative evidence-backed optimization decision.
#[must_use]
pub fn advise_exact_first(
    input: ExactFirstAdvisorInput,
    policy: ExactFirstAdvisorPolicy,
) -> ExactFirstAdvisorDecision {
    if input.rows < policy.min_ann_rows || input.dimensions == 0 {
        return advisor_decision(
            ExactFirstIndexRecommendation::ExactOnly,
            ExactFirstPrecisionRecommendation::Full,
            ExactFirstAdvisorReason::SmallCorpus,
        );
    }

    let full_bytes = input
        .rows
        .checked_mul(u64::from(input.dimensions))
        .and_then(|bytes| bytes.checked_mul(4));
    let (precision, precision_reason) =
        if full_bytes.is_some_and(|bytes| bytes <= input.memory_budget_bytes) {
            (
                ExactFirstPrecisionRecommendation::Full,
                ExactFirstAdvisorReason::FullPrecisionFits,
            )
        } else if input.prefix_certified {
            (
                ExactFirstPrecisionRecommendation::Prefix,
                ExactFirstAdvisorReason::CertifiedPrefix,
            )
        } else if input.scalar_codec_certified {
            (
                ExactFirstPrecisionRecommendation::ScalarQuantized,
                ExactFirstAdvisorReason::CertifiedScalarCodec,
            )
        } else if input.product_codec_certified {
            (
                ExactFirstPrecisionRecommendation::ProductQuantized,
                ExactFirstAdvisorReason::CertifiedProductCodec,
            )
        } else {
            return advisor_decision(
                ExactFirstIndexRecommendation::ExactOnly,
                ExactFirstPrecisionRecommendation::Full,
                ExactFirstAdvisorReason::InsufficientResources,
            );
        };

    let (index, index_reason) = if input.update_millihertz >= policy.high_churn_millihertz {
        (
            ExactFirstIndexRecommendation::Hnsw,
            ExactFirstAdvisorReason::HighChurn,
        )
    } else if input.filter_selectivity_bps <= policy.selective_filter_bps {
        (
            ExactFirstIndexRecommendation::Hnsw,
            ExactFirstAdvisorReason::SelectiveFilters,
        )
    } else if input.rows >= policy.min_ivf_rows
        && input.build_window_seconds >= policy.min_ivf_build_window_seconds
    {
        (
            ExactFirstIndexRecommendation::IvfFlat,
            ExactFirstAdvisorReason::LargeLowChurnCorpus,
        )
    } else {
        (ExactFirstIndexRecommendation::Hnsw, precision_reason)
    };
    advisor_decision(index, precision, index_reason)
}

const fn advisor_decision(
    index: ExactFirstIndexRecommendation,
    precision: ExactFirstPrecisionRecommendation,
    reason: ExactFirstAdvisorReason,
) -> ExactFirstAdvisorDecision {
    ExactFirstAdvisorDecision {
        index,
        precision,
        reason,
    }
}
