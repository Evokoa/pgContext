//! Stable context error taxonomy tests.

use context_core::{ContextError, Error};

#[test]
fn existing_core_errors_project_to_stable_categories() {
    let invalid = Error::InvalidVector("not a vector".to_string());
    assert_eq!(invalid.context_error(), ContextError::InvalidVector);

    let mismatch = Error::DimensionMismatch { left: 2, right: 3 };
    assert_eq!(mismatch.context_error(), ContextError::DimensionMismatch);

    let invalid_limit = Error::InvalidSearchLimit(0);
    assert_eq!(invalid_limit.context_error(), ContextError::InvalidFilter);
}
