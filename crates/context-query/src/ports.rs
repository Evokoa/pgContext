//! Query-owned synchronous infrastructure ports.

use crate::{
    Candidate, CandidatePage, ExternalRerankPage, FilterCandidateBatch, HydratedCandidate,
    LazyCursorAdvance, LazyCursorPage, LazyCursorTermination, LazyCursorWork, QueryIr, RecheckPage,
    Result, SourceReadiness, StageDiagnostic,
};

/// Remaining hard resources supplied to one infrastructure-port call.
///
/// Ports must bound their work to these values. The executor independently
/// validates returned work so an adapter cannot turn a cooperative limit into
/// silent partial execution or budget overrun.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PortBudget {
    max_comparisons: usize,
    max_memory_bytes: usize,
    max_hydration_bytes: usize,
    remaining_elapsed_micros: u64,
}

impl PortBudget {
    /// Creates an explicit per-call budget.
    ///
    /// The executor is what supplies this in production. It is public so a port
    /// adapter living in another crate can build the budget its own tests need
    /// without a back door into the executor.
    #[must_use]
    pub const fn new(
        max_comparisons: usize,
        max_memory_bytes: usize,
        max_hydration_bytes: usize,
        remaining_elapsed_micros: u64,
    ) -> Self {
        Self {
            max_comparisons,
            max_memory_bytes,
            max_hydration_bytes,
            remaining_elapsed_micros,
        }
    }

    /// Returns the maximum comparisons this call may perform.
    #[must_use]
    pub const fn max_comparisons(self) -> usize {
        self.max_comparisons
    }

    /// Returns the maximum extension-owned response/transient bytes this call may allocate.
    ///
    /// PostgreSQL executor-internal sort and SPI memory is outside this value;
    /// database adapters bound its input cardinality and elapsed execution
    /// separately before materializing Rust-owned responses.
    #[must_use]
    pub const fn max_memory_bytes(self) -> usize {
        self.max_memory_bytes
    }

    /// Returns the maximum source-key bytes this call may hydrate.
    #[must_use]
    pub const fn max_hydration_bytes(self) -> usize {
        self.max_hydration_bytes
    }

    /// Returns the remaining wall-clock allowance for this call.
    #[must_use]
    pub const fn remaining_elapsed_micros(self) -> u64 {
        self.remaining_elapsed_micros
    }
}

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

/// Monotonic query clock used for deterministic elapsed-budget enforcement.
pub trait QueryClock {
    /// Returns monotonically non-decreasing microseconds for this execution.
    fn now_micros(&self) -> u64;
}

/// Candidate-generation source such as exact, HNSW, sparse, or mmap search.
pub trait CandidateSource {
    /// Reports source readiness without performing candidate work.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral port error.
    fn readiness(&mut self, query: &QueryIr, budget: PortBudget) -> Result<SourceReadiness>;

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
    fn candidate_limit(
        &mut self,
        _query: &QueryIr,
        remaining: usize,
        _budget: PortBudget,
    ) -> Result<usize> {
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
        budget: PortBudget,
    ) -> Result<CandidatePage>;
}

/// Statement-local, resumable candidate provider.
///
/// Implementations own their frontier and visited state. They must not encode
/// or persist that state, and every operation must honor the supplied
/// [`PortBudget`] before doing adapter work.
pub trait CandidateCursor {
    /// Returns the next already-admitted candidate without consuming it.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral cancellation, budget, or adapter failure.
    fn peek(&mut self, budget: PortBudget) -> Result<Option<Candidate>>;

    /// Consumes and returns the next already-admitted candidate.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral cancellation, budget, or adapter failure.
    fn pop(&mut self, budget: PortBudget) -> Result<Option<Candidate>>;

    /// Advances the provider by at most the requested number of expansions.
    ///
    /// The returned page contains only candidates made safe to expose by this
    /// advance. `CandidatePage::exhausted()` may be true only when
    /// [`Self::termination`] is [`LazyCursorTermination::Exhausted`].
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral cancellation, budget, or adapter failure.
    fn advance(&mut self, request: LazyCursorAdvance, budget: PortBudget)
    -> Result<LazyCursorPage>;

    /// Reports whether the provider proved that no additional candidate can
    /// be produced.
    fn exhausted(&self) -> bool;

    /// Returns cumulative content-free work for this statement-local cursor.
    fn work(&self) -> LazyCursorWork;

    /// Returns the terminal reason once the cursor can no longer advance.
    fn termination(&self) -> Option<LazyCursorTermination>;
}

/// Adapter that derives logical candidates from a public filter.
pub trait FilterCandidateSource {
    /// Returns the bounded filter-candidate request for one leaf.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral port error when the adapter cannot derive
    /// a valid bounded request for this filter and budget.
    fn candidate_limit(
        &mut self,
        _query: &QueryIr,
        remaining: usize,
        _budget: PortBudget,
    ) -> Result<usize> {
        Ok(remaining)
    }

    /// Returns an owned bounded logical-ID batch.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral port error.
    fn filter_candidates(
        &mut self,
        query: &QueryIr,
        limit: usize,
        budget: PortBudget,
    ) -> Result<FilterCandidateBatch>;
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
        budget: PortBudget,
    ) -> Result<RecheckPage>;
}

/// Adapter for model-backed reranking with no orchestration policy.
pub trait ExternalReranker {
    /// Reranks only the supplied visible rows under the requested hard limit.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral provider, validation, or budget failure.
    fn rerank(
        &mut self,
        query: &QueryIr,
        rows: &[HydratedCandidate],
        limit: usize,
        budget: PortBudget,
    ) -> Result<ExternalRerankPage>;
}

/// Adapter for bounded topology expansion from already-visible seed rows.
pub trait TopologyExpander {
    /// Returns candidates discovered from the supplied seeds.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral topology, validation, or budget failure.
    fn expand(
        &mut self,
        query: &QueryIr,
        seeds: &[HydratedCandidate],
        max_depth: usize,
        limit: usize,
        budget: PortBudget,
    ) -> Result<CandidatePage>;
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
