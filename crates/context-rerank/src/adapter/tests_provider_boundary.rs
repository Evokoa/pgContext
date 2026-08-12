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
                model: request.model().as_str().to_owned(),
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

#[test]
fn output_memory_charges_only_the_largest_possible_winning_keys() {
    let rows = [1_usize, 31, 511]
        .into_iter()
        .enumerate()
        .map(|(index, key_bytes)| {
            HydratedCandidate::new(
                PointId::new(u64::try_from(index + 1).expect("small point")),
                SourceKey::new("k".repeat(key_bytes)).expect("bounded source key"),
                1.0,
            )
            .expect("hydrated candidate")
        })
        .collect::<Vec<_>>();

    let one = fixed_adapter_memory_bytes(&rows, 1).expect("one output projection");
    let all = fixed_adapter_memory_bytes(&rows, rows.len()).expect("all output projection");
    let two_extra_slots = 2 * size_of::<HydratedCandidate>();
    assert_eq!(all - one, two_extra_slots + 1 + 31);
}

#[test]
fn hydration_budget_covers_the_candidate_pass_and_winner_recheck() {
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
    let input = fused_rows(3);
    let first_pass = input
        .iter()
        .map(|row| row.source_key().as_str().len())
        .sum();

    let page = adapter
        .rerank(
            &query(1),
            &input,
            1,
            constrained_budget(3, 1 << 20, first_pass),
        )
        .expect("bounded refusal");
    assert!(!page.exhausted());
    assert_eq!(rows.calls.get(), 0, "admission must precede authorization");
    assert_eq!(backend.released(), 0);
}
