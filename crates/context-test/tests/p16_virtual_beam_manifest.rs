//! Phase 16 virtual-beam certification manifest contract.

use context_query::{BeamBudgetKind, BeamCompletion, BeamTransition};
use context_test::*;

#[test]
fn p16_manifest_freezes_state_and_terminal_vocabulary() {
    assert_eq!(
        [BeamTransition::Seed, BeamTransition::Vector].map(BeamTransition::stable_name),
        P16_TRANSITIONS
    );
    let terminations = [
        BeamCompletion::Exhausted,
        BeamCompletion::Cancelled,
        BeamCompletion::BudgetExhausted(BeamBudgetKind::AdmittedStates),
        BeamCompletion::BudgetExhausted(BeamBudgetKind::VisitedKeys),
        BeamCompletion::BudgetExhausted(BeamBudgetKind::VectorExpansions),
        BeamCompletion::BudgetExhausted(BeamBudgetKind::ExactReranks),
        BeamCompletion::BudgetExhausted(BeamBudgetKind::ParentBytes),
        BeamCompletion::BudgetExhausted(BeamBudgetKind::RetainedBytes),
        BeamCompletion::BudgetExhausted(BeamBudgetKind::Elapsed),
    ];
    assert_eq!(
        terminations.map(BeamCompletion::stable_name),
        P16_TERMINATIONS
    );
    assert_eq!(
        P16_PRUNING_REASONS,
        ["duplicate", "dominated", "cycle", "beam_width", "hop"]
    );
}

#[test]
fn p16_manifest_freezes_every_hard_resource_and_threshold() {
    assert_eq!(P16_DEFAULT_BEAM_WIDTH, 32);
    assert_eq!(P16_MAX_BEAM_WIDTH, 256);
    assert_eq!(P16_DEFAULT_EXPANSION_BATCH, 32);
    assert_eq!(P16_MAX_EXPANSION_BATCH, 256);
    assert_eq!(P16_MAX_ADMITTED_STATES, 65_536);
    assert_eq!(P16_MAX_VISITED_KEYS, 65_536);
    assert_eq!(P16_MAX_VECTOR_EXPANSIONS, 10_000_000);
    assert_eq!(P16_MAX_EXACT_RERANKS, 10_000_000);
    assert_eq!(P16_MAX_HOPS, 64);
    assert_eq!(P16_MAX_PARENT_BYTES, 16 * 1024 * 1024);
    assert_eq!(P16_MAX_RETAINED_BYTES, 256 * 1024 * 1024);
    assert_eq!(P16_MAX_ELAPSED_MICROS, 60_000_000);
    assert_eq!(P16_MAX_RESULTS, 10_000);
    assert_eq!(P16_MAX_P50_LATENCY_RATIO_BPS, 11_000);
    assert_eq!(P16_MAX_RETAINED_MEMORY_RATIO_BPS, 11_000);
}

#[test]
fn p16_manifest_freezes_fixture_identity_before_timing() {
    assert_eq!(P16_DATASET_SHA256.len(), 64);
    assert_eq!(P16_WORKLOAD_SHA256.len(), 64);
    assert_eq!(P16_QUERY_COUNT, 128);
    assert_eq!(P16_TIMING_REPEATS, 11);
    assert_eq!(P16_REPORT_MARKERS.len(), 12);
    assert_eq!(P16_GRAPH_OFF_ORACLE_FNV64, 0x36e3_f0bf_52dd_bbac);
    assert_eq!(P16_EXPANDED_BEAM_ORACLE_FNV64, 0x0d87_b67f_96b8_8dd5);
    assert_eq!(p16_virtual_beam_manifest_hash(), 0x02fe_7d74_4c48_4dcf);
}
