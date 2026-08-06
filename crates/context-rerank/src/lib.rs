//! Transport-neutral external reranking.
//!
//! This crate owns everything about talking to an external cross-encoder
//! *except* which one. It defines the wire format, a backend port, batching,
//! response validation, and a deterministic backend that needs no model at all.
//! Selecting a real inference runtime is a later, additive change: one more
//! [`RerankBackend`] implementation, not a change to this crate's shape.
//!
//! # Trust boundary
//!
//! A provider is untrusted infrastructure. A [`RerankBackend`] can return only
//! [`WireRerankResponse`] — plain data with no invariants. Turning that into
//! scores means going through [`WireRerankResponse::to_response`], which
//! re-establishes the validated [`context_query::RerankResponse`] contract, and
//! then [`context_query::validate_rerank_response`], which checks it against the
//! request that produced it. [`score_batch`] is both steps in one call, and is
//! the path [`BackendReranker`] takes. A backend has no way to manufacture a
//! validated score, so no implementation can skip validation by accident.
//!
//! The validated types live in `context-query` because the executor owns the
//! port; this crate owns their serialized form and the transport around them.
//!
//! # Where this plugs in
//!
//! [`BackendReranker`] adapts a backend to [`context_query::ExternalReranker`],
//! the reranking port the executor already wires up. Deciding what a row is
//! allowed to release is left to an [`AuthorizedRowSource`], because only the
//! component with database authority can answer that.

#![warn(missing_docs)]
#![warn(rustdoc::bare_urls)]
#![warn(rustdoc::broken_intra_doc_links)]

mod adapter;
mod backend;
mod wire;

pub use adapter::{AuthorizedRowSource, BackendReranker};
pub use backend::{
    BackendResult, DeterministicRerankBackend, MAX_RERANK_BATCH, RerankBackend, RerankBackendError,
    RerankBatches, RerankOutcome, score_all, score_batch,
};
pub use wire::{
    MAX_RERANK_DIAGNOSTIC_BYTES, WireError, WireRerankCandidate, WireRerankMetadata,
    WireRerankRequest, WireRerankResponse, WireRerankScore, WireResult, bound_diagnostic,
};
