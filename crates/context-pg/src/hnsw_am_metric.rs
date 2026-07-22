impl HnswScoreMetric {
    /// Returns the dense graph score preserving this metric's ordering.
    ///
    /// Bit metrics operate over the validated dense 0/1 storage form so their
    /// graph traversal score has the same ordering as the SQL operator.
    const fn navigation_metric(self) -> DistanceMetric {
        match self {
            Self::L2 => DistanceMetric::L2,
            Self::NegativeInnerProduct => DistanceMetric::NegativeInnerProduct,
            Self::Cosine => DistanceMetric::NegativeInnerProduct,
            Self::L1 => DistanceMetric::L1,
            Self::BitHamming => DistanceMetric::Hamming,
            Self::BitJaccard => DistanceMetric::Jaccard,
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "normalization is accumulated in f64 to avoid overflow, then stored in the f32 vector format"
    )]
    fn prepare_vector(
        self,
        vector: DenseVector,
    ) -> Result<Option<DenseVector>, context_core::Error> {
        if self != Self::Cosine {
            return Ok(Some(vector));
        }
        let mut values = vector.into_values();
        let norm_squared = values
            .iter()
            .map(|value| f64::from(*value) * f64::from(*value))
            .sum::<f64>();
        if norm_squared == 0.0 {
            // Match pgvector's cosine opclass: zero vectors have no defined
            // cosine distance, so they are deliberately absent from the
            // index instead of making CREATE INDEX or INSERT fail.
            return Ok(None);
        }
        if !norm_squared.is_finite() {
            return Err(context_core::Error::InvalidVector(
                "cosine HNSW vectors must have a finite nonzero norm".to_owned(),
            ));
        }
        let inverse_norm = norm_squared.sqrt().recip();
        values
            .iter_mut()
            .for_each(|value| *value = (f64::from(*value) * inverse_norm) as f32);
        DenseVector::new(values).map(Some)
    }

    const fn output_score(self, navigation_score: f32) -> f32 {
        match self {
            Self::Cosine => navigation_score + 1.0,
            _ => navigation_score,
        }
    }

    const fn storage_tag(self) -> u16 {
        match self {
            Self::L2 => 1,
            Self::NegativeInnerProduct => 2,
            Self::Cosine => 3,
            Self::L1 => 4,
            Self::BitHamming => 5,
            Self::BitJaccard => 6,
        }
    }
}
