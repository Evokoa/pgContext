//! Query-owned synchronous infrastructure ports.

use crate::{
    Candidate, CandidatePage, FilterCandidateBatch, HydratedCandidate, QueryIr, Result,
    SourceReadiness, StageDiagnostic,
};

/// Cooperative cancellation/interrupt hook checked at every port boundary.
pub trait Cancellation {
    /// Runs an adapter-specific interrupt checkpoint.
    ///
    /// PostgreSQL adapters use this hook for backend interrupts while pure
    /// tests use the default no-op implementation.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral interrupt error.
    fn check_interrupt(&self) -> Result<()> {
        Ok(())
    }

    /// Returns true when execution should stop before the next port call.
    fn is_cancelled(&self) -> bool;
}

/// Candidate-generation source such as exact, HNSW, sparse, or mmap search.
pub trait CandidateSource {
    /// Reports source readiness without performing candidate work.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral port error.
    fn readiness(&mut self, query: &QueryIr) -> Result<SourceReadiness>;

    /// Returns an owned bounded candidate page.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral port error.
    fn candidates(
        &mut self,
        query: &QueryIr,
        filter: Option<&FilterCandidateBatch>,
        limit: usize,
    ) -> Result<CandidatePage>;
}

/// Adapter that derives logical candidates from a public filter.
pub trait FilterCandidateSource {
    /// Returns an owned bounded logical-ID batch.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral port error.
    fn filter_candidates(&mut self, query: &QueryIr, limit: usize) -> Result<FilterCandidateBatch>;
}

/// Adapter that hydrates and rechecks candidates against authoritative rows.
pub trait SourceRechecker {
    /// Returns only candidates whose source rows remain visible and valid.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral port error.
    fn recheck(
        &mut self,
        query: &QueryIr,
        candidates: &[Candidate],
        limit: usize,
    ) -> Result<Vec<HydratedCandidate>>;
}

/// Bounded telemetry sink that receives no vectors, filters, or payloads.
pub trait TelemetrySink {
    /// Records one bounded stage diagnostic.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral port error.
    fn record(&mut self, diagnostic: &StageDiagnostic) -> Result<()>;
}
