//! Framework-free domain types and policies for pgContext.
//!
//! This crate owns core vocabulary that must compile and test without
//! PostgreSQL. Later milestones add vector representations, distance metrics,
//! query plans, and typed errors here before SQL adapters expose them.

pub mod policy;

mod catalog;
mod embedding_profile;
mod error;
mod exact;
mod exact_first;
mod identity;
mod integer_metric_kernels;
mod lazy_cursor;
mod matryoshka;
mod metric;
mod metric_kernels;
mod profile_lifecycle;
mod retrieval;
mod scroll;
mod vector;

pub use catalog::{
    CollectionName, ProfileName, QualifiedTableName, SourceKey, SqlIdentifier, VectorDimensions,
    VectorName,
};
pub use embedding_profile::{
    EmbeddingProfile, IntegerScale, ProviderBinaryLayout, VectorNormalization,
};
pub use error::{ContextError, Error, Result};
pub use exact::{ExactSearchItem, ScoredPoint, SearchLimit, exact_top_k};
pub use exact_first::{
    EXACT_FIRST_HIGH_CHURN_MILLIHERTZ, EXACT_FIRST_MAX_ATTEMPTS, EXACT_FIRST_MAX_COLUMNS,
    EXACT_FIRST_MAX_DDL_BYTES, EXACT_FIRST_MAX_ERROR_CODE_BYTES, EXACT_FIRST_MAX_INDEXES,
    EXACT_FIRST_MAX_JSON_DEPTH, EXACT_FIRST_MAX_JSON_NODES, EXACT_FIRST_MAX_LEASE_MILLIS,
    EXACT_FIRST_MAX_NAME_BYTES, EXACT_FIRST_MAX_OBJECTIVES_BYTES, EXACT_FIRST_MAX_PLAN_REVISIONS,
    EXACT_FIRST_MAX_SPEC_BYTES, EXACT_FIRST_MAX_TARGETS, EXACT_FIRST_MIN_ANN_ROWS,
    EXACT_FIRST_MIN_IVF_BUILD_WINDOW_SECONDS, EXACT_FIRST_MIN_IVF_ROWS,
    EXACT_FIRST_SELECTIVE_FILTER_BPS, ExactFirstAdvisorDecision, ExactFirstAdvisorInput,
    ExactFirstAdvisorPolicy, ExactFirstAdvisorReason, ExactFirstApplyPolicy, ExactFirstFailure,
    ExactFirstIndexRecommendation, ExactFirstPrecisionRecommendation, ExactFirstReadiness,
    ExactFirstReadinessFacts, ExactFirstReason, ExactFirstState, advise_exact_first,
    derive_exact_first_readiness,
};
pub use identity::PointId;
pub use lazy_cursor::{DEFAULT_LAZY_CURSOR_BATCH, LazyCursorTermination, MAX_LAZY_CURSOR_BATCH};
pub use matryoshka::{MAX_MATRYOSHKA_PREFIXES, MatryoshkaPolicy, PrefixDimensions};
pub use metric::DistanceMetric;
pub use profile_lifecycle::ProfileLifecycle;
pub use retrieval::{
    Completion, ConfigurationRevision, GenerationId, IndexKind, OccurrenceId, ProfileId,
    ReadinessReason, ScoreOrder, SourceAuthority, SourceVersion,
};
pub use scroll::{ScrollCursor, ScrollCursorError};
pub use vector::{
    BitVector, DenseVector, HalfVector, Int8Vector, ProviderBinaryVector, ProviderBitOrder,
    ProviderByteOrder, SparseEntry, SparseVector, UInt8Vector, VectorConversionPolicy,
    VectorRepresentation, f32_to_half_bits, half_bits_to_f32,
};

/// Returns the package version compiled into this crate.
#[must_use]
pub const fn crate_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
