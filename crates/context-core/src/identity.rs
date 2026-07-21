//! Logical identities owned by PostgreSQL-authoritative domain state.

/// Stable logical identifier for a collection point.
///
/// A `PointId` is not a heap TID, HNSW node identifier, artifact record
/// offset, or encoded physical address. Infrastructure adapters must translate
/// those identities explicitly at their boundaries.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PointId(u64);

impl PointId {
    /// Creates a logical point identifier.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the underlying catalog identifier.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Converts a signed SQL/catalog identifier without conflating negatives
    /// with the unsigned logical domain.
    #[must_use]
    pub fn from_i64(value: i64) -> Option<Self> {
        u64::try_from(value).ok().map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::PointId;

    #[test]
    fn point_id_round_trips_its_logical_value() {
        assert_eq!(PointId::new(42).get(), 42);
        assert_eq!(PointId::from_i64(42), Some(PointId::new(42)));
    }

    #[test]
    fn point_id_rejects_negative_sql_identifiers() {
        assert_eq!(PointId::from_i64(-1), None);
    }
}
