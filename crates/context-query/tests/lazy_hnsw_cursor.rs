//! Query-owned lazy-cursor contract tests.

use context_query::{
    CandidatePage, DEFAULT_LAZY_CURSOR_BATCH, LazyCursorAdvance, LazyCursorControl, LazyCursorPage,
    LazyCursorTermination, LazyCursorWork, MAX_LAZY_CURSOR_BATCH, QueryError, QueryIr, QueryKind,
    ScoreOrder,
};

#[test]
fn lazy_cursor_batch_bounds_are_typed_and_inclusive() {
    assert_eq!(
        LazyCursorAdvance::new(DEFAULT_LAZY_CURSOR_BATCH).map(LazyCursorAdvance::batch_size),
        Ok(DEFAULT_LAZY_CURSOR_BATCH)
    );
    assert_eq!(
        LazyCursorAdvance::new(MAX_LAZY_CURSOR_BATCH).map(LazyCursorAdvance::batch_size),
        Ok(MAX_LAZY_CURSOR_BATCH)
    );
    for invalid in [0, MAX_LAZY_CURSOR_BATCH + 1] {
        assert!(matches!(
            LazyCursorAdvance::new(invalid),
            Err(QueryError::InvalidInput {
                field: "lazy_cursor_batch",
                ..
            })
        ));
    }
}

#[test]
fn experimental_control_is_explicit_and_pages_cannot_overclaim_completion() {
    assert_eq!(LazyCursorControl::disabled().advance(), None);
    assert_eq!(
        LazyCursorControl::experimental(7)
            .and_then(|control| control.advance().ok_or(QueryError::InvalidInput {
                field: "test",
                reason: "missing experimental advance".to_owned(),
            }))
            .map(LazyCursorAdvance::batch_size),
        Ok(7)
    );
    let work = LazyCursorWork::new(1, 1, 0, 128);
    assert!(LazyCursorPage::new(CandidatePage::new(Vec::new(), true), work, None).is_err());
    assert!(
        LazyCursorPage::new(
            CandidatePage::new(Vec::new(), true),
            work,
            Some(LazyCursorTermination::ComparisonBudget),
        )
        .is_err()
    );
    let complete = LazyCursorPage::new(
        CandidatePage::new(Vec::new(), true),
        work,
        Some(LazyCursorTermination::Exhausted),
    );
    assert_eq!(
        complete.map(|page| page.termination()),
        Ok(Some(LazyCursorTermination::Exhausted))
    );
}

#[test]
fn query_ir_keeps_lazy_control_internal_and_vector_only() -> context_query::Result<()> {
    let nearest = QueryIr::nearest(None, vec![1.0, 0.0], ScoreOrder::LowerIsBetter, None, 4)
        .and_then(|query| query.with_lazy_cursor_control(LazyCursorControl::experimental(7)?))?;
    assert_eq!(
        nearest
            .lazy_cursor_control()
            .advance()
            .map(LazyCursorAdvance::batch_size),
        Some(7)
    );

    let lookup = QueryIr::new(
        QueryKind::Lookup {
            point_ids: vec![context_core::PointId::new(1)],
        },
        ScoreOrder::HigherIsBetter,
        None,
        1,
    )?;
    assert!(matches!(
        lookup.with_lazy_cursor_control(LazyCursorControl::experimental(1)?),
        Err(QueryError::InvalidInput {
            field: "lazy_cursor",
            ..
        })
    ));
    Ok(())
}

#[test]
fn lazy_cursor_termination_registry_is_exhaustive_and_completion_is_honest() {
    let terminations = [
        LazyCursorTermination::Exhausted,
        LazyCursorTermination::Cancelled,
        LazyCursorTermination::ComparisonBudget,
        LazyCursorTermination::ExpansionBudget,
        LazyCursorTermination::EdgeBudget,
        LazyCursorTermination::MemoryBudget,
        LazyCursorTermination::AdapterError,
    ];
    assert_eq!(
        terminations.map(LazyCursorTermination::stable_name),
        [
            "exhausted",
            "cancelled",
            "comparison_budget",
            "expansion_budget",
            "edge_budget",
            "memory_budget",
            "adapter_error",
        ]
    );
    assert!(LazyCursorTermination::Exhausted.is_complete());
    assert!(terminations[1..].iter().all(|reason| !reason.is_complete()));
}

#[test]
fn lazy_cursor_work_accumulates_with_checked_arithmetic() {
    let first = LazyCursorWork::new(5, 3, 11, 1_024);
    let second = LazyCursorWork::new(7, 2, 13, 2_048);
    assert_eq!(
        first.checked_add(second),
        Ok(LazyCursorWork::new(12, 5, 24, 3_072))
    );
    assert!(matches!(
        LazyCursorWork::new(usize::MAX, 0, 0, 0).checked_add(LazyCursorWork::new(1, 0, 0, 0)),
        Err(QueryError::ArithmeticOverflow {
            operation: "lazy_cursor_work"
        })
    ));
}
