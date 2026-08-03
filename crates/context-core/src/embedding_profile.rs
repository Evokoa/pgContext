//! Immutable authoritative embedding-profile contracts.

use crate::{
    DistanceMetric, Error, ProfileId, ProviderBitOrder, ProviderByteOrder, Result, SourceAuthority,
    VectorRepresentation, policy::MAX_VECTOR_DIMENSIONS,
};

/// Normalization promised by a provider profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VectorNormalization {
    /// Coordinates are stored exactly as produced.
    None,
    /// The provider promises unit-L2-normalized coordinates.
    UnitL2,
}

/// Packed-binary layout declared by a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderBinaryLayout {
    /// Bit significance inside one byte.
    pub bit_order: ProviderBitOrder,
    /// Byte significance inside one payload.
    pub byte_order: ProviderByteOrder,
}

/// Optional affine interpretation for integer provider coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IntegerScale {
    /// Positive coordinate scale.
    pub scale: f64,
    /// Provider zero point before scaling.
    pub zero_point: i32,
}

/// One immutable provider/model/source-representation contract.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingProfile {
    id: ProfileId,
    representation: VectorRepresentation,
    dimensions: usize,
    normalization: VectorNormalization,
    metric: DistanceMetric,
    provider: String,
    model: String,
    revision: String,
    input_template: String,
    output_template: String,
    binary_layout: Option<ProviderBinaryLayout>,
    integer_scale: Option<IntegerScale>,
    configuration_hash: u64,
}

impl EmbeddingProfile {
    /// Creates and validates an immutable embedding profile.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidVector`] for invalid dimensions, empty labels,
    /// incompatible representation/metric/layout tuples, invalid integer
    /// scaling, or the reserved zero configuration hash.
    #[allow(
        clippy::too_many_arguments,
        reason = "the constructor freezes the complete cross-layer profile contract in one validation boundary"
    )]
    pub fn new(
        id: ProfileId,
        representation: VectorRepresentation,
        dimensions: usize,
        normalization: VectorNormalization,
        metric: DistanceMetric,
        provider: String,
        model: String,
        revision: String,
        input_template: String,
        output_template: String,
        binary_layout: Option<ProviderBinaryLayout>,
        integer_scale: Option<IntegerScale>,
        configuration_hash: u64,
    ) -> Result<Self> {
        if !(1..=MAX_VECTOR_DIMENSIONS).contains(&dimensions) {
            return Err(Error::InvalidVector(format!(
                "embedding profile dimensions must be between 1 and {MAX_VECTOR_DIMENSIONS}: {dimensions}"
            )));
        }
        for (label, value) in [
            ("provider", provider.as_str()),
            ("model", model.as_str()),
            ("revision", revision.as_str()),
            ("input template", input_template.as_str()),
            ("output template", output_template.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(Error::InvalidVector(format!(
                    "embedding profile {label} must not be empty"
                )));
            }
        }
        if configuration_hash == 0 {
            return Err(Error::InvalidVector(
                "embedding profile configuration hash must be nonzero".to_owned(),
            ));
        }
        let binary_metric = matches!(metric, DistanceMetric::Hamming | DistanceMetric::Jaccard);
        if representation == VectorRepresentation::Bit {
            if !binary_metric || binary_layout.is_none() || integer_scale.is_some() {
                return Err(Error::InvalidVector(
                    "bit profiles require a binary metric and layout, with no integer scale"
                        .to_owned(),
                ));
            }
        } else if binary_metric || binary_layout.is_some() {
            return Err(Error::InvalidVector(
                "numeric profiles cannot declare binary metrics or layouts".to_owned(),
            ));
        }
        if let Some(scale) = integer_scale {
            if !matches!(
                representation,
                VectorRepresentation::Int8 | VectorRepresentation::UInt8
            ) || !scale.scale.is_finite()
                || scale.scale <= 0.0
            {
                return Err(Error::InvalidVector(
                    "integer scaling requires an int8/uint8 profile and a finite positive scale"
                        .to_owned(),
                ));
            }
            let valid_zero_point = match representation {
                VectorRepresentation::Int8 => (-128..=127).contains(&scale.zero_point),
                VectorRepresentation::UInt8 => (0..=255).contains(&scale.zero_point),
                _ => false,
            };
            if !valid_zero_point {
                return Err(Error::InvalidVector(format!(
                    "integer profile zero point is out of representation range: {}",
                    scale.zero_point
                )));
            }
        }

        Ok(Self {
            id,
            representation,
            dimensions,
            normalization,
            metric,
            provider,
            model,
            revision,
            input_template,
            output_template,
            binary_layout,
            integer_scale,
            configuration_hash,
        })
    }

    /// Returns whether the stored value, rather than a codec artifact, is authoritative.
    #[must_use]
    pub const fn source_authority(&self) -> SourceAuthority {
        match self.representation {
            VectorRepresentation::Int8
            | VectorRepresentation::UInt8
            | VectorRepresentation::Bit => SourceAuthority::ProviderNative,
            VectorRepresentation::Dense
            | VectorRepresentation::Half
            | VectorRepresentation::Sparse => SourceAuthority::PostgreSqlRow,
        }
    }

    /// Returns the stable profile identity.
    #[must_use]
    pub const fn id(&self) -> ProfileId {
        self.id
    }
    /// Returns the authoritative representation.
    #[must_use]
    pub const fn representation(&self) -> VectorRepresentation {
        self.representation
    }
    /// Returns the fixed dimensions or logical bits.
    #[must_use]
    pub const fn dimensions(&self) -> usize {
        self.dimensions
    }
    /// Returns the declared normalization.
    #[must_use]
    pub const fn normalization(&self) -> VectorNormalization {
        self.normalization
    }
    /// Returns the exact metric.
    #[must_use]
    pub const fn metric(&self) -> DistanceMetric {
        self.metric
    }
    /// Returns the provider label.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }
    /// Returns the model label.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }
    /// Returns the provider model revision.
    #[must_use]
    pub fn revision(&self) -> &str {
        &self.revision
    }
    /// Returns the provider input template.
    #[must_use]
    pub fn input_template(&self) -> &str {
        &self.input_template
    }
    /// Returns the provider output template.
    #[must_use]
    pub fn output_template(&self) -> &str {
        &self.output_template
    }
    /// Returns the checked packed-binary layout, when applicable.
    #[must_use]
    pub const fn binary_layout(&self) -> Option<ProviderBinaryLayout> {
        self.binary_layout
    }
    /// Returns affine integer scaling, when declared.
    #[must_use]
    pub const fn integer_scale(&self) -> Option<IntegerScale> {
        self.integer_scale
    }
    /// Returns the immutable configuration hash.
    #[must_use]
    pub const fn configuration_hash(&self) -> u64 {
        self.configuration_hash
    }
}
