//! Model-certified Matryoshka prefix contracts.
//!
//! A Matryoshka model promises that the leading coordinates of an embedding are
//! themselves a usable embedding at a lower dimension. pgContext uses that only
//! to make candidate generation cheaper: the stored value is never truncated or
//! rewritten, and final ranking always uses the full authoritative dimensions.
//!
//! Truncation is metric-meaningful only for real-valued coordinates under a
//! metric that is defined coordinate-wise, so a policy is accepted only for
//! dense or half representations under L2, inner product, or cosine. Packed
//! binary, sparse, and affine integer representations are rejected: their
//! coordinates are not independently interpretable at an arbitrary cut point.

use crate::{
    DistanceMetric, Error, Result, VectorNormalization, VectorRepresentation,
    policy::MAX_VECTOR_DIMENSIONS,
};

/// Maximum declared prefixes in one Matryoshka policy.
pub const MAX_MATRYOSHKA_PREFIXES: usize = 8;

/// Validated prefix dimension of a Matryoshka embedding.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct PrefixDimensions(usize);

impl PrefixDimensions {
    /// Creates a validated prefix dimension.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidVector`] when the prefix is zero or exceeds
    /// [`MAX_VECTOR_DIMENSIONS`].
    pub fn new(dimensions: usize) -> Result<Self> {
        if !(1..=MAX_VECTOR_DIMENSIONS).contains(&dimensions) {
            return Err(Error::InvalidVector(format!(
                "Matryoshka prefix dimensions must be between 1 and {MAX_VECTOR_DIMENSIONS}: {dimensions}"
            )));
        }
        Ok(Self(dimensions))
    }

    /// Returns the prefix dimension.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// Immutable Matryoshka prefix declaration attached to an embedding profile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatryoshkaPolicy {
    full_dimensions: usize,
    prefixes: Vec<PrefixDimensions>,
    normalization: VectorNormalization,
}

impl MatryoshkaPolicy {
    /// Creates a validated Matryoshka policy.
    ///
    /// `prefixes` must be strictly ascending, free of duplicates, each strictly
    /// below `full_dimensions`, and at most [`MAX_MATRYOSHKA_PREFIXES`] long.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidVector`] when the prefix list is empty, too
    /// long, not strictly ascending, or reaches `full_dimensions`.
    pub fn new(
        full_dimensions: usize,
        prefixes: Vec<PrefixDimensions>,
        normalization: VectorNormalization,
    ) -> Result<Self> {
        if !(1..=MAX_VECTOR_DIMENSIONS).contains(&full_dimensions) {
            return Err(Error::InvalidVector(format!(
                "Matryoshka full dimensions must be between 1 and {MAX_VECTOR_DIMENSIONS}: {full_dimensions}"
            )));
        }
        if prefixes.is_empty() || prefixes.len() > MAX_MATRYOSHKA_PREFIXES {
            return Err(Error::InvalidVector(format!(
                "Matryoshka policy must declare 1..={MAX_MATRYOSHKA_PREFIXES} prefixes"
            )));
        }
        let mut previous = 0;
        for prefix in &prefixes {
            if prefix.get() <= previous {
                return Err(Error::InvalidVector(
                    "Matryoshka prefixes must be strictly ascending without duplicates".to_owned(),
                ));
            }
            if prefix.get() >= full_dimensions {
                return Err(Error::InvalidVector(format!(
                    "Matryoshka prefix {} must be strictly below the full dimension {full_dimensions}",
                    prefix.get()
                )));
            }
            previous = prefix.get();
        }
        Ok(Self {
            full_dimensions,
            prefixes,
            normalization,
        })
    }

    /// Validates that a representation and metric can carry a prefix policy.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidVector`] for a representation or metric whose
    /// coordinates are not independently interpretable at a cut point.
    pub fn require_eligible(
        representation: VectorRepresentation,
        metric: DistanceMetric,
    ) -> Result<()> {
        if !matches!(
            representation,
            VectorRepresentation::Dense | VectorRepresentation::Half
        ) {
            return Err(Error::InvalidVector(
                "Matryoshka prefixes require a dense or half representation".to_owned(),
            ));
        }
        if !matches!(
            metric,
            DistanceMetric::L2 | DistanceMetric::InnerProduct | DistanceMetric::Cosine
        ) {
            return Err(Error::InvalidVector(
                "Matryoshka prefixes require the l2, inner_product, or cosine metric".to_owned(),
            ));
        }
        Ok(())
    }

    /// Returns the full authoritative dimension.
    #[must_use]
    pub const fn full_dimensions(&self) -> usize {
        self.full_dimensions
    }

    /// Returns the declared prefixes in ascending order.
    #[must_use]
    pub fn prefixes(&self) -> &[PrefixDimensions] {
        &self.prefixes
    }

    /// Returns the normalization the provider promises for a prefix.
    #[must_use]
    pub const fn normalization(&self) -> VectorNormalization {
        self.normalization
    }

    /// Reports whether a prefix dimension is declared by this policy.
    #[must_use]
    pub fn declares(&self, prefix: PrefixDimensions) -> bool {
        self.prefixes.contains(&prefix)
    }

    /// Returns the smallest declared prefix strictly greater than `prefix`.
    #[must_use]
    pub fn next_prefix_after(&self, prefix: PrefixDimensions) -> Option<PrefixDimensions> {
        self.prefixes
            .iter()
            .copied()
            .find(|candidate| candidate.get() > prefix.get())
    }

    /// Returns a bounded read-only view of a vector's leading coordinates.
    ///
    /// The authoritative value is never copied or truncated in place.
    ///
    /// # Errors
    ///
    /// Returns [`Error::DimensionMismatch`] when `coordinates` does not carry
    /// the full declared dimension, and [`Error::InvalidVector`] when the
    /// prefix is not declared by this policy.
    pub fn prefix_view<'a>(
        &self,
        coordinates: &'a [f32],
        prefix: PrefixDimensions,
    ) -> Result<&'a [f32]> {
        if coordinates.len() != self.full_dimensions {
            return Err(Error::DimensionMismatch {
                left: coordinates.len(),
                right: self.full_dimensions,
            });
        }
        if !self.declares(prefix) {
            return Err(Error::InvalidVector(format!(
                "Matryoshka prefix {} is not declared by this profile",
                prefix.get()
            )));
        }
        coordinates.get(..prefix.get()).ok_or_else(|| {
            Error::InvalidVector("Matryoshka prefix exceeds the supplied coordinates".to_owned())
        })
    }

    /// Reports whether renormalizing only the query prefix preserves rank order.
    ///
    /// pgContext renormalizes the query prefix but scores it against a plain
    /// truncation of the stored vector, because rewriting stored values is not
    /// on the table. That asymmetry is only safe where a positive rescale of
    /// one side cannot reorder results:
    ///
    /// - `cosine` normalizes both sides itself, so the rescale is inert;
    /// - `inner_product` is linear in the query, so a positive scale applies a
    ///   uniform positive factor to every candidate score;
    /// - `l2` is **not** safe. `|q - s|^2 = |q|^2 - 2 q.s + |s|^2` reweights the
    ///   dot-product term against a per-row `|s|^2`, so scaling one side alone
    ///   yields an order that is neither "truncate both" nor "renormalize
    ///   both".
    ///
    /// Under `l2` the prefix is therefore truncated plainly on both sides,
    /// which is the symmetric comparison the model certifies.
    #[must_use]
    pub const fn renormalizes_query_prefix(metric: DistanceMetric) -> bool {
        matches!(
            metric,
            DistanceMetric::Cosine | DistanceMetric::InnerProduct
        )
    }

    /// Returns an owned, query-side prefix ready for prefix scoring.
    ///
    /// When the profile promises unit-L2 coordinates *and* the metric makes a
    /// one-sided rescale order-preserving, the prefix is renormalized, because
    /// truncating a unit vector does not preserve its norm. This allocates a
    /// **query** vector only; no stored value is ever rewritten.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`MatryoshkaPolicy::prefix_view`], and
    /// [`Error::InvalidVector`] when a renormalized prefix has zero norm and
    /// therefore has no direction to preserve.
    pub fn query_prefix(
        &self,
        coordinates: &[f32],
        prefix: PrefixDimensions,
        metric: DistanceMetric,
    ) -> Result<Vec<f32>> {
        let view = self.prefix_view(coordinates, prefix)?;
        if self.normalization == VectorNormalization::None
            || !Self::renormalizes_query_prefix(metric)
        {
            return Ok(view.to_vec());
        }
        let norm = view
            .iter()
            .map(|coordinate| f64::from(*coordinate) * f64::from(*coordinate))
            .sum::<f64>()
            .sqrt();
        if !norm.is_finite() || norm <= 0.0 {
            return Err(Error::InvalidVector(
                "unit-L2 Matryoshka prefix has zero norm and cannot be renormalized".to_owned(),
            ));
        }
        Ok(view
            .iter()
            .map(|coordinate| {
                let scaled = f64::from(*coordinate) / norm;
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "prefix renormalization returns f32 query coordinates by contract"
                )]
                let narrowed = scaled as f32;
                narrowed
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;

    fn prefixes(values: &[usize]) -> Vec<PrefixDimensions> {
        values
            .iter()
            .map(|value| PrefixDimensions::new(*value).expect("bounded prefix"))
            .collect()
    }

    #[test]
    fn prefix_dimensions_enforce_policy_bounds() {
        assert!(PrefixDimensions::new(0).is_err());
        assert!(PrefixDimensions::new(1).is_ok());
        assert!(PrefixDimensions::new(MAX_VECTOR_DIMENSIONS).is_ok());
        assert!(PrefixDimensions::new(MAX_VECTOR_DIMENSIONS + 1).is_err());
    }

    #[test]
    fn policies_require_strictly_ascending_prefixes_below_the_full_dimension() {
        assert!(
            MatryoshkaPolicy::new(768, prefixes(&[128, 256, 512]), VectorNormalization::UnitL2)
                .is_ok()
        );
        assert!(
            MatryoshkaPolicy::new(768, Vec::new(), VectorNormalization::UnitL2).is_err(),
            "an empty prefix list is not a policy"
        );
        assert!(
            MatryoshkaPolicy::new(768, prefixes(&[256, 128]), VectorNormalization::None).is_err(),
            "descending prefixes must be rejected"
        );
        assert!(
            MatryoshkaPolicy::new(768, prefixes(&[128, 128]), VectorNormalization::None).is_err(),
            "duplicate prefixes must be rejected"
        );
        assert!(
            MatryoshkaPolicy::new(768, prefixes(&[768]), VectorNormalization::None).is_err(),
            "a prefix equal to the full dimension is not a prefix"
        );
        assert!(
            MatryoshkaPolicy::new(768, prefixes(&[1024]), VectorNormalization::None).is_err(),
            "a prefix above the full dimension must be rejected"
        );
        let too_many = (1..=MAX_MATRYOSHKA_PREFIXES + 1)
            .map(|index| index * 8)
            .collect::<Vec<_>>();
        assert!(
            MatryoshkaPolicy::new(4096, prefixes(&too_many), VectorNormalization::None).is_err()
        );
    }

    #[test]
    fn only_coordinate_wise_representations_and_metrics_are_eligible() {
        assert!(
            MatryoshkaPolicy::require_eligible(VectorRepresentation::Dense, DistanceMetric::Cosine)
                .is_ok()
        );
        assert!(
            MatryoshkaPolicy::require_eligible(VectorRepresentation::Half, DistanceMetric::L2)
                .is_ok()
        );
        for representation in [
            VectorRepresentation::Bit,
            VectorRepresentation::Sparse,
            VectorRepresentation::Int8,
            VectorRepresentation::UInt8,
        ] {
            assert!(
                MatryoshkaPolicy::require_eligible(representation, DistanceMetric::L2).is_err(),
                "{representation:?} must be ineligible"
            );
        }
        for metric in [
            DistanceMetric::Hamming,
            DistanceMetric::Jaccard,
            DistanceMetric::L1,
        ] {
            assert!(
                MatryoshkaPolicy::require_eligible(VectorRepresentation::Dense, metric).is_err(),
                "{metric:?} must be ineligible"
            );
        }
    }

    #[test]
    fn prefix_views_are_bounded_and_reject_undeclared_cuts() {
        let policy =
            MatryoshkaPolicy::new(4, prefixes(&[2]), VectorNormalization::None).expect("policy");
        let coordinates = [1.0_f32, 2.0, 3.0, 4.0];
        assert_eq!(
            policy
                .prefix_view(&coordinates, PrefixDimensions::new(2).expect("prefix"))
                .expect("view"),
            &[1.0, 2.0]
        );
        assert!(
            policy
                .prefix_view(&coordinates, PrefixDimensions::new(3).expect("prefix"))
                .is_err(),
            "an undeclared prefix must be rejected"
        );
        assert!(
            policy
                .prefix_view(&[1.0, 2.0], PrefixDimensions::new(2).expect("prefix"))
                .is_err(),
            "a short vector must be rejected"
        );
    }

    #[test]
    fn unit_l2_prefixes_are_renormalized_and_plain_prefixes_are_not() {
        let prefix = PrefixDimensions::new(2).expect("prefix");
        let coordinates = [3.0_f32, 4.0, 0.0, 0.0];

        let plain = MatryoshkaPolicy::new(4, prefixes(&[2]), VectorNormalization::None)
            .expect("policy")
            .query_prefix(&coordinates, prefix, DistanceMetric::Cosine)
            .expect("prefix");
        assert_eq!(plain, vec![3.0, 4.0]);

        let unit = MatryoshkaPolicy::new(4, prefixes(&[2]), VectorNormalization::UnitL2)
            .expect("policy")
            .query_prefix(&coordinates, prefix, DistanceMetric::Cosine)
            .expect("prefix");
        let norm = unit
            .iter()
            .map(|value| f64::from(*value) * f64::from(*value))
            .sum::<f64>()
            .sqrt();
        assert!((norm - 1.0).abs() < 1e-6, "prefix must be renormalized");

        assert!(
            MatryoshkaPolicy::new(4, prefixes(&[2]), VectorNormalization::UnitL2)
                .expect("policy")
                .query_prefix(&[0.0, 0.0, 1.0, 0.0], prefix, DistanceMetric::Cosine)
                .is_err(),
            "a zero-norm unit prefix has no direction to preserve"
        );
    }

    #[test]
    fn l2_prefixes_are_never_renormalized_on_one_side_only() {
        // Renormalizing only the query prefix reweights the dot-product term
        // against a per-row |s|^2 under l2, so the resulting order is neither
        // "truncate both" nor "renormalize both". Truncate plainly instead.
        assert!(!MatryoshkaPolicy::renormalizes_query_prefix(
            DistanceMetric::L2
        ));
        assert!(MatryoshkaPolicy::renormalizes_query_prefix(
            DistanceMetric::Cosine
        ));
        assert!(MatryoshkaPolicy::renormalizes_query_prefix(
            DistanceMetric::InnerProduct
        ));

        let prefix = PrefixDimensions::new(2).expect("prefix");
        let coordinates = [3.0_f32, 4.0, 0.0, 0.0];
        let policy =
            MatryoshkaPolicy::new(4, prefixes(&[2]), VectorNormalization::UnitL2).expect("policy");
        assert_eq!(
            policy
                .query_prefix(&coordinates, prefix, DistanceMetric::L2)
                .expect("prefix"),
            vec![3.0, 4.0],
            "an l2 prefix must be truncated plainly on both sides"
        );
    }

    #[test]
    fn widening_walks_declared_prefixes_in_order() {
        let policy =
            MatryoshkaPolicy::new(768, prefixes(&[128, 256, 512]), VectorNormalization::None)
                .expect("policy");
        let first = PrefixDimensions::new(128).expect("prefix");
        let second = policy.next_prefix_after(first).expect("next prefix");
        assert_eq!(second.get(), 256);
        let third = policy.next_prefix_after(second).expect("next prefix");
        assert_eq!(third.get(), 512);
        assert_eq!(policy.next_prefix_after(third), None);
    }
}
