    use core::cell::{Cell, RefCell};
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use context_core::{OccurrenceId, PointId, SourceKey, SourceVersion};
    use context_query::{
        RERANK_CONTENT_DIGEST_BYTES, RerankContentDigest, RerankContribution, RerankRequest,
    };

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
            RerankContentDigest::new([1; RERANK_CONTENT_DIGEST_BYTES]),
            format!("row {}", row.point_id().get()),
            usize::try_from(row.point_id().get()).map_err(|_| QueryError::ArithmeticOverflow {
                operation: "rerank_fixture_rank",
            })?,
            row.score(),
            vec![RerankContribution::new(
                "fixture",
                1,
                row.score(),
                1.0,
                0.01,
            )?],
            Vec::new(),
        )
    }

    struct AuthorizeAll;

    impl AuthorizedRowSource for AuthorizeAll {
        fn authorize(
            &mut self,
            row: &HydratedCandidate,
            _max_candidate_bytes: usize,
        ) -> context_query::Result<Option<RerankCandidate>> {
            candidate_for(row).map(Some)
        }
    }

    #[derive(Clone, Copy)]
    enum FinalChange {
        SourceVersion,
        Text,
        Revoke,
    }

    struct ChangeAfterRelease {
        calls: BTreeMap<PointId, usize>,
        change: FinalChange,
    }

    impl ChangeAfterRelease {
        fn new(change: FinalChange) -> Self {
            Self {
                calls: BTreeMap::new(),
                change,
            }
        }
    }

    impl AuthorizedRowSource for ChangeAfterRelease {
        fn authorize(
            &mut self,
            row: &HydratedCandidate,
            _max_candidate_bytes: usize,
        ) -> context_query::Result<Option<RerankCandidate>> {
            let calls = self.calls.entry(row.point_id()).or_default();
            *calls = calls.saturating_add(1);
            if *calls == 1 {
                return candidate_for(row).map(Some);
            }
            if matches!(self.change, FinalChange::Revoke) {
                return Ok(None);
            }
            let occurrence =
                OccurrenceId::new(row.point_id().get()).ok_or(QueryError::InvalidInput {
                    field: "occurrence_id",
                    reason: "must be nonzero".to_owned(),
                })?;
            let version =
                SourceVersion::new(if matches!(self.change, FinalChange::SourceVersion) {
                    2
                } else {
                    1
                })
                .ok_or(QueryError::InvalidInput {
                    field: "source_version",
                    reason: "must be nonzero".to_owned(),
                })?;
            let text = if matches!(self.change, FinalChange::Text) {
                format!("edited row {}", row.point_id().get())
            } else {
                format!("row {}", row.point_id().get())
            };
            RerankCandidate::new(
                occurrence,
                row.point_id(),
                version,
                RerankContentDigest::new(if matches!(self.change, FinalChange::Text) {
                    [2; RERANK_CONTENT_DIGEST_BYTES]
                } else {
                    [1; RERANK_CONTENT_DIGEST_BYTES]
                }),
                text,
                usize::try_from(row.point_id().get()).map_err(|_| {
                    QueryError::ArithmeticOverflow {
                        operation: "rerank_fixture_rank",
                    }
                })?,
                row.score(),
                vec![RerankContribution::new(
                    "fixture",
                    1,
                    row.score(),
                    1.0,
                    0.01,
                )?],
                Vec::new(),
            )
            .map(Some)
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
            _max_candidate_bytes: usize,
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
                model: request.model().as_str().to_owned(),
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

    fn constrained_budget(
        max_comparisons: usize,
        max_memory_bytes: usize,
        max_hydration_bytes: usize,
    ) -> PortBudget {
        PortBudget::new(
            max_comparisons,
            max_memory_bytes,
            max_hydration_bytes,
            60_000_000,
        )
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
            BackendRerankerConfig::new(
                RerankModelName::new("fixture-v1").expect("model"),
                RerankQuery::new("postgres retrieval").expect("query"),
                policy,
                RerankRequestId::new(1).expect("request id"),
            ),
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

    #[test]
    fn a_source_version_change_after_provider_scoring_fails_the_required_rerank() {
        let backend = DeterministicRerankBackend::new(7);
        let mut rows = ChangeAfterRelease::new(FinalChange::SourceVersion);
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
            .expect_err("source version drift must fail closed");
        assert!(matches!(
            error,
            QueryError::PortFailure {
                stage: "external_rerank_source_recheck",
                ..
            }
        ));
    }

    #[test]
    fn an_unversioned_text_edit_after_provider_scoring_also_fails_closed() {
        let backend = DeterministicRerankBackend::new(7);
        let mut rows = ChangeAfterRelease::new(FinalChange::Text);
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
            .expect_err("authorized text drift must fail closed");
        assert!(matches!(
            error,
            QueryError::PortFailure {
                stage: "external_rerank_source_recheck",
                ..
            }
        ));
    }

    #[test]
    fn a_post_provider_permission_change_fails_closed_under_the_degrade_policy() {
        let backend = DeterministicRerankBackend::new(7);
        let mut rows = ChangeAfterRelease::new(FinalChange::Revoke);
        let (cancellation, clock) = (NeverCancelled, FixedClock);
        let mut adapter = reranker(
            &backend,
            &mut rows,
            &cancellation,
            &clock,
            RerankFallbackPolicy::DegradeWithoutRerank,
        );

        let error = adapter
            .rerank(&query(4), &fused_rows(4), 4, budget(1_000))
            .expect_err("permission drift must never use provider fallback");
        assert!(matches!(
            error,
            QueryError::PortFailure {
                stage: "external_rerank_source_recheck",
                ..
            }
        ));
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
                model: request.model().as_str().to_owned(),
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
    fn tied_scores_use_the_canonical_occurrence_membership_boundary() {
        struct ReverseOccurrences;

        impl AuthorizedRowSource for ReverseOccurrences {
            fn authorize(
                &mut self,
                row: &HydratedCandidate,
                _max_candidate_bytes: usize,
            ) -> context_query::Result<Option<RerankCandidate>> {
                let occurrence = OccurrenceId::new(10 - row.point_id().get()).expect("occurrence");
                let version = SourceVersion::new(1).expect("version");
                RerankCandidate::new(
                    occurrence,
                    row.point_id(),
                    version,
                    RerankContentDigest::new([1; RERANK_CONTENT_DIGEST_BYTES]),
                    format!("row {}", row.point_id().get()),
                    usize::try_from(row.point_id().get()).expect("rank"),
                    row.score(),
                    vec![RerankContribution::new(
                        "fixture",
                        1,
                        row.score(),
                        1.0,
                        0.01,
                    )?],
                    Vec::new(),
                )
                .map(Some)
            }
        }

        let backend = TiedBackend;
        let mut rows = ReverseOccurrences;
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
            vec![5, 4, 3],
            "top-k membership must use score then occurrence, as detached finalization does"
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
            RerankFallbackPolicy::DegradeWithoutRerank,
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
    fn a_withheld_row_fails_closed_before_provider_dispatch() {
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

        let error = adapter
            .rerank(&query(5), &fused_rows(5), 5, budget(1_000))
            .expect_err("authority withholding must fail closed");
        assert!(matches!(
            error,
            QueryError::PortFailure {
                stage: "external_rerank_source_authorization",
                ..
            }
        ));
    }

    #[test]
    fn a_withheld_row_fails_even_when_other_rows_could_fill_the_limit() {
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

        let error = adapter
            .rerank(&query(3), &fused_rows(5), 3, budget(1_000))
            .expect_err("authority withholding is not a partial-output policy");
        assert!(matches!(
            error,
            QueryError::PortFailure {
                stage: "external_rerank_source_authorization",
                ..
            }
        ));
        assert_eq!(rows.calls.get(), 2);
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
            RerankFallbackPolicy::DegradeWithoutRerank,
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
    fn an_insufficient_comparison_budget_releases_nothing() {
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
        assert!(!page.exhausted());
        assert_eq!(page.comparisons(), 0);
        assert_eq!(rows.calls.get(), 0, "no partial prefix may be authorized");
        assert_eq!(backend.released(), 0);
        assert!(page.rows().is_empty());
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
        assert_eq!(backend.released(), 0, "partial membership is never released");
    }

    #[test]
    fn insufficient_memory_and_hydration_fail_before_authorization() {
        for budget in [
            constrained_budget(10, 1, 1 << 20),
            constrained_budget(10, 1 << 20, 1),
        ] {
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
                .rerank(&query(2), &fused_rows(6), 2, budget)
                .expect("bounded refusal");
            assert!(!page.exhausted());
            assert_eq!(page.comparisons(), 0);
            assert_eq!(rows.calls.get(), 0);
            assert_eq!(backend.released(), 0);
        }
    }

    #[test]
    fn every_candidate_is_scored_before_a_low_fused_rank_row_can_be_promoted() {
        struct PromoteLastBackend;

        impl RerankBackend for PromoteLastBackend {
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
                    model: request.model().as_str().to_owned(),
                    model_revision: 7,
                    scores: request
                        .candidates()
                        .iter()
                        .map(|candidate| WireRerankScore {
                            occurrence_id: candidate.occurrence_id().get(),
                            score: if candidate.point_id().get() == 6 { 1.0 } else { 0.0 },
                        })
                        .collect(),
                })
            }
        }

        let backend = PromoteLastBackend;
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
            .rerank(&query(2), &fused_rows(6), 2, budget(6))
            .expect("complete rerank");
        assert!(page.exhausted());
        assert_eq!(page.comparisons(), 6);
        assert_eq!(page.rows()[0].point_id().get(), 6);
    }

    #[test]
    fn nothing_authorized_fails_closed_before_provider_dispatch() {
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

        let error = adapter
            .rerank(&query(4), &fused_rows(4), 4, budget(1_000))
            .expect_err("authority denial must fail closed");
        assert!(matches!(
            error,
            QueryError::PortFailure {
                stage: "external_rerank_source_authorization",
                ..
            }
        ));
        assert_eq!(rows.calls.get(), 1);
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

    include!("tests_provider_boundary.rs");
