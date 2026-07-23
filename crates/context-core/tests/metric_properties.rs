//! Reproducible algebraic properties for every exact metric representation.

use context_core::{
    BitVector, DenseVector, DistanceMetric, Error, HalfVector, SparseEntry, SparseVector,
};

const PROPERTY_SEED: u64 = 0x6d65_7472_6963_7331;

#[test]
fn numeric_metric_properties_hold_for_dense_half_and_sparse() -> Result<(), Error> {
    let mut random = DeterministicValues::new(PROPERTY_SEED);

    for dimensions in 1..=12 {
        for _case in 0..32 {
            let mut left_values = random.vector(dimensions);
            let mut right_values = random.vector(dimensions);
            left_values[0] = nonzero(left_values[0]);
            right_values[0] = nonzero(right_values[0]);

            let dense_left = DenseVector::new(left_values.clone())?;
            let dense_right = DenseVector::new(right_values.clone())?;
            assert_numeric_properties(
                |metric| metric.distance(&dense_left, &dense_right),
                |metric| metric.distance(&dense_right, &dense_left),
                |metric| metric.distance(&dense_left, &dense_left),
            )?;

            let half_left = HalfVector::new(left_values.clone())?;
            let half_right = HalfVector::new(right_values.clone())?;
            assert_numeric_properties(
                |metric| metric.distance_half(&half_left, &half_right),
                |metric| metric.distance_half(&half_right, &half_left),
                |metric| metric.distance_half(&half_left, &half_left),
            )?;

            let sparse_left = sparse(dimensions, &left_values)?;
            let sparse_right = sparse(dimensions, &right_values)?;
            assert_numeric_properties(
                |metric| metric.distance_sparse(&sparse_left, &sparse_right),
                |metric| metric.distance_sparse(&sparse_right, &sparse_left),
                |metric| metric.distance_sparse(&sparse_left, &sparse_left),
            )?;

            let dense_dot = DistanceMetric::InnerProduct.distance(&dense_left, &dense_right)?;
            let self_dot = DistanceMetric::InnerProduct.distance(&dense_left, &dense_left)?;
            let dense_negative =
                DistanceMetric::NegativeInnerProduct.distance(&dense_left, &dense_right)?;
            let self_negative =
                DistanceMetric::NegativeInnerProduct.distance(&dense_left, &dense_left)?;
            assert_eq!(
                dense_negative.total_cmp(&self_negative),
                self_dot.total_cmp(&dense_dot)
            );
        }
    }

    Ok(())
}

#[test]
fn bit_metric_properties_are_symmetric_nonnegative_and_finite() -> Result<(), Error> {
    let mut random = DeterministicValues::new(PROPERTY_SEED ^ 0x6269_7473);

    for dimensions in 1..=128 {
        let left = BitVector::new(random.bits(dimensions))?;
        let right = BitVector::new(random.bits(dimensions))?;

        assert_eq!(left.hamming_distance(&left)?, 0);
        assert_eq!(left.jaccard_distance(&left)?, 0.0);
        assert_eq!(
            left.hamming_distance(&right)?,
            right.hamming_distance(&left)?
        );
        assert_eq!(
            left.jaccard_distance(&right)?,
            right.jaccard_distance(&left)?
        );
        assert!(left.jaccard_distance(&right)?.is_finite());
        assert!((0.0..=1.0).contains(&left.jaccard_distance(&right)?));
    }

    Ok(())
}

#[test]
fn dense_binary_navigation_metrics_match_the_bit_vector_oracle() -> Result<(), Error> {
    for dimensions in 1..=8 {
        let combinations = 1_u16 << dimensions;
        for left_bits in 0..combinations {
            for right_bits in 0..combinations {
                let left = bits_from_mask(dimensions, left_bits);
                let right = bits_from_mask(dimensions, right_bits);
                let dense_left = DenseVector::new(bits_as_dense(&left))?;
                let dense_right = DenseVector::new(bits_as_dense(&right))?;
                let bit_left = BitVector::new(left)?;
                let bit_right = BitVector::new(right)?;

                let exact_hamming = u16::try_from(bit_left.hamming_distance(&bit_right)?)
                    .map_err(|error| Error::InvalidVector(error.to_string()))?;
                assert_eq!(
                    DistanceMetric::Hamming.distance(&dense_left, &dense_right)?,
                    f32::from(exact_hamming)
                );
                let navigation =
                    f64::from(DistanceMetric::Jaccard.distance(&dense_left, &dense_right)?);
                let exact = bit_left.jaccard_distance(&bit_right)?;
                assert!((navigation - exact).abs() <= f64::from(f32::EPSILON));
            }
        }
    }

    Ok(())
}

#[test]
fn l2_navigation_does_not_preserve_jaccard_ordering() -> Result<(), Error> {
    let query = DenseVector::new(vec![1.0, 0.0, 0.0])?;
    let empty = DenseVector::new(vec![0.0, 0.0, 0.0])?;
    let overlapping = DenseVector::new(vec![1.0, 1.0, 1.0])?;

    assert!(
        DistanceMetric::L2.distance(&query, &empty)?
            < DistanceMetric::L2.distance(&query, &overlapping)?
    );
    assert!(
        DistanceMetric::Jaccard.distance(&query, &overlapping)?
            < DistanceMetric::Jaccard.distance(&query, &empty)?
    );

    Ok(())
}

#[test]
fn binary_navigation_metrics_reject_non_binary_coordinates() -> Result<(), Error> {
    let binary = DenseVector::new(vec![0.0, 1.0])?;
    let non_binary = DenseVector::new(vec![0.0, 0.5])?;

    for metric in [DistanceMetric::Hamming, DistanceMetric::Jaccard] {
        assert!(matches!(
            metric.distance(&binary, &non_binary),
            Err(Error::InvalidVector(_))
        ));
    }

    Ok(())
}

#[test]
fn every_metric_representation_rejects_dimension_mismatch() -> Result<(), Error> {
    for dimensions in 1..=32 {
        let dense_left = DenseVector::new(vec![1.0; dimensions])?;
        let dense_right = DenseVector::new(vec![1.0; dimensions + 1])?;
        let half_left = HalfVector::new(vec![1.0; dimensions])?;
        let half_right = HalfVector::new(vec![1.0; dimensions + 1])?;
        let sparse_left = sparse(dimensions, &vec![1.0; dimensions])?;
        let sparse_right = sparse(dimensions + 1, &vec![1.0; dimensions + 1])?;
        let bit_left = BitVector::new(vec![false; dimensions])?;
        let bit_right = BitVector::new(vec![false; dimensions + 1])?;

        for metric in [
            DistanceMetric::L2,
            DistanceMetric::InnerProduct,
            DistanceMetric::NegativeInnerProduct,
            DistanceMetric::Cosine,
            DistanceMetric::L1,
        ] {
            assert!(matches!(
                metric.distance(&dense_left, &dense_right),
                Err(Error::DimensionMismatch { .. })
            ));
            assert!(matches!(
                metric.distance_half(&half_left, &half_right),
                Err(Error::DimensionMismatch { .. })
            ));
            assert!(matches!(
                metric.distance_sparse(&sparse_left, &sparse_right),
                Err(Error::DimensionMismatch { .. })
            ));
        }
        assert!(matches!(
            bit_left.hamming_distance(&bit_right),
            Err(Error::DimensionMismatch { .. })
        ));
        assert!(matches!(
            bit_left.jaccard_distance(&bit_right),
            Err(Error::DimensionMismatch { .. })
        ));
    }

    Ok(())
}

fn assert_numeric_properties(
    forward: impl Fn(DistanceMetric) -> Result<f32, Error>,
    reverse: impl Fn(DistanceMetric) -> Result<f32, Error>,
    identity: impl Fn(DistanceMetric) -> Result<f32, Error>,
) -> Result<(), Error> {
    for metric in [
        DistanceMetric::L2,
        DistanceMetric::InnerProduct,
        DistanceMetric::NegativeInnerProduct,
        DistanceMetric::Cosine,
        DistanceMetric::L1,
    ] {
        let score = forward(metric)?;
        assert!(score.is_finite(), "non-finite {metric:?} score");
        assert_eq!(score, reverse(metric)?, "asymmetric {metric:?} score");
    }
    for metric in [
        DistanceMetric::L2,
        DistanceMetric::Cosine,
        DistanceMetric::L1,
    ] {
        let score = forward(metric)?;
        assert!(score >= -f32::EPSILON, "negative {metric:?} distance");
        assert!(
            identity(metric)?.abs() <= f32::EPSILON,
            "{metric:?} identity"
        );
    }
    Ok(())
}

fn sparse(dimensions: usize, values: &[f32]) -> Result<SparseVector, Error> {
    let entries = values
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, value)| *value != 0.0)
        .map(|(index, value)| SparseEntry::new(index + 1, value))
        .collect::<Result<Vec<_>, _>>()?;
    SparseVector::new(dimensions, entries)
}

fn nonzero(value: f32) -> f32 {
    if value == 0.0 { 1.0 } else { value }
}

fn bits_from_mask(dimensions: usize, mask: u16) -> Vec<bool> {
    (0..dimensions)
        .map(|index| mask & (1_u16 << index) != 0)
        .collect()
}

fn bits_as_dense(bits: &[bool]) -> Vec<f32> {
    bits.iter().map(|bit| u8::from(*bit).into()).collect()
}

struct DeterministicValues(u64);

impl DeterministicValues {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0
    }

    fn vector(&mut self, dimensions: usize) -> Vec<f32> {
        (0..dimensions)
            .map(|_| {
                let bytes = self.next().to_le_bytes();
                let value = i16::from_le_bytes([bytes[0], bytes[1]]) % 257;
                f32::from(value) / 16.0
            })
            .collect()
    }

    fn bits(&mut self, dimensions: usize) -> Vec<bool> {
        (0..dimensions).map(|_| self.next() & 1 == 1).collect()
    }
}
