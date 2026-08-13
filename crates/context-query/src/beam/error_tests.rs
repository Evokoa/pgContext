use super::*;

struct ErrorProvider;

impl BeamExpansionProvider for ErrorProvider {
    fn expand(
        &mut self,
        _request: &BeamProviderRequest,
        _budget: PortBudget,
    ) -> Result<BeamExpansionBatch> {
        Err(QueryError::PortFailure {
            stage: "beam_fixture",
            message: "injected".to_owned(),
        })
    }
}

#[test]
fn provider_and_interrupt_errors_propagate_without_partial_output() {
    let provider_error = VirtualBeamEngine::new(roomy_budget(2)).run(
        &[seed(1, 0.0)],
        &mut ErrorProvider,
        &NeverCancelled,
        &FixedClock(0),
    );
    assert!(matches!(
        provider_error,
        Err(QueryError::PortFailure {
            stage: "beam_fixture",
            ..
        })
    ));

    struct InterruptFailure;
    impl Cancellation for InterruptFailure {
        fn check_interrupt(&self) -> Result<()> {
            Err(QueryError::PortFailure {
                stage: "beam_interrupt",
                message: "injected".to_owned(),
            })
        }

        fn is_cancelled(&self) -> bool {
            false
        }
    }

    let mut provider = GraphProvider::default();
    let interrupt_error = VirtualBeamEngine::new(roomy_budget(2)).run(
        &[seed(1, 0.0)],
        &mut provider,
        &InterruptFailure,
        &FixedClock(0),
    );
    assert!(matches!(
        interrupt_error,
        Err(QueryError::PortFailure {
            stage: "beam_interrupt",
            ..
        })
    ));
    assert_eq!(provider.calls, 0);
}

#[test]
fn exact_rerank_exhaustion_stops_before_the_second_provider_call() {
    struct ExactChain;
    impl BeamExpansionProvider for ExactChain {
        fn expand(
            &mut self,
            request: &BeamProviderRequest,
            _budget: PortBudget,
        ) -> Result<BeamExpansionBatch> {
            let parent = request.parents()[0];
            let child = parent.occurrence_id().get() + 1;
            let expansion = BeamExpansion::new(
                parent.state_id(),
                occurrence(child),
                PointId::new(child),
                None,
                None,
                PathPatternState::default(),
                1.0,
                0.0,
                Some(1.0),
            )?;
            Ok(BeamExpansionBatch::new(vec![expansion], 1, 1))
        }
    }

    let outcome = VirtualBeamEngine::new(budget(1, 1, 8, 8, 8, 1, 1 << 20, 2 << 20, 4, 4, 10_000))
        .run(
            &[seed(1, 0.0)],
            &mut ExactChain,
            &NeverCancelled,
            &FixedClock(0),
        )
        .expect("typed exact budget outcome");
    assert_eq!(
        outcome.completion(),
        BeamCompletion::BudgetExhausted(BeamBudgetKind::ExactReranks)
    );
    assert_eq!(outcome.diagnostics().provider_calls(), 1);
}

#[test]
fn public_diagnostics_and_paths_do_not_expose_authorization_tokens() {
    let token = authorization(0xdead_beef);
    let seed = BeamSeed::new(
        occurrence(1),
        PointId::new(1),
        None,
        PathPatternState::default(),
        token,
        BeamScoreComponents::new(1.0, 0.0, 0.0, None).expect("finite score"),
    );
    let outcome = VirtualBeamEngine::new(roomy_budget(1))
        .run(
            &[seed],
            &mut GraphProvider::default(),
            &NeverCancelled,
            &FixedClock(0),
        )
        .expect("content-free outcome");
    let rendered = format!("{:?}{:?}", outcome.diagnostics(), outcome.hits());
    assert!(!rendered.contains("3735928559"));
    assert_eq!(outcome.hits()[0].path()[0].occurrence_id(), occurrence(1));
}

fn terminal_budget(
    retained_bytes: usize,
    vector_expansions: usize,
    exact_reranks: usize,
    elapsed_micros: u64,
) -> BeamBudget {
    budget(
        2,
        2,
        16,
        16,
        vector_expansions,
        exact_reranks,
        1 << 20,
        retained_bytes,
        4,
        2,
        elapsed_micros,
    )
}

#[test]
fn post_provider_terminals_release_transients_at_the_exact_memory_boundary() {
    fn cancelled_run(retained_bytes: usize) -> BeamOutcome {
        let cancelled = Rc::new(Cell::new(false));
        let mut provider = GraphProvider {
            edges: BTreeMap::from([(
                1,
                vec![Edge {
                    child: 2,
                    score: 1.0,
                    exact: None,
                    pattern: 0,
                }],
            )]),
            cancel_after_call: Some(Rc::clone(&cancelled)),
            ..GraphProvider::default()
        };
        VirtualBeamEngine::new(terminal_budget(retained_bytes, 8, 8, 10_000))
            .run(
                &[seed(1, 0.0)],
                &mut provider,
                &SharedCancellation(cancelled),
                &FixedClock(0),
            )
            .expect("cancelled terminal outcome")
    }

    struct SlowResponse(Rc<Cell<u64>>);
    impl BeamExpansionProvider for SlowResponse {
        fn expand(
            &mut self,
            request: &BeamProviderRequest,
            _budget: PortBudget,
        ) -> Result<BeamExpansionBatch> {
            self.0.set(10);
            Ok(BeamExpansionBatch::new(
                vec![BeamExpansion::new(
                    request.parents()[0].state_id(),
                    occurrence(2),
                    PointId::new(2),
                    None,
                    None,
                    PathPatternState::default(),
                    1.0,
                    0.0,
                    None,
                )?],
                1,
                0,
            ))
        }
    }
    fn elapsed_run(retained_bytes: usize) -> BeamOutcome {
        let now = Rc::new(Cell::new(0));
        VirtualBeamEngine::new(terminal_budget(retained_bytes, 8, 8, 10))
            .run(
                &[seed(1, 0.0)],
                &mut SlowResponse(Rc::clone(&now)),
                &NeverCancelled,
                &SharedClock(now),
            )
            .expect("elapsed terminal outcome")
    }

    for (baseline, expected, rerun) in [
        (
            cancelled_run(4 << 20),
            BeamCompletion::Cancelled,
            cancelled_run as fn(usize) -> BeamOutcome,
        ),
        (
            elapsed_run(4 << 20),
            BeamCompletion::BudgetExhausted(BeamBudgetKind::Elapsed),
            elapsed_run as fn(usize) -> BeamOutcome,
        ),
    ] {
        assert_eq!(baseline.completion(), expected);
        let exact_bytes = baseline.diagnostics().retained_bytes();
        let exact = rerun(exact_bytes);
        assert_eq!(exact.completion(), expected);
        assert!(exact.diagnostics().retained_bytes() <= exact_bytes);
    }
}

#[test]
fn pre_provider_work_terminals_release_parents_at_the_exact_memory_boundary() {
    struct OneChild {
        exact: bool,
    }
    impl BeamExpansionProvider for OneChild {
        fn expand(
            &mut self,
            request: &BeamProviderRequest,
            _budget: PortBudget,
        ) -> Result<BeamExpansionBatch> {
            let parent = request.parents()[0];
            let child = parent.occurrence_id().get() + 1;
            Ok(BeamExpansionBatch::new(
                vec![BeamExpansion::new(
                    parent.state_id(),
                    occurrence(child),
                    PointId::new(child),
                    None,
                    None,
                    PathPatternState::default(),
                    1.0,
                    0.0,
                    self.exact.then_some(1.0),
                )?],
                1,
                usize::from(self.exact),
            ))
        }
    }
    fn work_run(retained_bytes: usize, exact: bool) -> BeamOutcome {
        VirtualBeamEngine::new(terminal_budget(
            retained_bytes,
            if exact { 2 } else { 1 },
            1,
            10_000,
        ))
        .run(
            &[seed(1, 0.0)],
            &mut OneChild { exact },
            &NeverCancelled,
            &FixedClock(0),
        )
        .expect("work terminal outcome")
    }

    for (exact, expected) in [
        (
            false,
            BeamCompletion::BudgetExhausted(BeamBudgetKind::VectorExpansions),
        ),
        (
            true,
            BeamCompletion::BudgetExhausted(BeamBudgetKind::ExactReranks),
        ),
    ] {
        let baseline = work_run(4 << 20, exact);
        assert_eq!(baseline.completion(), expected);
        let exact_bytes = baseline.diagnostics().retained_bytes();
        let boundary = work_run(exact_bytes, exact);
        assert_eq!(boundary.completion(), expected);
        assert!(boundary.diagnostics().retained_bytes() <= exact_bytes);
    }
}

#[test]
fn one_parent_request_is_reserved_before_the_exact_provider_boundary() {
    struct CapacityProvider {
        calls: Rc<Cell<usize>>,
        capacity: Rc<Cell<usize>>,
    }
    impl BeamExpansionProvider for CapacityProvider {
        fn expand(
            &mut self,
            request: &BeamProviderRequest,
            _budget: PortBudget,
        ) -> Result<BeamExpansionBatch> {
            self.calls.set(self.calls.get() + 1);
            self.capacity.set(request.parents.capacity());
            Err(QueryError::PortFailure {
                stage: "beam_request_boundary",
                message: "injected after admission".to_owned(),
            })
        }
    }
    let run = |retained_bytes| {
        let calls = Rc::new(Cell::new(0));
        let capacity = Rc::new(Cell::new(0));
        let outcome = VirtualBeamEngine::new(terminal_budget(retained_bytes, 8, 8, 10_000)).run(
            &[seed(1, 0.0)],
            &mut CapacityProvider {
                calls: Rc::clone(&calls),
                capacity: Rc::clone(&capacity),
            },
            &NeverCancelled,
            &FixedClock(0),
        );
        (outcome, calls.get(), capacity.get())
    };

    let mut excluded = 1_usize;
    let mut admitted = 4 << 20;
    assert_eq!(run(admitted).1, 1);
    while excluded + 1 < admitted {
        let middle = excluded + (admitted - excluded) / 2;
        if run(middle).1 == 0 {
            excluded = middle;
        } else {
            admitted = middle;
        }
    }
    let (exact, exact_calls, exact_capacity) = run(admitted);
    assert_eq!(exact_calls, 1);
    assert!(exact_capacity <= 4);
    assert!(matches!(
        exact,
        Err(QueryError::PortFailure {
            stage: "beam_request_boundary",
            ..
        })
    ));
    let (below, below_calls, _) = run(excluded);
    assert_eq!(below_calls, 0);
    assert_eq!(
        below.expect("pre-provider budget outcome").completion(),
        BeamCompletion::BudgetExhausted(BeamBudgetKind::RetainedBytes)
    );
}
