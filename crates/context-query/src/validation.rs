//! Pure semantic validation for SQL-facing query-plan constructors.

use crate::{QueryError, Result};

/// Maximum formula byte length preserved by the stable query-builder contract.
pub const MAX_FORMULA_BYTES: usize = 512;

/// Validated bounded formula text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Formula(String);

impl Formula {
    /// Creates bounded nonempty formula text without interpreting its grammar.
    ///
    /// Query constructors currently produce inspectable JSON plans; formula
    /// execution semantics are owned by the later executable query pipeline.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the text is empty or exceeds
    /// [`MAX_FORMULA_BYTES`].
    pub fn new(formula: impl Into<String>) -> Result<Self> {
        let formula = formula.into();
        if formula.is_empty() || formula.len() > MAX_FORMULA_BYTES {
            return Err(QueryError::InvalidInput {
                field: "formula",
                reason: format!("query formula must be 1..={MAX_FORMULA_BYTES} bytes"),
            });
        }
        Ok(Self(formula))
    }

    /// Returns the validated formula text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the wrapper and returns the formula text.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

/// Semantic validators shared by JSON-plan adapters.
pub struct QueryPlanValidator;

impl QueryPlanValidator {
    /// Validates the legacy positive signed query limit.
    ///
    /// This builder contract intentionally does not apply the executable
    /// [`crate::ExecutionBudget`] result ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for zero or negative limits.
    pub fn limit(limit: i64) -> Result<()> {
        if limit <= 0 {
            return Err(invalid(
                "limit",
                format!("query limit must be positive: {limit}"),
            ));
        }
        Ok(())
    }

    /// Validates positive and negative recommendation example lists.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for an empty positive list or any
    /// nonpositive point identifier.
    pub fn recommend_point_ids(positive: &[i64], negative: &[i64]) -> Result<()> {
        if positive.is_empty() {
            return Err(invalid(
                "positive",
                "recommend query requires at least one positive point id",
            ));
        }
        validate_point_ids(positive)?;
        validate_point_ids(negative)
    }

    /// Validates a nonempty discovery-context list.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for an empty list or any
    /// nonpositive point identifier.
    pub fn discover_point_ids(context: &[i64]) -> Result<()> {
        if context.is_empty() {
            return Err(invalid(
                "context",
                "discover query requires at least one context point id",
            ));
        }
        validate_point_ids(context)
    }

    /// Validates a nonempty ordered lookup list.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for an empty list or any
    /// nonpositive point identifier.
    pub fn lookup_point_ids(point_ids: &[i64]) -> Result<()> {
        if point_ids.is_empty() {
            return Err(invalid(
                "point_ids",
                "lookup query requires at least one point id",
            ));
        }
        validate_point_ids(point_ids)
    }

    /// Validates a nonempty prefetch branch list.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when no branch is present.
    pub fn prefetch_branches(branch_count: usize) -> Result<()> {
        if branch_count == 0 {
            return Err(invalid(
                "branches",
                "prefetch query requires at least one branch",
            ));
        }
        Ok(())
    }

    /// Validates a finite nonnegative branch weight.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for negative or nonfinite values.
    pub fn weight(weight: f64) -> Result<()> {
        if !weight.is_finite() || weight < 0.0 {
            return Err(invalid(
                "weight",
                format!("query branch weight must be finite and non-negative: {weight}"),
            ));
        }
        Ok(())
    }

    /// Validates optional finite score bounds and their ordering.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] for nonfinite values or when the
    /// minimum exceeds the maximum.
    pub fn score_threshold(minimum: Option<f64>, maximum: Option<f64>) -> Result<()> {
        if let Some(minimum) = minimum
            && !minimum.is_finite()
        {
            return Err(invalid(
                "min_score",
                format!("query min_score must be finite: {minimum}"),
            ));
        }
        if let Some(maximum) = maximum
            && !maximum.is_finite()
        {
            return Err(invalid(
                "max_score",
                format!("query max_score must be finite: {maximum}"),
            ));
        }
        if let (Some(minimum), Some(maximum)) = (minimum, maximum)
            && minimum > maximum
        {
            return Err(invalid(
                "score_threshold",
                "query score threshold min_score must not exceed max_score",
            ));
        }
        Ok(())
    }
}

fn validate_point_ids(point_ids: &[i64]) -> Result<()> {
    if let Some(point_id) = point_ids.iter().find(|point_id| **point_id <= 0) {
        return Err(invalid(
            "point_id",
            format!("query point id must be positive: {point_id}"),
        ));
    }
    Ok(())
}

fn invalid(field: &'static str, reason: impl Into<String>) -> QueryError {
    QueryError::InvalidInput {
        field,
        reason: reason.into(),
    }
}
