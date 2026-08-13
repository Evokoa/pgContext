//! Query-owned controls and diagnostics for statement-local candidate cursors.

use crate::{CandidatePage, QueryError, Result};

pub use context_core::{DEFAULT_LAZY_CURSOR_BATCH, LazyCursorTermination, MAX_LAZY_CURSOR_BATCH};

/// Validated request to advance a lazy candidate provider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LazyCursorAdvance {
    batch_size: usize,
}

/// Internal opt-in controlling whether a candidate source uses its lazy path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LazyCursorControl {
    /// Preserve the source's eager compatibility path.
    Disabled,
    /// Use statement-local cursor state with the validated advance request.
    Experimental(LazyCursorAdvance),
}

impl LazyCursorControl {
    /// Returns the default disabled control.
    #[must_use]
    pub const fn disabled() -> Self {
        Self::Disabled
    }

    /// Creates an internal experimental opt-in.
    ///
    /// # Errors
    ///
    /// Returns the same batch validation failure as [`LazyCursorAdvance::new`].
    pub fn experimental(batch_size: usize) -> Result<Self> {
        LazyCursorAdvance::new(batch_size).map(Self::Experimental)
    }

    /// Returns the advance request only for the experimental path.
    #[must_use]
    pub const fn advance(self) -> Option<LazyCursorAdvance> {
        match self {
            Self::Disabled => None,
            Self::Experimental(advance) => Some(advance),
        }
    }
}

impl LazyCursorAdvance {
    /// Creates a positive bounded cursor-advance request.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when `batch_size` is zero or above
    /// [`MAX_LAZY_CURSOR_BATCH`].
    pub fn new(batch_size: usize) -> Result<Self> {
        if !(1..=MAX_LAZY_CURSOR_BATCH).contains(&batch_size) {
            return Err(QueryError::InvalidInput {
                field: "lazy_cursor_batch",
                reason: format!("must be between 1 and {MAX_LAZY_CURSOR_BATCH}"),
            });
        }
        Ok(Self { batch_size })
    }

    /// Returns the maximum provider expansions for this advance.
    #[must_use]
    pub const fn batch_size(self) -> usize {
        self.batch_size
    }
}

/// Content-free cumulative work reported by a lazy candidate cursor.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LazyCursorWork {
    comparisons: usize,
    expansions: usize,
    edges: usize,
    retained_bytes: usize,
}

impl LazyCursorWork {
    /// Records one complete work snapshot.
    #[must_use]
    pub const fn new(
        comparisons: usize,
        expansions: usize,
        edges: usize,
        retained_bytes: usize,
    ) -> Self {
        Self {
            comparisons,
            expansions,
            edges,
            retained_bytes,
        }
    }

    /// Adds two snapshots without wrapping platform-sized counters.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::ArithmeticOverflow`] if any field overflows.
    pub fn checked_add(self, other: Self) -> Result<Self> {
        Ok(Self {
            comparisons: self.comparisons.checked_add(other.comparisons).ok_or(
                QueryError::ArithmeticOverflow {
                    operation: "lazy_cursor_work",
                },
            )?,
            expansions: self.expansions.checked_add(other.expansions).ok_or(
                QueryError::ArithmeticOverflow {
                    operation: "lazy_cursor_work",
                },
            )?,
            edges: self
                .edges
                .checked_add(other.edges)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "lazy_cursor_work",
                })?,
            retained_bytes: self
                .retained_bytes
                .checked_add(other.retained_bytes)
                .ok_or(QueryError::ArithmeticOverflow {
                    operation: "lazy_cursor_work",
                })?,
        })
    }

    /// Returns score comparisons performed.
    #[must_use]
    pub const fn comparisons(self) -> usize {
        self.comparisons
    }

    /// Returns provider states expanded.
    #[must_use]
    pub const fn expansions(self) -> usize {
        self.expansions
    }

    /// Returns adjacency entries examined.
    #[must_use]
    pub const fn edges(self) -> usize {
        self.edges
    }

    /// Returns extension-owned bytes retained by the cursor.
    #[must_use]
    pub const fn retained_bytes(self) -> usize {
        self.retained_bytes
    }
}

/// One bounded cursor page with honest completion evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct LazyCursorPage {
    page: CandidatePage,
    work: LazyCursorWork,
    termination: Option<LazyCursorTermination>,
}

impl LazyCursorPage {
    /// Creates a cursor page after validating its completion claim.
    ///
    /// # Errors
    ///
    /// Returns [`QueryError::InvalidInput`] when the page's `exhausted` flag
    /// disagrees with the typed terminal reason.
    pub fn new(
        page: CandidatePage,
        work: LazyCursorWork,
        termination: Option<LazyCursorTermination>,
    ) -> Result<Self> {
        let proved_exhausted = termination.is_some_and(LazyCursorTermination::is_complete);
        if page.exhausted() != proved_exhausted {
            return Err(QueryError::InvalidInput {
                field: "lazy_cursor_completion",
                reason: "candidate page exhaustion must match typed termination".to_owned(),
            });
        }
        Ok(Self {
            page,
            work,
            termination,
        })
    }

    /// Returns the candidate-source page.
    #[must_use]
    pub const fn page(&self) -> &CandidatePage {
        &self.page
    }

    /// Consumes this wrapper and returns the candidate-source page.
    #[must_use]
    pub fn into_page(self) -> CandidatePage {
        self.page
    }

    /// Returns cumulative content-free cursor work.
    #[must_use]
    pub const fn work(&self) -> LazyCursorWork {
        self.work
    }

    /// Returns the typed terminal reason, if any.
    #[must_use]
    pub const fn termination(&self) -> Option<LazyCursorTermination> {
        self.termination
    }
}
