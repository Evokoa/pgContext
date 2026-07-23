//! Formula parser and bounded evaluator tests.

#![allow(clippy::expect_used)]

use context_query::{Formula, MAX_FORMULA_OPERATIONS, QueryError};

#[test]
fn formula_evaluates_arithmetic_with_precedence_and_score_aliases() {
    let formula = Formula::new("($score + 2) * -score / 2")
        .expect("bounded formula text should be accepted")
        .compile()
        .expect("documented arithmetic should compile");
    assert_eq!(formula.operation_count(), 4);
    assert_eq!(
        formula
            .evaluate(2.0, MAX_FORMULA_OPERATIONS)
            .expect("finite arithmetic should evaluate"),
        -4.0
    );
}

#[test]
fn formula_execution_rejects_constructor_compatible_opaque_text() {
    let formula =
        Formula::new("legacy opaque formula").expect("constructors preserve bounded opaque text");
    assert!(matches!(
        formula.compile(),
        Err(QueryError::InvalidInput {
            field: "formula",
            ..
        })
    ));
}

#[test]
fn formula_rejects_division_by_zero_nonfinite_results_and_small_budgets() {
    let division = Formula::new("score / 0")
        .expect("bounded formula should construct")
        .compile()
        .expect("division expression should compile");
    assert!(matches!(
        division.evaluate(1.0, MAX_FORMULA_OPERATIONS),
        Err(QueryError::InvalidInput {
            field: "formula",
            ..
        })
    ));

    let overflow = Formula::new("score * 1e308")
        .expect("bounded formula should construct")
        .compile()
        .expect("finite literal should compile");
    assert!(matches!(
        overflow.evaluate(1e308, MAX_FORMULA_OPERATIONS),
        Err(QueryError::InvalidInput {
            field: "formula",
            ..
        })
    ));

    assert!(matches!(
        overflow.evaluate(1.0, 0),
        Err(QueryError::WorkBudgetExceeded {
            budget: "formula_operations",
            ..
        })
    ));
}

#[test]
fn formula_compilation_is_operation_bounded() {
    let expression = format!("{}score", "+".repeat(MAX_FORMULA_OPERATIONS + 1));
    let formula = Formula::new(expression).expect("expression remains under the byte ceiling");
    assert!(matches!(
        formula.compile(),
        Err(QueryError::InvalidInput {
            field: "formula",
            ..
        })
    ));
}
