//! Bridge from a [`RerankBackend`] to the executor's reranking port.
//!
//! The executor already owns a reranking port — [`ExternalReranker`] — and
//! there must be exactly one. This module adapts a transport-level backend to
//! it rather than introducing a parallel path: the backend knows how to talk to
//! a provider, and the adapter knows how the executor's budget, ordering, and
//! authority rules apply to what comes back.
//!
//! Authorization is the caller's job, not the adapter's. A hydrated row carries
//! no text and no version, so releasing anything to a provider requires an
//! [`AuthorizedRowSource`] that re-reads the authoritative row and decides what
//! may leave the database.
//!
//! # The score a reranked row carries
//!
//! The executor re-sorts a completed page by [`HydratedCandidate::score`] and
//! stages above this one — a score threshold, a weight, a formula — read the
//! same value. So the row carries the *provider's own score*, not its rank: a
//! rank ordinal would order correctly and then make every score-reading stage
//! above it meaningless. After an external rerank the score is the reranker's
//! relevance, which is what a threshold over a reranked query should filter on.
//!
//! # Reranking is all or nothing
//!
//! A row can miss the provider three ways: the source withheld it, the
//! comparison budget cut it, or the provider returned no score for it. The
//! executor requires a completed page to hold exactly the rows it asked for,
//! and no honest score exists for a row nobody ranked — so when fewer than
//! `limit` rows come back scored, the whole page is reported as
//! *non-authoritative* rather than padded with invented scores.
//!
//! A row that was never ranked therefore does not appear in a complete answer.
//! That is the point: it was excluded by the deployment's own authorization or
//! budget, not quietly reordered. A provider cannot exploit it either, since
//! omitting a score is worth no more to it than returning the lowest one — and
//! omitting enough scores degrades the query instead of trimming the answer.

use std::collections::BTreeMap;

use context_query::{
    Cancellation, ExternalRerankPage, ExternalReranker, HydratedCandidate, PortBudget, QueryClock,
    QueryError, QueryIr, QueryKind, RerankCandidate, RerankFallbackPolicy, RerankRequestId,
    ScoreOrder,
};

use crate::{
    MAX_RERANK_BATCH, RerankBackend, RerankBackendError, RerankBatches, RerankOutcome, score_all,
};

/// Supplies the authorized snapshot for hydrated rows.
///
/// The implementation is the component with database authority: it re-reads the
/// row, applies whatever the deployment considers releasable, and stamps the
/// occurrence identity and source version the response is validated against.
/// Returning `None` withholds the row from the provider. A withheld row cannot
/// be ranked, so it does not appear in a complete reranked answer; if that
/// leaves fewer than the requested number of rows, the query degrades rather
/// than returning a shorter one. Withhold a row only when it genuinely must not
/// be released.
pub trait AuthorizedRowSource {
    /// Returns what may be released for `row`, or `None` to withhold it.
    ///
    /// # Errors
    ///
    /// Returns a transport-neutral failure when the authoritative row cannot be
    /// read.
    fn authorize(
        &mut self,
        row: &HydratedCandidate,
    ) -> context_query::Result<Option<RerankCandidate>>;
}

/// Adapts a [`RerankBackend`] to the executor's [`ExternalReranker`] port.
pub struct BackendReranker<'a, B, S, C, K> {
    backend: &'a B,
    rows: &'a mut S,
    cancellation: &'a C,
    clock: &'a K,
    policy: RerankFallbackPolicy,
    batch_size: usize,
    next_request_id: u64,
}

impl<'a, B, S, C, K> BackendReranker<'a, B, S, C, K>
where
    B: RerankBackend,
    S: AuthorizedRowSource,
    C: Cancellation,
    K: QueryClock,
{
    /// Creates the adapter.
    ///
    /// `first_request_id` seeds the request identities this adapter derives; it
    /// is advanced past every batch it issues, so no two calls through the same
    /// adapter can be confused for one another.
    pub fn new(
        backend: &'a B,
        rows: &'a mut S,
        cancellation: &'a C,
        clock: &'a K,
        policy: RerankFallbackPolicy,
        first_request_id: RerankRequestId,
    ) -> Self {
        Self {
            backend,
            rows,
            cancellation,
            clock,
            policy,
            batch_size: MAX_RERANK_BATCH,
            next_request_id: first_request_id.get(),
        }
    }

    /// Sets the number of candidates sent to the backend per call.
    ///
    /// # Errors
    ///
    /// Returns [`RerankBackendError::InvalidPlan`] when `batch_size` is outside
    /// `1..=`[`MAX_RERANK_BATCH`]. It is rejected at configuration time so a
    /// misconfiguration cannot surface as a failure mid-query.
    pub fn with_batch_size(mut self, batch_size: usize) -> crate::BackendResult<Self> {
        if batch_size == 0 || batch_size > MAX_RERANK_BATCH {
            return Err(RerankBackendError::InvalidPlan {
                reason: format!("rerank batch size must be 1..={MAX_RERANK_BATCH}"),
            });
        }
        self.batch_size = batch_size;
        Ok(self)
    }
}

impl<B, S, C, K> ExternalReranker for BackendReranker<'_, B, S, C, K>
where
    B: RerankBackend,
    S: AuthorizedRowSource,
    C: Cancellation,
    K: QueryClock,
{
    fn rerank(
        &mut self,
        query: &QueryIr,
        rows: &[HydratedCandidate],
        limit: usize,
        budget: PortBudget,
    ) -> context_query::Result<ExternalRerankPage> {
        let model_revision = self.backend.model_revision();
        if rows.is_empty() || limit == 0 {
            return Ok(ExternalRerankPage::new(Vec::new(), 0, true, model_revision));
        }
        // The plan names the revision it requires. The executor checks this too,
        // but only after the port returns — by then the authorized text has
        // already been released, so a backend serving the wrong weights has to
        // be caught before anything leaves the database.
        if let QueryKind::ExternalRerank {
            model_revision: required,
            ..
        } = query.kind()
            && *required != model_revision
        {
            return Err(QueryError::PortFailure {
                stage: "external_rerank",
                message: "backend serves a different model revision than the plan requires"
                    .to_owned(),
            });
        }

        // A provider ranks by descending relevance, and the executor sorts the
        // returned page by the query's ordering. Those only agree under
        // `HigherIsBetter` — which is what the IR pins this query kind to — so
        // any other ordering is refused rather than silently inverted.
        if query.score_order() != ScoreOrder::HigherIsBetter {
            return Err(QueryError::PortFailure {
                stage: "external_rerank",
                message: "external reranking requires higher-is-better ordering".to_owned(),
            });
        }

        // A port may not exceed its budget, and the provider is what performs
        // the comparisons. Rows past the budget are never authorized and never
        // released, so a budget below the requested row count cannot produce an
        // authoritative rerank — it degrades the query below.
        let comparable = rows.len().min(budget.max_comparisons());
        let mut candidates = Vec::with_capacity(comparable);
        let mut by_occurrence = BTreeMap::new();
        for row in rows.iter().take(comparable) {
            // `authorize` is a database read per row; without this the phase
            // before the first provider call is uninterruptible.
            self.cancellation.check_interrupt()?;
            let Some(candidate) = self.rows.authorize(row)? else {
                continue;
            };
            if by_occurrence
                .insert(candidate.occurrence_id(), row.point_id())
                .is_some()
            {
                return Err(QueryError::PortFailure {
                    stage: "external_rerank",
                    message: "authorized rows repeated an occurrence".to_owned(),
                });
            }
            candidates.push(candidate);
        }
        if candidates.is_empty() {
            return Ok(unranked(model_revision, 0));
        }

        // The provider compares everything released to it, whether or not it
        // answers for every row, so this is the work the query performed.
        let comparisons = candidates.len();
        let expires_at_micros = self
            .clock
            .now_micros()
            .checked_add(budget.remaining_elapsed_micros())
            .ok_or(QueryError::ArithmeticOverflow {
                operation: "rerank_deadline_projection",
            })?;
        let batches = RerankBatches::plan(
            RerankRequestId::new(self.next_request_id)?,
            model_revision,
            expires_at_micros,
            candidates,
            self.batch_size,
        )
        .map_err(port_failure)?;
        let issued = u64::try_from(batches.requests().len()).map_err(|_| {
            QueryError::ArithmeticOverflow {
                operation: "rerank_request_identity",
            }
        })?;
        self.next_request_id =
            self.next_request_id
                .checked_add(issued)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "rerank_request_identity",
                })?;

        let outcome = score_all(
            self.backend,
            &batches,
            self.cancellation,
            self.clock,
            self.policy,
        )
        .map_err(port_failure)?;
        let scores = match outcome {
            RerankOutcome::Reranked(scores) => scores,
            RerankOutcome::Degraded { .. } => {
                return Ok(unranked(model_revision, comparisons));
            }
        };

        // Only rows the provider actually judged can carry a reranked score.
        // A page short of `limit` cannot be completed without inventing a score
        // for a row nobody ranked, so it is reported as non-authoritative
        // instead. That is also what stops a provider from editing the answer:
        // omitting a score degrades the query, it never removes a row.
        if scores.len() < limit {
            return Ok(unranked(model_revision, comparisons));
        }

        let by_point = rows
            .iter()
            .map(|row| (row.point_id(), row))
            .collect::<BTreeMap<_, _>>();
        let mut ranked = Vec::with_capacity(scores.len());
        for score in &scores {
            let Some(point_id) = by_occurrence.get(&score.occurrence_id()) else {
                // `score_all` already refuses an occurrence the request never
                // released, so this is unreachable through the validated path.
                // It is refused rather than skipped so no future path can
                // introduce a row the caller never hydrated.
                return Err(QueryError::PortFailure {
                    stage: "external_rerank",
                    message: "provider scored an unreleased occurrence".to_owned(),
                });
            };
            ranked.push((*point_id, score.score()));
        }
        // Same comparison the executor applies to the page it gets back, so the
        // rows this call keeps at the limit boundary are the rows that survive.
        ranked.sort_by(|left, right| {
            right
                .1
                .total_cmp(&left.1)
                .then_with(|| left.0.cmp(&right.0))
        });

        let mut page = Vec::with_capacity(limit);
        for (point_id, score) in ranked.into_iter().take(limit) {
            let Some(row) = by_point.get(&point_id) else {
                return Err(QueryError::UnexpectedPointId {
                    stage: "external_rerank",
                    point_id,
                });
            };
            page.push(HydratedCandidate::new(
                point_id,
                row.source_key().clone(),
                score,
            )?);
        }
        Ok(ExternalRerankPage::new(
            page,
            comparisons,
            true,
            model_revision,
        ))
    }
}

/// Reports that no authoritative reranking happened.
///
/// The port has no channel for "usable but not reranked", so an un-exhausted
/// page is the honest answer: the executor surfaces it as a budget-exhausted
/// completion rather than presenting an un-reranked ordering as a reranked one.
/// Under [`RerankFallbackPolicy::DegradeWithoutRerank`] that is the difference
/// between a visibly incomplete answer and a hard error, not between two
/// orderings.
///
/// `comparisons` still reports the work released to the provider. The attempt
/// cost the same whether or not its answer was usable, and a degraded stage
/// that reported zero would understate what the query actually did.
fn unranked(model_revision: u64, comparisons: usize) -> ExternalRerankPage {
    ExternalRerankPage::new(Vec::new(), comparisons, false, model_revision)
}

fn port_failure(error: RerankBackendError) -> QueryError {
    QueryError::PortFailure {
        stage: "external_rerank",
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use core::cell::{Cell, RefCell};
    use std::collections::BTreeSet;

    use super::*;
    use context_core::{OccurrenceId, PointId, SourceKey, SourceVersion};
    use context_query::RerankRequest;

    use crate::{BackendResult, DeterministicRerankBackend, WireRerankResponse, WireRerankScore};

    struct FixedClock;

    impl QueryClock for FixedClock {
        fn now_micros(&self) -> u64 {
            1_000
        }
    }

    struct NeverCancelled;

    impl Cancellation for NeverCancelled {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    fn candidate_for(row: &HydratedCandidate) -> context_query::Result<RerankCandidate> {
        let occurrence =
            OccurrenceId::new(row.point_id().get()).ok_or(QueryError::InvalidInput {
                field: "occurrence_id",
                reason: "must be nonzero".to_owned(),
            })?;
        let version = SourceVersion::new(1).ok_or(QueryError::InvalidInput {
            field: "source_version",
            reason: "must be nonzero".to_owned(),
        })?;
        RerankCandidate::new(
            occurrence,
            row.point_id(),
            version,
            format!("row {}", row.point_id().get()),
            Vec::new(),
        )
    }

    struct AuthorizeAll;

    impl AuthorizedRowSource for AuthorizeAll {
        fn authorize(
            &mut self,
            row: &HydratedCandidate,
        ) -> context_query::Result<Option<RerankCandidate>> {
            candidate_for(row).map(Some)
        }
    }

    /// Withholds the rows whose point id is in `withheld`, and counts calls so
    /// a test can assert a row was never even considered for release.
    struct AuthorizeSome {
        withheld: BTreeSet<u64>,
        calls: Cell<usize>,
    }

    impl AuthorizeSome {
        fn new(withheld: impl IntoIterator<Item = u64>) -> Self {
            Self {
                withheld: withheld.into_iter().collect(),
                calls: Cell::new(0),
            }
        }
    }

    impl AuthorizedRowSource for AuthorizeSome {
        fn authorize(
            &mut self,
            row: &HydratedCandidate,
        ) -> context_query::Result<Option<RerankCandidate>> {
            self.calls.set(self.calls.get() + 1);
            if self.withheld.contains(&row.point_id().get()) {
                return Ok(None);
            }
            candidate_for(row).map(Some)
        }
    }

    struct UnavailableBackend;

    impl RerankBackend for UnavailableBackend {
        fn model_revision(&self) -> u64 {
            7
        }

        fn score(
            &self,
            _request: &RerankRequest,
            _deadline_micros: u64,
        ) -> BackendResult<WireRerankResponse> {
            Err(RerankBackendError::Unavailable)
        }
    }

    /// Scores only the first candidate of each batch, which the envelope allows.
    struct PartialBackend;

    impl RerankBackend for PartialBackend {
        fn model_revision(&self) -> u64 {
            7
        }

        fn score(
            &self,
            request: &RerankRequest,
            _deadline_micros: u64,
        ) -> BackendResult<WireRerankResponse> {
            Ok(WireRerankResponse {
                version: request.version(),
                request_id: request.request_id().get(),
                model_revision: 7,
                scores: request
                    .candidates()
                    .iter()
                    .take(1)
                    .map(|candidate| WireRerankScore {
                        occurrence_id: candidate.occurrence_id().get(),
                        score: 1.0,
                    })
                    .collect(),
            })
        }
    }

    /// Records the identities and candidate counts it was asked to score.
    struct RecordingBackend {
        seen: RefCell<Vec<(u64, usize)>>,
    }

    impl RecordingBackend {
        fn new() -> Self {
            Self {
                seen: RefCell::new(Vec::new()),
            }
        }

        fn identities(&self) -> Vec<u64> {
            self.seen.borrow().iter().map(|(id, _)| *id).collect()
        }

        fn released(&self) -> usize {
            self.seen.borrow().iter().map(|(_, count)| *count).sum()
        }
    }

    impl RerankBackend for RecordingBackend {
        fn model_revision(&self) -> u64 {
            7
        }

        fn score(
            &self,
            request: &RerankRequest,
            deadline_micros: u64,
        ) -> BackendResult<WireRerankResponse> {
            self.seen
                .borrow_mut()
                .push((request.request_id().get(), request.candidates().len()));
            DeterministicRerankBackend::new(7).score(request, deadline_micros)
        }
    }

    /// Rows already in fused order: point 1 best, point n worst.
    fn fused_rows(count: u64) -> Vec<HydratedCandidate> {
        (1..=count)
            .map(|point| {
                HydratedCandidate::new(
                    PointId::new(point),
                    SourceKey::new(format!("row-{point}")).expect("source key"),
                    1.0 - f64::from(u32::try_from(point).expect("small")) / 100.0,
                )
                .expect("hydrated")
            })
            .collect()
    }

    fn budget(max_comparisons: usize) -> PortBudget {
        PortBudget::new(max_comparisons, 1 << 20, 1 << 20, 60_000_000)
    }

    /// An `ExternalRerank` node. The IR pins this kind to `HigherIsBetter`, so
    /// that is the ordering the executor ever hands this port in practice.
    fn plan_for(limit: usize, model_revision: u64) -> QueryIr {
        let inner = QueryIr::nearest(
            None,
            vec![0.0, 1.0],
            ScoreOrder::HigherIsBetter,
            None,
            limit,
        )
        .expect("query");
        QueryIr::new(
            QueryKind::ExternalRerank {
                query: Box::new(inner),
                model_revision,
            },
            ScoreOrder::HigherIsBetter,
            None,
            limit,
        )
        .expect("external rerank plan")
    }

    fn query(limit: usize) -> QueryIr {
        plan_for(limit, 7)
    }

    fn reranker<'a, B: RerankBackend, S: AuthorizedRowSource>(
        backend: &'a B,
        rows: &'a mut S,
        cancellation: &'a NeverCancelled,
        clock: &'a FixedClock,
        policy: RerankFallbackPolicy,
    ) -> BackendReranker<'a, B, S, NeverCancelled, FixedClock> {
        BackendReranker::new(
            backend,
            rows,
            cancellation,
            clock,
            policy,
            RerankRequestId::new(1).expect("request id"),
        )
    }

    /// The order the deterministic backend puts these points in.
    fn expected_order(points: &[u64]) -> Vec<u64> {
        let mut scored = points
            .iter()
            .map(|point| {
                (
                    *point,
                    DeterministicRerankBackend::score_of(&format!("row {point}")),
                )
            })
            .collect::<Vec<_>>();
        scored.sort_by(|left, right| right.1.total_cmp(&left.1).then(left.0.cmp(&right.0)));
        scored.into_iter().map(|(point, _)| point).collect()
    }

    fn point_ids(page: &ExternalRerankPage) -> Vec<u64> {
        page.rows().iter().map(|row| row.point_id().get()).collect()
    }

    #[test]
    fn the_page_carries_the_providers_own_scores_in_the_providers_order() {
        // The executor re-sorts by score, and stages above this one read the
        // same value, so the score must be the reranker's relevance rather than
        // a rank ordinal that would make a threshold or formula meaningless.
        let backend = DeterministicRerankBackend::new(7);
        let mut rows = AuthorizeAll;
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::Require,
        )
        .with_batch_size(2)
        .expect("batch size");

        let page = adapter
            .rerank(&query(5), &fused_rows(5), 5, budget(1_000))
            .expect("reranked");

        assert!(page.exhausted());
        assert_eq!(page.comparisons(), 5);
        assert_eq!(point_ids(&page), expected_order(&[1, 2, 3, 4, 5]));

        let scores = page
            .rows()
            .iter()
            .map(HydratedCandidate::score)
            .collect::<Vec<_>>();
        let expected = point_ids(&page)
            .into_iter()
            .map(|point| DeterministicRerankBackend::score_of(&format!("row {point}")))
            .collect::<Vec<_>>();
        assert_eq!(scores, expected, "the score must be the provider's own");
        assert!(
            scores.windows(2).all(|pair| pair[0] > pair[1]),
            "a re-sort under HigherIsBetter must reproduce the provider order"
        );
    }

    /// Scores every candidate identically, so only the tie-break decides which
    /// rows survive the limit.
    struct TiedBackend;

    impl RerankBackend for TiedBackend {
        fn model_revision(&self) -> u64 {
            7
        }

        fn score(
            &self,
            request: &RerankRequest,
            _deadline_micros: u64,
        ) -> BackendResult<WireRerankResponse> {
            Ok(WireRerankResponse {
                version: request.version(),
                request_id: request.request_id().get(),
                model_revision: 7,
                scores: request
                    .candidates()
                    .iter()
                    .map(|candidate| WireRerankScore {
                        occurrence_id: candidate.occurrence_id().get(),
                        score: 0.5,
                    })
                    .collect(),
            })
        }
    }

    #[test]
    fn tied_scores_break_the_way_the_executor_would_break_them() {
        // The adapter truncates to `limit` and the executor then re-sorts what
        // it kept. If the two tie-breaks disagreed, the adapter would discard
        // rows the executor would have ranked first.
        let backend = TiedBackend;
        let mut rows = AuthorizeAll;
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::Require,
        );

        let page = adapter
            .rerank(&query(3), &fused_rows(5), 3, budget(1_000))
            .expect("reranked");
        assert_eq!(
            point_ids(&page),
            vec![1, 2, 3],
            "ties must resolve on ascending point id, as deterministic_points does"
        );
    }

    #[test]
    fn a_degraded_attempt_still_charges_the_work_it_released() {
        // The provider was called; reporting zero comparisons would understate
        // what the query actually did.
        let backend = PartialBackend;
        let mut rows = AuthorizeAll;
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::Require,
        )
        .with_batch_size(2)
        .expect("batch size");

        let page = adapter
            .rerank(&query(5), &fused_rows(5), 5, budget(1_000))
            .expect("degraded");
        assert!(!page.exhausted());
        assert_eq!(page.comparisons(), 5, "every released row was compared");
    }

    #[test]
    fn an_ordering_the_provider_cannot_satisfy_is_refused() {
        // A provider ranks by descending relevance, and the IR pins this query
        // kind to HigherIsBetter. Any other ordering would be sorted back to
        // front, so it is refused before a row is even authorized.
        let backend = DeterministicRerankBackend::new(7);
        let mut rows = AuthorizeSome::new([]);
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::Require,
        );

        let plan = QueryIr::nearest(None, vec![0.0, 1.0], ScoreOrder::LowerIsBetter, None, 3)
            .expect("query");
        let error = adapter
            .rerank(&plan, &fused_rows(3), 3, budget(1_000))
            .expect_err("refuse");
        assert!(matches!(
            error,
            QueryError::PortFailure {
                stage: "external_rerank",
                ..
            }
        ));
        assert_eq!(rows.calls.get(), 0, "no row may be authorized");
    }

    #[test]
    fn a_withheld_row_degrades_the_page_rather_than_shortening_it() {
        // The executor rejects a completed page short of `limit`, and there is
        // no honest score for a row the provider never judged.
        let backend = DeterministicRerankBackend::new(7);
        let mut rows = AuthorizeSome::new([2, 4]);
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::Require,
        );

        let page = adapter
            .rerank(&query(5), &fused_rows(5), 5, budget(1_000))
            .expect("degraded");
        assert!(!page.exhausted());
        assert!(page.rows().is_empty());
    }

    #[test]
    fn withheld_rows_still_allow_a_complete_page_under_a_smaller_limit() {
        let backend = DeterministicRerankBackend::new(7);
        let mut rows = AuthorizeSome::new([2, 4]);
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::Require,
        );

        let page = adapter
            .rerank(&query(3), &fused_rows(5), 3, budget(1_000))
            .expect("reranked");
        assert!(page.exhausted());
        assert_eq!(point_ids(&page), expected_order(&[1, 3, 5]));
        assert_eq!(page.comparisons(), 3);
    }

    #[test]
    fn a_provider_omitting_a_score_degrades_the_query_instead_of_dropping_a_row() {
        let backend = PartialBackend;
        let mut rows = AuthorizeAll;
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::Require,
        )
        .with_batch_size(2)
        .expect("batch size");

        // 5 candidates across 3 batches, one score each: too few to complete a
        // 5-row page, so the provider degrades the query rather than editing it.
        let page = adapter
            .rerank(&query(5), &fused_rows(5), 5, budget(1_000))
            .expect("degraded");
        assert!(!page.exhausted());
        assert!(page.rows().is_empty(), "a provider must not remove rows");
    }

    #[test]
    fn the_comparison_budget_bounds_what_is_released_to_the_provider() {
        let backend = RecordingBackend::new();
        let mut rows = AuthorizeSome::new([]);
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::Require,
        );

        let page = adapter
            .rerank(&query(2), &fused_rows(6), 2, budget(2))
            .expect("reranked");
        assert_eq!(page.comparisons(), 2, "the port must respect its budget");
        assert_eq!(
            rows.calls.get(),
            2,
            "rows past the budget are never authorized, let alone released"
        );
        assert_eq!(backend.released(), 2);
        assert_eq!(page.rows().len(), 2);
    }

    #[test]
    fn a_budget_too_small_to_rerank_the_page_degrades_rather_than_truncating() {
        let backend = RecordingBackend::new();
        let mut rows = AuthorizeSome::new([]);
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::Require,
        );

        let page = adapter
            .rerank(&query(6), &fused_rows(6), 6, budget(2))
            .expect("degraded");
        assert!(!page.exhausted());
        assert_eq!(backend.released(), 2, "the budget still bounds the release");
    }

    #[test]
    fn nothing_releasable_reports_an_unranked_page() {
        let backend = DeterministicRerankBackend::new(7);
        let mut rows = AuthorizeSome::new([1, 2, 3, 4]);
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::Require,
        );

        let page = adapter
            .rerank(&query(4), &fused_rows(4), 4, budget(1_000))
            .expect("unranked");
        assert!(!page.exhausted(), "an un-reranked page must say so");
        assert_eq!(rows.calls.get(), 4);
    }

    #[test]
    fn a_backend_failure_under_the_require_policy_fails_the_port() {
        let backend = UnavailableBackend;
        let mut rows = AuthorizeAll;
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::Require,
        );

        let error = adapter
            .rerank(&query(4), &fused_rows(4), 4, budget(1_000))
            .expect_err("fail closed");
        assert!(matches!(
            error,
            QueryError::PortFailure {
                stage: "external_rerank",
                ..
            }
        ));
    }

    #[test]
    fn a_backend_failure_under_the_degrade_policy_reports_an_unranked_page() {
        let backend = UnavailableBackend;
        let mut rows = AuthorizeAll;
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::DegradeWithoutRerank,
        );

        let page = adapter
            .rerank(&query(4), &fused_rows(4), 4, budget(1_000))
            .expect("degrade");
        assert!(!page.exhausted());
        assert!(page.rows().is_empty());
    }

    #[test]
    fn a_backend_serving_the_wrong_revision_is_refused_before_any_text_is_released() {
        let backend = DeterministicRerankBackend::new(9);
        let mut rows = AuthorizeSome::new([]);
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::Require,
        );

        // The plan requires revision 7; the backend serves 9.
        let error = adapter
            .rerank(&query(4), &fused_rows(4), 4, budget(1_000))
            .expect_err("refuse");
        assert!(matches!(
            error,
            QueryError::PortFailure {
                stage: "external_rerank",
                ..
            }
        ));
        assert_eq!(rows.calls.get(), 0, "no row may even be authorized");
    }

    #[test]
    fn an_unusable_batch_size_is_refused_at_configuration_time() {
        let backend = DeterministicRerankBackend::new(7);
        let mut rows = AuthorizeAll;
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        for size in [0, MAX_RERANK_BATCH + 1] {
            let configured = reranker(
                &backend,
                &mut rows,
                &cancellation,
                &clock,
                RerankFallbackPolicy::Require,
            )
            .with_batch_size(size);
            assert!(matches!(
                configured,
                Err(RerankBackendError::InvalidPlan { .. })
            ));
        }
    }

    #[test]
    fn request_identities_never_repeat_across_calls() {
        let backend = RecordingBackend::new();
        let mut rows = AuthorizeAll;
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::Require,
        )
        .with_batch_size(2)
        .expect("batch size");

        let input = fused_rows(5);
        adapter
            .rerank(&query(5), &input, 5, budget(1_000))
            .expect("first call");
        adapter
            .rerank(&query(5), &input, 5, budget(1_000))
            .expect("second call");

        let issued = backend.identities();
        assert_eq!(issued, vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(
            issued.iter().collect::<BTreeSet<_>>().len(),
            issued.len(),
            "an identity must never be reused across calls"
        );
    }

    #[test]
    fn an_empty_page_is_not_sent_to_a_provider() {
        let backend = UnavailableBackend;
        let mut rows = AuthorizeAll;
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::Require,
        );
        let page = adapter
            .rerank(&query(4), &[], 4, budget(1_000))
            .expect("empty");
        assert!(page.rows().is_empty());
        assert!(page.exhausted());
    }

    #[test]
    fn a_provider_scoring_an_unreleased_occurrence_is_refused_not_skipped() {
        struct ImpostorBackend;

        impl RerankBackend for ImpostorBackend {
            fn model_revision(&self) -> u64 {
                7
            }

            fn score(
                &self,
                request: &RerankRequest,
                _deadline_micros: u64,
            ) -> BackendResult<WireRerankResponse> {
                Ok(WireRerankResponse {
                    version: request.version(),
                    request_id: request.request_id().get(),
                    model_revision: 7,
                    scores: vec![WireRerankScore {
                        occurrence_id: 9_999,
                        score: 1.0,
                    }],
                })
            }
        }

        let backend = ImpostorBackend;
        let mut rows = AuthorizeAll;
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::Require,
        );
        assert!(
            adapter
                .rerank(&query(4), &fused_rows(4), 4, budget(1_000))
                .is_err()
        );
    }
}
