//! Property coverage for vector text round trips.

use core::fmt;
use std::collections::BTreeMap;

use context_core::{BitVector, DenseVector, HalfVector, SparseEntry, SparseVector};
use proptest::prelude::*;
use proptest::test_runner::{FileFailurePersistence, TestCaseError};

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(FileFailurePersistence::Off)),
        .. ProptestConfig::default()
    })]

    #[test]
    fn dense_vector_text_round_trips(values in finite_values(1_usize..64, -10_000.0, 10_000.0)) {
        let vector = must(DenseVector::new(values))?;
        let reparsed = must(vector.to_string().parse::<DenseVector>())?;

        prop_assert_eq!(reparsed, vector);
    }

    #[test]
    fn half_vector_text_round_trips(values in finite_values(1_usize..64, -65_504.0, 65_504.0)) {
        let vector = must(HalfVector::new(values))?;
        let reparsed = must(vector.to_string().parse::<HalfVector>())?;

        prop_assert_eq!(reparsed, vector);
    }

    #[test]
    fn sparse_vector_text_round_trips(
        dimensions in 1_usize..256,
        generated_entries in prop::collection::vec((1_usize..256, -10_000.0_f32..10_000.0), 0..64),
    ) {
        let entries = canonical_entries(dimensions, generated_entries)?;
        let vector = must(SparseVector::new(dimensions, entries))?;
        let reparsed = must(vector.to_string().parse::<SparseVector>())?;

        prop_assert_eq!(reparsed, vector);
    }

    #[test]
    fn bit_vector_text_round_trips(bits in prop::collection::vec(any::<bool>(), 1..256)) {
        let vector = must(BitVector::new(bits))?;
        let reparsed = must(vector.to_string().parse::<BitVector>())?;

        prop_assert_eq!(reparsed, vector);
    }
}

fn finite_values(
    len: impl Strategy<Value = usize>,
    min: f32,
    max: f32,
) -> impl Strategy<Value = Vec<f32>> {
    len.prop_flat_map(move |len| prop::collection::vec(min..max, len))
}

fn canonical_entries(
    dimensions: usize,
    generated_entries: Vec<(usize, f32)>,
) -> Result<Vec<SparseEntry>, TestCaseError> {
    generated_entries
        .into_iter()
        .filter(|(index, _)| *index <= dimensions)
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .map(|(index, value)| must(SparseEntry::new(index, value)))
        .collect()
}

fn must<T, E: fmt::Display>(result: Result<T, E>) -> Result<T, TestCaseError> {
    result.map_err(|error| TestCaseError::fail(error.to_string()))
}
