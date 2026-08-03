//! Canonical retrieval identities, ordering, and lifecycle vocabulary.

use core::cmp::Ordering;

/// Ordering direction for a metric or scored retrieval branch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScoreOrder {
    /// Smaller distance values rank first.
    LowerIsBetter,
    /// Larger similarity or fusion scores rank first.
    HigherIsBetter,
}

impl ScoreOrder {
    /// Returns whether `candidate` strictly outranks `existing`.
    #[must_use]
    pub fn is_better(self, candidate: f64, existing: f64) -> bool {
        self.compare(candidate, existing).is_lt()
    }

    /// Compares two finite scores in result order.
    #[must_use]
    pub fn compare(self, left: f64, right: f64) -> Ordering {
        match self {
            Self::LowerIsBetter => left.total_cmp(&right),
            Self::HigherIsBetter => right.total_cmp(&left),
        }
    }
}

/// Canonical retrieval index family.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum IndexKind {
    /// Authoritative exact scan with no derived index requirement.
    Exact,
    /// Hierarchical navigable small-world graph.
    Hnsw,
    /// Inverted-file flat vector index.
    IvfFlat,
}

/// Authority represented by a retrieval value or artifact.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SourceAuthority {
    /// An ordinary visible PostgreSQL row.
    PostgreSqlRow,
    /// A provider-native value stored in a visible PostgreSQL row.
    ProviderNative,
    /// Rebuildable data derived from an authoritative source row.
    DerivedArtifact,
}

macro_rules! nonzero_id {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u64);

        impl $name {
            /// Creates the identity, rejecting the reserved zero value.
            #[must_use]
            pub const fn new(value: u64) -> Option<Self> {
                if value == 0 { None } else { Some(Self(value)) }
            }

            /// Returns the non-zero numeric identity.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}

nonzero_id!(
    GenerationId,
    "Published or staged artifact generation identity."
);
nonzero_id!(
    ConfigurationRevision,
    "Immutable retrieval configuration revision identity."
);
nonzero_id!(ProfileId, "Immutable vector or model profile identity.");
nonzero_id!(
    OccurrenceId,
    "Stable occurrence identity for one candidate-producing source record."
);
nonzero_id!(
    SourceVersion,
    "Visible authoritative source version used to validate a candidate."
);

/// Bounded source-readiness reason safe for diagnostics and telemetry.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReadinessReason {
    /// Adapter has not established readiness yet.
    Uninitialized,
    /// No active generation or index exists.
    GenerationMissing,
    /// Configuration changed after the active generation was built.
    ConfigurationChanged,
    /// Source metadata or artifact generation is stale.
    StaleGeneration,
    /// Selected source kind cannot serve this query shape.
    UnsupportedQuery,
    /// Source failed validation and requires repair or rebuild.
    ValidationFailed,
    /// The artifact was built for a different authorization scope.
    PermissionScopeMismatch,
}

/// Terminal bounded-execution classification.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Completion {
    /// Execution completed normally.
    Complete,
    /// Cooperative cancellation stopped execution at a port boundary.
    Cancelled,
    /// A work or result budget prevented authoritative completion.
    BudgetExhausted,
    /// Execution returned a visible fallback or partial strategy result.
    Degraded,
}
