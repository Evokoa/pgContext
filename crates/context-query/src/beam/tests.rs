#![allow(clippy::expect_used)]

#[path = "error_tests.rs"]
mod error_tests;

use super::*;
use crate::{Cancellation, PortBudget, QueryClock, QueryError, Result};
use context_core::{OccurrenceId, PointId};
use proptest::prelude::*;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::rc::Rc;

const VALID_USIZE_BUDGETS: [usize; 9] = [32, 32, 128, 128, 1_024, 1_024, 1 << 20, 2 << 20, 10];

fn budget_from_fields(
    values: [usize; 9],
    max_hops: u16,
    max_elapsed_micros: u64,
) -> Result<BeamBudget> {
    BeamBudget::new(
        values[0],
        values[1],
        values[2],
        values[3],
        values[4],
        values[5],
        values[6],
        values[7],
        max_hops,
        values[8],
        max_elapsed_micros,
    )
}

fn explicit_budget() -> BeamBudget {
    budget_from_fields(VALID_USIZE_BUDGETS, 8, 10_000).expect("bounded fixture budget")
}

#[test]
fn budget_rejects_zero_and_every_frozen_maximum_overflow() {
    let valid = explicit_budget();
    assert_eq!(valid.beam_width(), 32);
    assert_eq!(valid.expansion_batch(), 32);

    let maxima = [
        MAX_BEAM_WIDTH,
        MAX_BEAM_EXPANSION_BATCH,
        MAX_BEAM_ADMITTED_STATES,
        MAX_BEAM_VISITED_KEYS,
        MAX_BEAM_VECTOR_EXPANSIONS,
        MAX_BEAM_EXACT_RERANKS,
        MAX_BEAM_PARENT_BYTES,
        MAX_BEAM_RETAINED_BYTES,
        context_core::policy::MAX_SEARCH_LIMIT,
    ];
    for (index, maximum) in maxima.into_iter().enumerate() {
        let mut zero = VALID_USIZE_BUDGETS;
        zero[index] = 0;
        assert!(budget_from_fields(zero, 8, 10_000).is_err());

        let mut overflow = VALID_USIZE_BUDGETS;
        overflow[index] = maximum + 1;
        assert!(budget_from_fields(overflow, 8, 10_000).is_err());
    }
    assert!(budget_from_fields(VALID_USIZE_BUDGETS, 0, 10_000).is_err());
    assert!(budget_from_fields(VALID_USIZE_BUDGETS, MAX_BEAM_HOPS + 1, 10_000).is_err());
    assert!(budget_from_fields(VALID_USIZE_BUDGETS, 8, 0).is_err());
    assert!(budget_from_fields(VALID_USIZE_BUDGETS, 8, MAX_BEAM_ELAPSED_MICROS + 1).is_err());
    assert!(BeamBudget::default_internal(0).is_err());
    assert!(BeamBudget::default_internal(context_core::policy::MAX_SEARCH_LIMIT + 1).is_err());
    assert!(BeamBudget::default_internal(1).is_ok());
}

#[test]
fn score_components_reject_non_finite_inputs_and_sum_overflow() {
    for invalid in [
        BeamScoreComponents::new(f64::NAN, 0.0, 0.0, None),
        BeamScoreComponents::new(0.0, f64::INFINITY, 0.0, None),
        BeamScoreComponents::new(0.0, 0.0, f64::NEG_INFINITY, None),
        BeamScoreComponents::new(0.0, 0.0, 0.0, Some(f64::NAN)),
        BeamScoreComponents::new(f64::MAX, f64::MAX, 0.0, None),
    ] {
        assert!(matches!(
            invalid,
            Err(QueryError::InvalidInput {
                field: "beam_score",
                ..
            })
        ));
    }
    let scores =
        BeamScoreComponents::new(0.5, 0.25, 1.0, Some(0.75)).expect("finite score components");
    assert_eq!(scores.ranking(), 2.0);
    let negative_zero = BeamScoreComponents::new(-0.0, 0.0, 0.0, None).expect("finite signed zero");
    assert_eq!(negative_zero.ranking().to_bits(), (-0.0_f64).to_bits());
}

#[test]
fn stable_vocabulary_is_content_free() {
    assert_eq!(BeamTransition::Seed.stable_name(), "seed");
    assert_eq!(BeamTransition::Vector.stable_name(), "vector");
    assert_eq!(BeamCompletion::Cancelled.stable_name(), "cancelled");
    assert_eq!(
        BeamCompletion::BudgetExhausted(BeamBudgetKind::VisitedKeys).stable_name(),
        "visited_keys"
    );
}

fn occurrence(value: u64) -> OccurrenceId {
    OccurrenceId::new(value).expect("nonzero occurrence")
}

fn authorization(value: u64) -> AuthorizationContextToken {
    AuthorizationContextToken::new(value).expect("nonzero authorization")
}

fn seed(id: u64, score: f64) -> BeamSeed {
    BeamSeed::new(
        occurrence(id),
        PointId::new(id),
        Some(BeamNodeId::new(id)),
        PathPatternState::default(),
        authorization(1),
        BeamScoreComponents::new(score, 0.0, 0.0, None).expect("finite seed score"),
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "boundary tests vary each independent hard resource"
)]
fn budget(
    width: usize,
    batch: usize,
    states: usize,
    visited: usize,
    vector: usize,
    reranks: usize,
    parent_bytes: usize,
    retained_bytes: usize,
    hops: u16,
    results: usize,
    elapsed: u64,
) -> BeamBudget {
    BeamBudget::new(
        width,
        batch,
        states,
        visited,
        vector,
        reranks,
        parent_bytes,
        retained_bytes,
        hops,
        results,
        elapsed,
    )
    .expect("bounded beam budget")
}

fn roomy_budget(results: usize) -> BeamBudget {
    budget(
        32,
        32,
        512,
        512,
        4_096,
        4_096,
        1 << 20,
        4 << 20,
        16,
        results,
        10_000,
    )
}

#[derive(Clone, Copy, Debug)]
struct Edge {
    child: u64,
    score: f64,
    exact: Option<f64>,
    pattern: u32,
}

#[derive(Default)]
struct GraphProvider {
    edges: BTreeMap<u64, Vec<Edge>>,
    reverse: bool,
    calls: usize,
    cancel_after_call: Option<Rc<Cell<bool>>>,
}

impl BeamExpansionProvider for GraphProvider {
    fn expand(
        &mut self,
        request: &BeamProviderRequest,
        _budget: PortBudget,
    ) -> Result<BeamExpansionBatch> {
        self.calls += 1;
        let mut expansions = Vec::new();
        for parent in request.parents() {
            if let Some(edges) = self.edges.get(&parent.occurrence_id().get()) {
                for edge in edges {
                    if expansions.len() == request.max_expansions() {
                        break;
                    }
                    expansions.push(BeamExpansion::new(
                        parent.state_id(),
                        occurrence(edge.child),
                        PointId::new(edge.child),
                        Some(BeamNodeId::new(edge.child)),
                        None,
                        PathPatternState::new(edge.pattern),
                        edge.score,
                        0.0,
                        edge.exact,
                    )?);
                }
            }
        }
        if self.reverse {
            expansions.reverse();
        }
        let exact = expansions
            .iter()
            .filter(|expansion| expansion.scores.exact().is_some())
            .count();
        if let Some(cancelled) = &self.cancel_after_call {
            cancelled.set(true);
        }
        Ok(BeamExpansionBatch::new(
            expansions.clone(),
            expansions.len(),
            exact,
        ))
    }
}

struct FixedClock(u64);

impl QueryClock for FixedClock {
    fn now_micros(&self) -> u64 {
        self.0
    }
}

struct StepClock {
    now: Cell<u64>,
    step: u64,
}

struct SharedClock(Rc<Cell<u64>>);

impl QueryClock for SharedClock {
    fn now_micros(&self) -> u64 {
        self.0.get()
    }
}

impl QueryClock for StepClock {
    fn now_micros(&self) -> u64 {
        let now = self.now.get();
        self.now.set(now.saturating_add(self.step));
        now
    }
}

#[derive(Default)]
struct NeverCancelled;

impl Cancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

struct SharedCancellation(Rc<Cell<bool>>);

impl Cancellation for SharedCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.get()
    }
}

#[test]
fn graph_off_seed_selection_matches_the_exhaustive_oracle() {
    let seeds = [seed(3, 0.5), seed(1, 0.9), seed(2, 0.9), seed(4, -1.0)];
    let outcome = VirtualBeamEngine::new(roomy_budget(3))
        .run(
            &seeds,
            &mut GraphProvider::default(),
            &NeverCancelled,
            &FixedClock(0),
        )
        .expect("graph-off beam");
    assert_eq!(outcome.completion(), BeamCompletion::Exhausted);
    assert_eq!(
        outcome
            .hits()
            .iter()
            .map(|hit| hit.occurrence_id().get())
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(outcome.hits().iter().all(|hit| hit.path().len() == 1));
    assert_eq!(outcome.diagnostics().provider_calls(), 1);
}

#[test]
fn empty_later_batch_finalizes_from_every_dominant_state() {
    let mut provider = GraphProvider {
        edges: BTreeMap::from([(
            1,
            vec![Edge {
                child: 2,
                score: -5.0,
                exact: None,
                pattern: 0,
            }],
        )]),
        ..GraphProvider::default()
    };
    let outcome = VirtualBeamEngine::new(roomy_budget(1))
        .run(
            &[seed(1, 10.0)],
            &mut provider,
            &NeverCancelled,
            &FixedClock(0),
        )
        .expect("two-batch beam");
    assert_eq!(outcome.completion(), BeamCompletion::Exhausted);
    assert_eq!(provider.calls, 2);
    assert_eq!(outcome.hits()[0].occurrence_id(), occurrence(1));
}

#[test]
fn parent_paths_dominance_duplicates_and_cycles_are_deterministic() {
    let edges = BTreeMap::from([
        (
            1,
            vec![
                Edge {
                    child: 3,
                    score: 1.0,
                    exact: None,
                    pattern: 0,
                },
                Edge {
                    child: 3,
                    score: 1.0,
                    exact: None,
                    pattern: 0,
                },
            ],
        ),
        (
            2,
            vec![
                Edge {
                    child: 3,
                    score: 2.0,
                    exact: None,
                    pattern: 0,
                },
                Edge {
                    child: 3,
                    score: 2.0,
                    exact: None,
                    pattern: 0,
                },
            ],
        ),
        (
            3,
            vec![Edge {
                child: 2,
                score: 9.0,
                exact: None,
                pattern: 0,
            }],
        ),
    ]);
    let run = |reverse| {
        let mut provider = GraphProvider {
            edges: edges.clone(),
            reverse,
            calls: 0,
            cancel_after_call: None,
        };
        VirtualBeamEngine::new(roomy_budget(8))
            .run(
                &[seed(1, 1.0), seed(2, 0.5)],
                &mut provider,
                &NeverCancelled,
                &FixedClock(0),
            )
            .expect("cyclic vector graph")
    };
    let forward = run(false);
    let reverse = run(true);
    assert_eq!(forward.hits(), reverse.hits());
    let child = forward
        .hits()
        .iter()
        .find(|hit| hit.occurrence_id() == occurrence(3))
        .expect("dominating child");
    assert_eq!(
        child
            .path()
            .iter()
            .map(|step| step.occurrence_id().get())
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(forward.diagnostics().pruning().duplicate() >= 1);
    assert!(forward.diagnostics().pruning().dominated() >= 1);
    assert!(forward.diagnostics().pruning().cycle() >= 1);
}

#[test]
fn exact_scores_and_canonical_ties_control_membership() {
    let mut provider = GraphProvider {
        edges: BTreeMap::from([(
            1,
            vec![
                Edge {
                    child: 4,
                    score: 100.0,
                    exact: Some(0.25),
                    pattern: 0,
                },
                Edge {
                    child: 3,
                    score: 0.25,
                    exact: Some(1.0),
                    pattern: 0,
                },
                Edge {
                    child: 2,
                    score: 0.25,
                    exact: Some(1.0),
                    pattern: 0,
                },
            ],
        )]),
        ..GraphProvider::default()
    };
    let outcome = VirtualBeamEngine::new(roomy_budget(4))
        .run(
            &[seed(1, 0.0)],
            &mut provider,
            &NeverCancelled,
            &FixedClock(0),
        )
        .expect("exact-ranked beam");
    assert_eq!(
        outcome
            .hits()
            .iter()
            .map(|hit| hit.occurrence_id().get())
            .collect::<Vec<_>>(),
        vec![2, 3, 4, 1]
    );
    assert_eq!(outcome.diagnostics().exact_reranks(), 3);
}

#[test]
fn canonical_ties_use_occurrence_before_hop_and_provider_order() {
    let edges = BTreeMap::from([(
        2,
        vec![
            Edge {
                child: 1,
                score: 1.0,
                exact: None,
                pattern: 7,
            },
            Edge {
                child: 1,
                score: 2.0,
                exact: Some(1.0),
                pattern: 7,
            },
        ],
    )]);
    let run = |reverse| {
        let mut provider = GraphProvider {
            edges: edges.clone(),
            reverse,
            ..GraphProvider::default()
        };
        VirtualBeamEngine::new(roomy_budget(3))
            .run(
                &[seed(10, 1.0), seed(2, 0.0)],
                &mut provider,
                &NeverCancelled,
                &FixedClock(0),
            )
            .expect("canonical tied beam")
    };
    let forward = run(false);
    let reverse = run(true);
    assert_eq!(forward.hits(), reverse.hits());
    assert_eq!(forward.hits()[0].occurrence_id(), occurrence(1));
    assert_eq!(forward.hits()[1].occurrence_id(), occurrence(10));
}

#[test]
fn occurrence_identity_rejects_cross_pattern_and_authorization_point_changes() {
    for changed in [
        BeamSeed::new(
            occurrence(1),
            PointId::new(2),
            None,
            PathPatternState::new(9),
            authorization(1),
            BeamScoreComponents::new(1.0, 0.0, 0.0, None).expect("score"),
        ),
        BeamSeed::new(
            occurrence(1),
            PointId::new(2),
            None,
            PathPatternState::default(),
            authorization(2),
            BeamScoreComponents::new(1.0, 0.0, 0.0, None).expect("score"),
        ),
    ] {
        let result = VirtualBeamEngine::new(roomy_budget(2)).run(
            &[seed(1, 0.0), changed],
            &mut GraphProvider::default(),
            &NeverCancelled,
            &FixedClock(0),
        );
        assert!(matches!(
            result,
            Err(QueryError::InvariantViolation {
                operation: "beam_occurrence_point_identity"
            })
        ));
    }
}

proptest! {
    #[test]
    fn arbitrary_graph_off_seeds_match_an_independent_sort(
        raw in prop::collection::vec((-10_000_i32..10_000, 1_u64..2_000), 1..64),
        top_k in 1_usize..16,
    ) {
        let mut by_occurrence = BTreeMap::new();
        for (score, id) in raw {
            by_occurrence
                .entry(id)
                .and_modify(|best: &mut i32| *best = (*best).max(score))
                .or_insert(score);
        }
        let seeds = by_occurrence
            .iter()
            .map(|(id, score)| seed(*id, f64::from(*score)))
            .collect::<Vec<_>>();
        let mut oracle = by_occurrence.into_iter().collect::<Vec<_>>();
        oracle.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        oracle.truncate(top_k.min(oracle.len()));
        let outcome = VirtualBeamEngine::new(roomy_budget(top_k))
            .run(
                &seeds,
                &mut GraphProvider::default(),
                &NeverCancelled,
                &FixedClock(0),
            )
            .expect("property beam");
        prop_assert_eq!(
            outcome
                .hits()
                .iter()
                .map(|hit| hit.occurrence_id().get())
                .collect::<Vec<_>>(),
            oracle.into_iter().map(|(id, _)| id).collect::<Vec<_>>(),
        );
    }
}

struct MalformedProvider {
    kind: &'static str,
}

impl BeamExpansionProvider for MalformedProvider {
    fn expand(
        &mut self,
        request: &BeamProviderRequest,
        _budget: PortBudget,
    ) -> Result<BeamExpansionBatch> {
        let parent = request.parents()[0];
        let expansion = BeamExpansion::new(
            if self.kind == "parent" {
                BeamStateId::fixture(999)
            } else {
                parent.state_id()
            },
            occurrence(2),
            PointId::new(2),
            None,
            (self.kind == "topology").then(|| TopologyNodeId::new(2)),
            PathPatternState::default(),
            1.0,
            0.0,
            (self.kind == "rerank").then_some(1.0),
        )?;
        match self.kind {
            "overreturn" => Ok(BeamExpansionBatch::new(
                vec![expansion; request.max_expansions() + 1],
                request.max_expansions() + 1,
                0,
            )),
            "rerank" => Ok(BeamExpansionBatch::new(vec![expansion], 1, 0)),
            _ => Ok(BeamExpansionBatch::new(vec![expansion], 1, 0)),
        }
    }
}

#[test]
fn malformed_provider_batches_fail_without_partial_output() {
    for kind in ["overreturn", "parent", "topology", "rerank"] {
        let result =
            VirtualBeamEngine::new(budget(2, 1, 8, 8, 8, 8, 1 << 20, 2 << 20, 2, 2, 10_000)).run(
                &[seed(1, 0.0)],
                &mut MalformedProvider { kind },
                &NeverCancelled,
                &FixedClock(0),
            );
        assert!(result.is_err(), "{kind} must be rejected");
    }
}

#[test]
fn cancellation_precedes_provider_work_and_response_admission() {
    let cancelled = Rc::new(Cell::new(true));
    let mut provider = GraphProvider::default();
    let outcome = VirtualBeamEngine::new(roomy_budget(2))
        .run(
            &[seed(1, 0.0)],
            &mut provider,
            &SharedCancellation(Rc::clone(&cancelled)),
            &FixedClock(0),
        )
        .expect("pre-cancelled beam");
    assert_eq!(outcome.completion(), BeamCompletion::Cancelled);
    assert_eq!(provider.calls, 0);

    cancelled.set(false);
    let mut provider = GraphProvider {
        edges: BTreeMap::from([(
            1,
            vec![Edge {
                child: 2,
                score: 10.0,
                exact: None,
                pattern: 0,
            }],
        )]),
        cancel_after_call: Some(Rc::clone(&cancelled)),
        ..GraphProvider::default()
    };
    let outcome = VirtualBeamEngine::new(roomy_budget(2))
        .run(
            &[seed(1, 0.0)],
            &mut provider,
            &SharedCancellation(cancelled),
            &FixedClock(0),
        )
        .expect("cancelled response");
    assert_eq!(outcome.completion(), BeamCompletion::Cancelled);
    assert_eq!(outcome.hits().len(), 1);
    assert_eq!(outcome.hits()[0].occurrence_id(), occurrence(1));
}

#[test]
fn elapsed_deadline_is_rechecked_after_empty_and_nonempty_provider_responses() {
    struct SlowProvider {
        now: Rc<Cell<u64>>,
        nonempty: bool,
    }
    impl BeamExpansionProvider for SlowProvider {
        fn expand(
            &mut self,
            request: &BeamProviderRequest,
            _budget: PortBudget,
        ) -> Result<BeamExpansionBatch> {
            self.now.set(10);
            if self.nonempty {
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
            } else {
                Ok(BeamExpansionBatch::new(Vec::new(), 0, 0))
            }
        }
    }
    for nonempty in [false, true] {
        let now = Rc::new(Cell::new(0));
        let outcome = VirtualBeamEngine::new(budget(2, 2, 8, 8, 8, 8, 1 << 20, 2 << 20, 4, 2, 10))
            .run(
                &[seed(1, 0.0)],
                &mut SlowProvider {
                    now: Rc::clone(&now),
                    nonempty,
                },
                &NeverCancelled,
                &SharedClock(now),
            )
            .expect("elapsed provider outcome");
        assert_eq!(
            outcome.completion(),
            BeamCompletion::BudgetExhausted(BeamBudgetKind::Elapsed)
        );
        assert_eq!(outcome.hits().len(), 1);
    }
}

#[test]
fn every_hard_budget_returns_an_incomplete_typed_outcome() {
    let seeds = [seed(1, 2.0), seed(2, 1.0)];
    let cases = [
        (
            budget(2, 1, 1, 2, 8, 8, 1 << 20, 2 << 20, 2, 2, 10_000),
            BeamBudgetKind::AdmittedStates,
        ),
        (
            budget(2, 1, 2, 1, 8, 8, 1 << 20, 2 << 20, 2, 2, 10_000),
            BeamBudgetKind::VisitedKeys,
        ),
        (
            budget(2, 1, 8, 8, 8, 8, 1, 2 << 20, 2, 2, 10_000),
            BeamBudgetKind::ParentBytes,
        ),
        (
            budget(2, 1, 8, 8, 8, 8, 1 << 20, 1, 2, 2, 10_000),
            BeamBudgetKind::RetainedBytes,
        ),
    ];
    for (budget, kind) in cases {
        let outcome = VirtualBeamEngine::new(budget)
            .run(
                &seeds,
                &mut GraphProvider::default(),
                &NeverCancelled,
                &FixedClock(0),
            )
            .expect("typed budget outcome");
        assert_eq!(outcome.completion(), BeamCompletion::BudgetExhausted(kind));
        assert!(!outcome.completion().is_complete());
    }

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
        ..GraphProvider::default()
    };
    let outcome = VirtualBeamEngine::new(budget(1, 1, 8, 8, 1, 8, 1 << 20, 2 << 20, 4, 4, 10_000))
        .run(
            &[seed(1, 0.0)],
            &mut provider,
            &NeverCancelled,
            &FixedClock(0),
        )
        .expect("vector budget outcome");
    assert_eq!(
        outcome.completion(),
        BeamCompletion::BudgetExhausted(BeamBudgetKind::VectorExpansions)
    );

    let outcome = VirtualBeamEngine::new(budget(1, 1, 8, 8, 8, 8, 1 << 20, 2 << 20, 4, 4, 5))
        .run(
            &[seed(1, 0.0)],
            &mut GraphProvider::default(),
            &NeverCancelled,
            &StepClock {
                now: Cell::new(0),
                step: 5,
            },
        )
        .expect("elapsed budget outcome");
    assert_eq!(
        outcome.completion(),
        BeamCompletion::BudgetExhausted(BeamBudgetKind::Elapsed)
    );
}

#[test]
fn beam_width_hub_and_hop_caps_bound_provider_work() {
    let hub = (2_u32..=32)
        .map(|child| Edge {
            child: u64::from(child),
            score: f64::from(child),
            exact: None,
            pattern: 0,
        })
        .collect::<Vec<_>>();
    let mut provider = GraphProvider {
        edges: BTreeMap::from([(1, hub)]),
        ..GraphProvider::default()
    };
    let outcome = VirtualBeamEngine::new(budget(
        4,
        16,
        64,
        64,
        64,
        64,
        1 << 20,
        2 << 20,
        1,
        8,
        10_000,
    ))
    .run(
        &[seed(1, 0.0)],
        &mut provider,
        &NeverCancelled,
        &FixedClock(0),
    )
    .expect("bounded hub");
    assert_eq!(outcome.completion(), BeamCompletion::Exhausted);
    assert_eq!(outcome.diagnostics().vector_expansions(), 16);
    assert_eq!(outcome.diagnostics().pruning().beam_width(), 12);
    assert_eq!(outcome.diagnostics().pruning().hop(), 4);
}

#[test]
fn parent_and_output_memory_boundaries_are_inclusive() {
    let baseline_budget = budget(1, 1, 512, 512, 1, 1, 1 << 20, 4 << 20, 16, 2, 10_000);
    let baseline = VirtualBeamEngine::new(baseline_budget)
        .run(
            &[seed(1, 1.0)],
            &mut GraphProvider::default(),
            &NeverCancelled,
            &FixedClock(0),
        )
        .expect("baseline memory projection");
    let parent_bytes = baseline.diagnostics().parent_bytes();
    let retained_bytes = baseline.diagnostics().retained_bytes();

    let exact = budget(
        1,
        1,
        512,
        512,
        1,
        1,
        parent_bytes,
        retained_bytes,
        16,
        2,
        10_000,
    );
    let outcome = VirtualBeamEngine::new(exact)
        .run(
            &[seed(1, 1.0)],
            &mut GraphProvider::default(),
            &NeverCancelled,
            &FixedClock(0),
        )
        .expect("inclusive memory boundary");
    assert_eq!(outcome.completion(), BeamCompletion::Exhausted);
    assert_eq!(outcome.hits().len(), 1);

    let parent_short = budget(
        1,
        1,
        512,
        512,
        1,
        1,
        parent_bytes - 1,
        retained_bytes,
        16,
        2,
        10_000,
    );
    let outcome = VirtualBeamEngine::new(parent_short)
        .run(
            &[seed(1, 1.0)],
            &mut GraphProvider::default(),
            &NeverCancelled,
            &FixedClock(0),
        )
        .expect("exclusive parent boundary");
    assert_eq!(
        outcome.completion(),
        BeamCompletion::BudgetExhausted(BeamBudgetKind::ParentBytes)
    );

    let output_short = budget(
        1,
        1,
        512,
        512,
        1,
        1,
        parent_bytes,
        retained_bytes - 1,
        16,
        2,
        10_000,
    );
    let outcome = VirtualBeamEngine::new(output_short)
        .run(
            &[seed(1, 1.0)],
            &mut GraphProvider::default(),
            &NeverCancelled,
            &FixedClock(0),
        )
        .expect("exclusive output boundary");
    assert_eq!(
        outcome.completion(),
        BeamCompletion::BudgetExhausted(BeamBudgetKind::RetainedBytes)
    );
}

#[test]
fn full_response_transient_memory_boundary_is_inclusive_and_adaptive() {
    let edges = BTreeMap::from([(
        1,
        vec![
            Edge {
                child: 2,
                score: 2.0,
                exact: None,
                pattern: 0,
            },
            Edge {
                child: 3,
                score: 1.0,
                exact: None,
                pattern: 0,
            },
        ],
    )]);
    let run = |retained_bytes| {
        VirtualBeamEngine::new(budget(
            2,
            2,
            16,
            16,
            16,
            16,
            1 << 20,
            retained_bytes,
            4,
            3,
            10_000,
        ))
        .run(
            &[seed(1, 0.0)],
            &mut GraphProvider {
                edges: edges.clone(),
                ..GraphProvider::default()
            },
            &NeverCancelled,
            &FixedClock(0),
        )
        .expect("full response memory outcome")
    };
    let baseline = run(2 << 20);
    assert_eq!(baseline.completion(), BeamCompletion::Exhausted);
    let mut excluded = 1_usize;
    let mut admitted = 2 << 20;
    while excluded + 1 < admitted {
        let middle = excluded + (admitted - excluded) / 2;
        if run(middle).completion() == BeamCompletion::Exhausted {
            admitted = middle;
        } else {
            excluded = middle;
        }
    }
    assert_eq!(run(admitted).completion(), BeamCompletion::Exhausted);
    assert_eq!(
        run(excluded).completion(),
        BeamCompletion::BudgetExhausted(BeamBudgetKind::RetainedBytes)
    );
}
