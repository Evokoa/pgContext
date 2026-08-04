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
mod identity;
mod integer_metric_kernels;
mod matryoshka;
mod metric;
mod metric_kernels;
mod retrieval;
mod scroll;
mod vector;

pub use catalog::{
    CollectionName, QualifiedTableName, SourceKey, SqlIdentifier, VectorDimensions, VectorName,
};
pub use embedding_profile::{
    EmbeddingProfile, IntegerScale, ProviderBinaryLayout, VectorNormalization,
};
pub use error::{ContextError, Error, Result};
pub use exact::{ExactSearchItem, ScoredPoint, SearchLimit, exact_top_k};
pub use identity::PointId;
pub use matryoshka::{MAX_MATRYOSHKA_PREFIXES, MatryoshkaPolicy, PrefixDimensions};
pub use metric::DistanceMetric;
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
