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

    /// Returns the bounded number of candidates this leaf should request.
    ///
    /// The default exposes the remaining global allowance for compatibility
    /// with sources whose candidate pool is independent of the result limit.
    /// Production adapters should override this with a per-leaf request so one
    /// branch cannot reserve work intended for its siblings.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral port error when the adapter cannot derive a
    /// valid request for this query shape.
    fn candidate_limit(&mut self, _query: &QueryIr, remaining: usize) -> Result<usize> {
        Ok(remaining)
    }

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
    /// Returns the bounded filter-candidate request for one leaf.
    fn candidate_limit(&mut self, _query: &QueryIr, remaining: usize) -> Result<usize> {
        Ok(remaining)
    }

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
