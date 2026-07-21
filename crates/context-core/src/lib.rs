//! Framework-free domain types and policies for pgContext.
//!
//! This crate owns core vocabulary that must compile and test without
//! PostgreSQL. Later milestones add vector representations, distance metrics,
//! query plans, and typed errors here before SQL adapters expose them.

pub mod policy;

mod catalog;
mod error;
mod exact;
mod identity;
mod metric;
mod metric_kernels;
mod scroll;
mod vector;

pub use catalog::{
    CollectionName, QualifiedTableName, SourceKey, SqlIdentifier, VectorDimensions, VectorName,
};
pub use error::{ContextError, Error, Result};
pub use exact::{ExactSearchItem, ScoredPoint, SearchLimit, exact_top_k};
pub use identity::PointId;
pub use metric::DistanceMetric;
pub use scroll::{ScrollCursor, ScrollCursorError};
pub use vector::{
    BitVector, DenseVector, HalfVector, SparseEntry, SparseVector, VectorConversionPolicy,
    VectorRepresentation, f32_to_half_bits, half_bits_to_f32,
};

/// Returns the package version compiled into this crate.
#[must_use]
pub const fn crate_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
