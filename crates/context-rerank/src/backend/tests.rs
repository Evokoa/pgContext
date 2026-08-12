    use core::cell::Cell;

    use super::*;
    use context_core::{OccurrenceId, PointId, SourceVersion};
    use context_query::{
        QueryError, RERANK_CONTENT_DIGEST_BYTES, RerankContentDigest, RerankContribution,
    };

    fn model() -> RerankModelName {
        RerankModelName::new("fixture-v1").expect("model")
    }

    fn query_text() -> RerankQuery {
        RerankQuery::new("postgres retrieval").expect("query")
    }

    /// A clock the test advances by hand, so elapsed time is not wall-clock.
    struct TestClock {
        now: Cell<u64>,
    }

    impl TestClock {
        const fn at(now: u64) -> Self {
            Self {
                now: Cell::new(now),
            }
        }

        fn advance(&self, micros: u64) {
            self.now.set(self.now.get() + micros);
        }
    }

    impl QueryClock for TestClock {
        fn now_micros(&self) -> u64 {
            self.now.get()
        }
    }

    struct NeverCancelled;

    impl Cancellation for NeverCancelled {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    /// Reports cancellation the way PostgreSQL does: through `check_interrupt`,
    /// with `is_cancelled` never becoming true.
    struct InterruptedLikePostgres;

    impl Cancellation for InterruptedLikePostgres {
        fn check_interrupt(&self) -> context_query::Result<()> {
            Err(QueryError::PortFailure {
                stage: "test_interrupt",
                message: "cancelled".to_owned(),
            })
        }

        fn is_cancelled(&self) -> bool {
            false
        }
    }

    struct UnavailableBackend {
        model_revision: u64,
    }

    impl RerankBackend for UnavailableBackend {
        fn model_revision(&self) -> u64 {
            self.model_revision
        }

        fn score(
            &self,
            _request: &RerankRequest,
            _deadline_micros: u64,
        ) -> BackendResult<WireRerankResponse> {
            Err(RerankBackendError::Unavailable)
        }
    }

    /// Counts calls so a test can assert a call was never issued.
    struct CountingBackend {
        calls: Cell<usize>,
    }

    impl CountingBackend {
        const fn new() -> Self {
            Self {
                calls: Cell::new(0),
            }
        }
    }

    impl RerankBackend for CountingBackend {
        fn model_revision(&self) -> u64 {
            7
        }

        fn score(
            &self,
            request: &RerankRequest,
            deadline_micros: u64,
        ) -> BackendResult<WireRerankResponse> {
            self.calls.set(self.calls.get() + 1);
            DeterministicRerankBackend::new(7).score(request, deadline_micros)
        }
    }

    /// Answers the request it was given with someone else's occurrence — the
    /// shape a confused or hostile backend actually produces.
    struct ImpostorBackend {
        model_revision: u64,
    }

    struct PartialBackend;

    impl RerankBackend for PartialBackend {
        fn model_revision(&self) -> u64 {
            7
        }

        fn score(
            &self,
            request: &RerankRequest,
            deadline_micros: u64,
        ) -> BackendResult<WireRerankResponse> {
            let mut response =
                DeterministicRerankBackend::new(7).score(request, deadline_micros)?;
            response.scores.truncate(1);
            Ok(response)
        }
    }

    impl RerankBackend for ImpostorBackend {
        fn model_revision(&self) -> u64 {
            self.model_revision
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
                model_revision: self.model_revision,
                scores: vec![crate::WireRerankScore {
                    occurrence_id: 9_999,
                    score: 1.0,
                }],
            })
        }
    }

    /// Stalls past the envelope's expiry before answering.
    struct SlowBackend<'clock> {
        clock: &'clock TestClock,
        stall_micros: u64,
    }

    impl RerankBackend for SlowBackend<'_> {
        fn model_revision(&self) -> u64 {
            7
        }

        fn score(
            &self,
            request: &RerankRequest,
            deadline_micros: u64,
        ) -> BackendResult<WireRerankResponse> {
            self.clock.advance(self.stall_micros);
            DeterministicRerankBackend::new(7).score(request, deadline_micros)
        }
    }

    fn candidate_with_ids(occurrence: u64, point: u64) -> RerankCandidate {
        RerankCandidate::new(
            OccurrenceId::new(occurrence).expect("occurrence"),
            PointId::new(point),
            SourceVersion::new(1).expect("version"),
            RerankContentDigest::new([1; RERANK_CONTENT_DIGEST_BYTES]),
            format!("candidate text {occurrence}"),
            usize::try_from(occurrence).expect("rank"),
            0.1,
            vec![
                RerankContribution::new(
                    "fixture",
                    usize::try_from(occurrence).expect("rank"),
                    0.1,
                    1.0,
                    0.01,
                )
                .expect("contribution"),
            ],
            Vec::new(),
        )
        .expect("candidate")
    }

    fn candidate(index: u64) -> RerankCandidate {
        candidate_with_ids(index, index)
    }

    fn candidates(count: u64) -> Vec<RerankCandidate> {
        (1..=count).map(candidate).collect()
    }

    fn plan(candidates: Vec<RerankCandidate>, batch_size: usize) -> BackendResult<RerankBatches> {
        RerankBatches::plan(
            RerankRequestId::new(100).expect("request id"),
            &model(),
            7,
            10_000,
            &query_text(),
            candidates,
            batch_size,
        )
    }

    fn batches(count: u64, batch_size: usize) -> RerankBatches {
        plan(candidates(count), batch_size).expect("plan")
    }

    #[test]
    fn batching_covers_every_candidate_exactly_once_with_distinct_identities() {
        let planned = batches(7, 3);
        assert_eq!(planned.requests().len(), 3);

        let identities = planned
            .requests()
            .iter()
            .map(|request| request.request_id().get())
            .collect::<BTreeSet<_>>();
        assert_eq!(identities, BTreeSet::from([100, 101, 102]));

        let covered = planned
            .requests()
            .iter()
            .flat_map(RerankRequest::candidates)
            .map(RerankCandidate::occurrence_id)
            .collect::<Vec<_>>();
        assert_eq!(covered.len(), 7, "no candidate may be dropped or repeated");
        assert_eq!(covered.iter().copied().collect::<BTreeSet<_>>().len(), 7);
    }

    #[test]
    fn an_occurrence_repeated_across_batches_is_refused() {
        // Each batch is individually valid, so only a plan-wide check catches
        // this. Releasing the row twice would multiply the authorized byte
        // budget and let the provider pick which of its two scores survives.
        let repeated = vec![candidate(1), candidate(2), candidate(1)];
        assert!(matches!(
            plan(repeated, 2),
            Err(RerankBackendError::InvalidPlan { .. })
        ));
    }

    #[test]
    fn a_point_repeated_across_batches_is_refused() {
        let repeated = vec![candidate(1), candidate(2), candidate_with_ids(3, 1)];
        assert!(matches!(
            plan(repeated, 2),
            Err(RerankBackendError::InvalidPlan { .. })
        ));
    }

    #[test]
    fn an_empty_candidate_set_is_refused_rather_than_silently_succeeding() {
        // `chunks` yields nothing for an empty slice, so without this check the
        // plan would be empty and the whole rerank would report success with no
        // ordering at all.
        assert!(matches!(
            plan(Vec::new(), 4),
            Err(RerankBackendError::InvalidPlan { .. })
        ));
    }

    #[test]
    fn a_batch_is_split_on_bytes_as_well_as_on_count() {
        // 200 candidates of 25 KiB fit the 512-candidate ceiling and blow the
        // 4 MiB request ceiling, so a count-only split would build a plan the
        // envelope refuses.
        let text = "x".repeat(25 * 1024);
        let candidates = (1..=200_u64)
            .map(|index| {
                RerankCandidate::new(
                    OccurrenceId::new(index).expect("occurrence"),
                    PointId::new(index),
                    SourceVersion::new(1).expect("version"),
                    RerankContentDigest::new([1; RERANK_CONTENT_DIGEST_BYTES]),
                    text.clone(),
                    usize::try_from(index).expect("rank"),
                    0.1,
                    vec![
                        RerankContribution::new(
                            "fixture",
                            usize::try_from(index).expect("rank"),
                            0.1,
                            1.0,
                            0.01,
                        )
                        .expect("contribution"),
                    ],
                    Vec::new(),
                )
                .expect("candidate")
            })
            .collect::<Vec<_>>();

        let planned = plan(candidates, MAX_RERANK_BATCH).expect("plan");
        assert!(
            planned.requests().len() > 1,
            "the byte ceiling must force a split"
        );
        for request in planned.requests() {
            assert!(request.projected_bytes() <= MAX_RERANK_REQUEST_BYTES);
        }
        let covered = planned
            .requests()
            .iter()
            .flat_map(RerankRequest::candidates)
            .count();
        assert_eq!(covered, 200, "no candidate may be dropped by the split");
    }

    #[test]
    fn an_unusable_batch_size_is_refused_rather_than_clamped() {
        for size in [0, MAX_RERANK_BATCH + 1] {
            assert!(matches!(
                plan(candidates(2), size),
                Err(RerankBackendError::InvalidPlan { .. })
            ));
        }
    }

    #[test]
    fn a_derived_identity_that_would_overflow_fails_instead_of_wrapping() {
        let planned = RerankBatches::plan(
            RerankRequestId::new(u64::MAX).expect("request id"),
            &model(),
            7,
            10_000,
            &query_text(),
            candidates(4),
            2,
        );
        assert!(matches!(
            planned,
            Err(RerankBackendError::InvalidPlan { .. })
        ));
    }

    #[test]
    fn the_deterministic_backend_scores_every_batch() {
        let backend = DeterministicRerankBackend::new(7);
        let outcome = score_all(
            &backend,
            &batches(7, 3),
            &NeverCancelled,
            &TestClock::at(0),
            RerankFallbackPolicy::Require,
        )
        .expect("scored");
        let RerankOutcome::Reranked(scores) = outcome else {
            unreachable!("a healthy backend must not degrade")
        };
        assert_eq!(scores.len(), 7);
        assert!(scores.iter().all(|score| score.score().is_finite()));
    }

    #[test]
    fn the_deterministic_backend_matches_its_pinned_values() {
        // Pinned so that changing the offset basis, the prime, or the shift is
        // a test failure rather than a silent change to a "never changes" score.
        assert_eq!(
            DeterministicRerankBackend::score_of("candidate text 1"),
            0.039_453_922_731_895_41
        );
        assert_eq!(
            DeterministicRerankBackend::score_of("candidate text 2"),
            0.456_253_198_859_871_03
        );
        assert_eq!(
            DeterministicRerankBackend::score_of(""),
            0.957_673_425_242_056_1
        );
    }

    #[test]
    fn texts_differing_in_one_character_are_not_almost_tied() {
        // Without the finalizer these differ by ~1e-7, which makes the fixture
        // unable to demonstrate that an ordering was carried through.
        let first = DeterministicRerankBackend::score_of("candidate text 1");
        let second = DeterministicRerankBackend::score_of("candidate text 2");
        assert!(
            (first - second).abs() > 0.01,
            "{first} and {second} are too close to order meaningfully"
        );
    }

    #[test]
    fn the_deterministic_backend_stays_inside_the_unit_interval() {
        for index in 0..2_000_u64 {
            let score = DeterministicRerankBackend::score_of(&format!("row {index}"));
            assert!((0.0..1.0).contains(&score), "{score} escaped [0, 1)");
        }
    }

    #[test]
    fn a_model_revision_drift_is_refused_before_the_call_is_made() {
        let backend = CountingBackend::new();
        let planned = RerankBatches::plan(
            RerankRequestId::new(1).expect("request id"),
            &model(),
            8,
            10_000,
            &query_text(),
            candidates(2),
            2,
        )
        .expect("plan");
        let error =
            score_batch(&backend, &planned.requests()[0], &TestClock::at(0)).expect_err("refuse");
        assert_eq!(
            error,
            RerankBackendError::Rejected(RerankRejection::ModelRevisionMismatch)
        );
        assert_eq!(backend.calls.get(), 0, "the provider must not be called");
    }

    #[test]
    fn a_provider_scoring_an_unreleased_row_is_refused() {
        let backend = ImpostorBackend { model_revision: 7 };
        let planned = batches(2, 2);
        let error =
            score_batch(&backend, &planned.requests()[0], &TestClock::at(0)).expect_err("refuse");
        assert_eq!(
            error,
            RerankBackendError::Rejected(RerankRejection::UnknownOccurrence)
        );
    }

    #[test]
    fn provider_omission_is_never_authoritative_even_when_one_score_would_fill_a_limit() {
        let planned = batches(4, 4);
        let error = score_batch(&PartialBackend, &planned.requests()[0], &TestClock::at(0))
            .expect_err("partial output is not a reranked batch");
        assert_eq!(
            error,
            RerankBackendError::Rejected(RerankRejection::Incomplete)
        );
        let outcome = score_all(
            &PartialBackend,
            &planned,
            &NeverCancelled,
            &TestClock::at(0),
            RerankFallbackPolicy::DegradeWithoutRerank,
        )
        .expect("explicit fallback policy");
        assert_eq!(
            outcome,
            RerankOutcome::Degraded {
                reason: "partial_output"
            }
        );
    }

    #[test]
    fn time_spent_inside_the_backend_counts_against_the_expiry() {
        // The clock starts well inside the envelope's 10_000 expiry and the
        // provider stalls past it. Reading the clock before the call — the
        // obvious mistake — would accept this response.
        let clock = TestClock::at(1_000);
        let backend = SlowBackend {
            clock: &clock,
            stall_micros: 20_000,
        };
        let planned = batches(2, 2);
        let error = score_batch(&backend, &planned.requests()[0], &clock).expect_err("refuse");
        assert_eq!(
            error,
            RerankBackendError::Rejected(RerankRejection::Expired)
        );
    }

    #[test]
    fn an_expired_batch_is_refused_before_authorized_text_is_released() {
        let backend = CountingBackend::new();
        let planned = batches(4, 2);
        let error = score_batch(&backend, &planned.requests()[0], &TestClock::at(10_000))
            .expect_err("expired before dispatch");
        assert_eq!(
            error,
            RerankBackendError::Rejected(RerankRejection::Expired)
        );
        assert_eq!(
            backend.calls.get(),
            0,
            "no batch may release authorized text at the inclusive expiry instant"
        );
    }

    #[test]
    fn a_response_arriving_inside_the_expiry_is_accepted() {
        let clock = TestClock::at(1_000);
        let backend = SlowBackend {
            clock: &clock,
            stall_micros: 500,
        };
        let planned = batches(2, 2);
        let scores = score_batch(&backend, &planned.requests()[0], &clock).expect("accepted");
        assert_eq!(scores.len(), 2);
    }

    #[test]
    fn a_failure_fails_the_query_under_the_require_policy() {
        let backend = UnavailableBackend { model_revision: 7 };
        let error = score_all(
            &backend,
            &batches(4, 2),
            &NeverCancelled,
            &TestClock::at(0),
            RerankFallbackPolicy::Require,
        )
        .expect_err("fail closed");
        assert_eq!(error, RerankBackendError::Unavailable);
    }

    #[test]
    fn a_failure_degrades_with_a_stable_reason_under_the_fuse_policy() {
        let backend = UnavailableBackend { model_revision: 7 };
        let outcome = score_all(
            &backend,
            &batches(4, 2),
            &NeverCancelled,
            &TestClock::at(0),
            RerankFallbackPolicy::DegradeWithoutRerank,
        )
        .expect("degrade");
        assert_eq!(
            outcome,
            RerankOutcome::Degraded {
                reason: "unavailable"
            }
        );
    }

    /// Answers the first batch and then goes away.
    struct FlakyBackend;

    impl RerankBackend for FlakyBackend {
        fn model_revision(&self) -> u64 {
            7
        }

        fn score(
            &self,
            request: &RerankRequest,
            deadline_micros: u64,
        ) -> BackendResult<WireRerankResponse> {
            if request.request_id().get() == 100 {
                return DeterministicRerankBackend::new(7).score(request, deadline_micros);
            }
            Err(RerankBackendError::Timeout)
        }
    }

    #[test]
    fn a_partial_failure_never_yields_a_half_reranked_ordering() {
        let error = score_all(
            &FlakyBackend,
            &batches(4, 2),
            &NeverCancelled,
            &TestClock::at(0),
            RerankFallbackPolicy::Require,
        )
        .expect_err("fail closed");
        assert_eq!(error, RerankBackendError::Timeout);
    }

    #[test]
    fn a_partial_failure_discards_the_batches_that_did_succeed() {
        let outcome = score_all(
            &FlakyBackend,
            &batches(4, 2),
            &NeverCancelled,
            &TestClock::at(0),
            RerankFallbackPolicy::DegradeWithoutRerank,
        )
        .expect("degrade");
        assert_eq!(
            outcome,
            RerankOutcome::Degraded { reason: "timeout" },
            "the first batch's scores must not survive as a partial ordering"
        );
    }

    #[test]
    fn a_postgres_style_interrupt_stops_before_the_first_call() {
        let backend = CountingBackend::new();
        let error = score_all(
            &backend,
            &batches(4, 2),
            &InterruptedLikePostgres,
            &TestClock::at(0),
            RerankFallbackPolicy::Require,
        )
        .expect_err("stop");
        assert_eq!(error, RerankBackendError::Cancelled);
        assert_eq!(
            backend.calls.get(),
            0,
            "no authorized text may be released after cancellation"
        );
    }

    #[test]
    fn cancellation_is_never_degraded_into_a_usable_answer() {
        let backend = CountingBackend::new();
        let error = score_all(
            &backend,
            &batches(4, 2),
            &InterruptedLikePostgres,
            &TestClock::at(0),
            RerankFallbackPolicy::DegradeWithoutRerank,
        )
        .expect_err("stop");
        assert_eq!(error, RerankBackendError::Cancelled);
        assert_eq!(backend.calls.get(), 0);
    }

    #[test]
    fn a_provider_diagnostic_cannot_flood_a_log() {
        let error = RerankBackendError::transport("!".repeat(10_000));
        let RerankBackendError::Transport { reason } = &error else {
            unreachable!("constructed a transport failure")
        };
        assert_eq!(reason.len(), crate::MAX_RERANK_DIAGNOSTIC_BYTES);
    }

    #[test]
    fn bounding_a_diagnostic_never_splits_a_character() {
        let error = RerankBackendError::transport("é".repeat(1_000));
        let RerankBackendError::Transport { reason } = &error else {
            unreachable!("constructed a transport failure")
        };
        assert!(reason.len() <= crate::MAX_RERANK_DIAGNOSTIC_BYTES);
        assert!(reason.chars().all(|character| character == 'é'));
    }
    use std::collections::BTreeSet;
