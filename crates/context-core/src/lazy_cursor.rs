//! Shared vocabulary for statement-local lazy candidate traversal.

/// Default number of provider expansions requested by one cursor advance.
pub const DEFAULT_LAZY_CURSOR_BATCH: usize = 32;
/// Maximum provider expansions requested by one cursor advance.
pub const MAX_LAZY_CURSOR_BATCH: usize = 256;

/// Stable terminal reason for a statement-local candidate cursor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LazyCursorTermination {
    /// The provider proved no additional candidates remain.
    Exhausted,
    /// Cooperative cancellation stopped the provider.
    Cancelled,
    /// The comparison allowance stopped the provider before the next score.
    ComparisonBudget,
    /// The expansion allowance stopped the provider before the next pop.
    ExpansionBudget,
    /// The adjacency allowance stopped the provider before the next edge.
    EdgeBudget,
    /// The retained/transient allocation allowance stopped the provider.
    MemoryBudget,
    /// The provider adapter failed or returned corrupt state.
    AdapterError,
}

impl LazyCursorTermination {
    /// Returns the bounded stable diagnostic name.
    #[must_use]
    pub const fn stable_name(self) -> &'static str {
        match self {
            Self::Exhausted => "exhausted",
            Self::Cancelled => "cancelled",
            Self::ComparisonBudget => "comparison_budget",
            Self::ExpansionBudget => "expansion_budget",
            Self::EdgeBudget => "edge_budget",
            Self::MemoryBudget => "memory_budget",
            Self::AdapterError => "adapter_error",
        }
    }

    /// Reports whether the cursor proved complete exhaustion.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Exhausted)
    }
}
