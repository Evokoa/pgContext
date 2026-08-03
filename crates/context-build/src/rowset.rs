//! Ordered, prevalidated rowsets for bounded set-based PostgreSQL adapters.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{Display, Formatter};

/// One input row with stable zero-based input ordinality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedRow<K, V> {
    ordinal: usize,
    key: K,
    value: V,
}

impl<K, V> OrderedRow<K, V> {
    /// Returns the zero-based input position.
    #[must_use]
    pub const fn ordinal(&self) -> usize {
        self.ordinal
    }

    /// Returns the typed source key.
    #[must_use]
    pub const fn key(&self) -> &K {
        &self.key
    }

    /// Returns the typed mutation or projection value.
    #[must_use]
    pub const fn value(&self) -> &V {
        &self.value
    }
}

/// Duplicate-free input rows whose order is independent of database ordering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedRowset<K, V> {
    rows: Vec<OrderedRow<K, V>>,
    chunk_limit: usize,
}

impl<K: Ord, V> OrderedRowset<K, V> {
    /// Validates a bounded rowset before an adapter performs any mutation.
    ///
    /// # Errors
    ///
    /// Returns [`RowsetError::EmptyChunkLimit`], [`RowsetError::TooManyRows`],
    /// or [`RowsetError::DuplicateKey`] without returning a partial rowset.
    pub fn new(entries: Vec<(K, V)>, chunk_limit: usize) -> Result<Self, RowsetError> {
        if chunk_limit == 0 {
            return Err(RowsetError::EmptyChunkLimit);
        }
        if entries.len() > chunk_limit {
            return Err(RowsetError::TooManyRows {
                actual: entries.len(),
                limit: chunk_limit,
            });
        }

        let mut seen = BTreeMap::new();
        for (ordinal, (key, _)) in entries.iter().enumerate() {
            if let Some(first) = seen.insert(key, ordinal) {
                return Err(RowsetError::DuplicateKey {
                    first,
                    duplicate: ordinal,
                });
            }
        }
        let rows = entries
            .into_iter()
            .enumerate()
            .map(|(ordinal, (key, value))| OrderedRow {
                ordinal,
                key,
                value,
            })
            .collect();
        Ok(Self { rows, chunk_limit })
    }

    /// Validates every source key before a set-based mutation starts.
    ///
    /// Missing-key ordinals are returned in input order, without exposing key
    /// contents that may contain tenant or source data.
    ///
    /// # Errors
    ///
    /// Returns [`RowsetError::MissingSource`] when the predicate rejects one
    /// or more keys.
    pub fn validate_sources(
        &self,
        mut source_exists: impl FnMut(&K) -> bool,
    ) -> Result<(), RowsetError> {
        let ordinals = self
            .rows
            .iter()
            .filter(|row| !source_exists(&row.key))
            .map(|row| row.ordinal)
            .collect::<Vec<_>>();
        if ordinals.is_empty() {
            Ok(())
        } else {
            Err(RowsetError::MissingSource { ordinals })
        }
    }

    /// Returns rows in their original input order.
    #[must_use]
    pub fn rows(&self) -> &[OrderedRow<K, V>] {
        &self.rows
    }

    /// Returns bounded chunks while preserving global input ordinalities.
    pub fn chunks(
        &self,
        rows_per_statement: usize,
    ) -> Result<std::slice::Chunks<'_, OrderedRow<K, V>>, RowsetError> {
        if rows_per_statement == 0 {
            return Err(RowsetError::EmptyChunkLimit);
        }
        Ok(self.rows.chunks(rows_per_statement.min(self.chunk_limit)))
    }
}

/// Rejected ordered-rowset construction or validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowsetError {
    /// A zero statement/chunk bound would never make progress.
    EmptyChunkLimit,
    /// Input exceeds the declared all-or-nothing validation bound.
    TooManyRows {
        /// Actual input cardinality.
        actual: usize,
        /// Configured maximum cardinality.
        limit: usize,
    },
    /// One key appeared more than once.
    DuplicateKey {
        /// First zero-based occurrence.
        first: usize,
        /// Repeated zero-based occurrence.
        duplicate: usize,
    },
    /// Source validation found missing keys at these input positions.
    MissingSource {
        /// Stable, zero-based input positions.
        ordinals: Vec<usize>,
    },
}

impl Display for RowsetError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyChunkLimit => formatter.write_str("rowset chunk limit must be positive"),
            Self::TooManyRows { actual, limit } => {
                write!(
                    formatter,
                    "rowset contains {actual} rows, exceeding limit {limit}"
                )
            }
            Self::DuplicateKey { first, duplicate } => write!(
                formatter,
                "rowset key at ordinal {duplicate} duplicates ordinal {first}"
            ),
            Self::MissingSource { ordinals } => {
                write!(
                    formatter,
                    "source rows are missing at input ordinals {ordinals:?}"
                )
            }
        }
    }
}

impl Error for RowsetError {}
