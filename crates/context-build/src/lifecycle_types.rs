//! Stable type labels shared by build jobs and generation manifests.

/// Operation performed by one supervised generation job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildJobKind {
    /// Build a new immutable artifact inventory.
    ArtifactBuild,
    /// Compact bounded existing artifacts into a replacement inventory.
    Compaction,
    /// Backfill a derived projection from authoritative source rows.
    ProjectionBackfill,
    /// Produce reproducible certification evidence.
    Certification,
}

impl BuildJobKind {
    /// Parses the stable catalog representation.
    #[must_use]
    pub const fn from_catalog(value: &str) -> Option<Self> {
        match value.as_bytes() {
            b"artifact_build" => Some(Self::ArtifactBuild),
            b"compaction" => Some(Self::Compaction),
            b"projection_backfill" => Some(Self::ProjectionBackfill),
            b"certification" => Some(Self::Certification),
            _ => None,
        }
    }

    /// Returns the stable catalog representation.
    #[must_use]
    pub const fn as_catalog(self) -> &'static str {
        match self {
            Self::ArtifactBuild => "artifact_build",
            Self::Compaction => "compaction",
            Self::ProjectionBackfill => "projection_backfill",
            Self::Certification => "certification",
        }
    }
}

/// Typed derived output produced by the shared lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ArtifactKind {
    /// Immutable HNSW search segment.
    HnswSegment,
    /// Mutable or frozen HNSW delta segment.
    HnswDelta,
    /// HNSW segment directory and routing metadata.
    HnswDirectory,
    /// IVFFlat centroid inventory.
    IvfCentroids,
    /// IVFFlat posting inventory.
    IvfPostings,
    /// Quantized vector code inventory.
    QuantizedCodes,
    /// Derived document chunk projection.
    ChunkProjection,
    /// Derived vector projection.
    VectorProjection,
    /// Derived graph projection.
    GraphProjection,
    /// Derived graph community summaries.
    CommunitySummary,
    /// Reproducible benchmark or certification evidence.
    CertificationEvidence,
}

impl ArtifactKind {
    /// Parses the stable catalog representation.
    #[must_use]
    pub const fn from_catalog(value: &str) -> Option<Self> {
        match value.as_bytes() {
            b"hnsw_segment" => Some(Self::HnswSegment),
            b"hnsw_delta" => Some(Self::HnswDelta),
            b"hnsw_directory" => Some(Self::HnswDirectory),
            b"ivf_centroids" => Some(Self::IvfCentroids),
            b"ivf_postings" => Some(Self::IvfPostings),
            b"quantized_codes" => Some(Self::QuantizedCodes),
            b"chunk_projection" => Some(Self::ChunkProjection),
            b"vector_projection" => Some(Self::VectorProjection),
            b"graph_projection" => Some(Self::GraphProjection),
            b"community_summary" => Some(Self::CommunitySummary),
            b"certification_evidence" => Some(Self::CertificationEvidence),
            _ => None,
        }
    }

    /// Returns the stable catalog representation.
    #[must_use]
    pub const fn as_catalog(self) -> &'static str {
        match self {
            Self::HnswSegment => "hnsw_segment",
            Self::HnswDelta => "hnsw_delta",
            Self::HnswDirectory => "hnsw_directory",
            Self::IvfCentroids => "ivf_centroids",
            Self::IvfPostings => "ivf_postings",
            Self::QuantizedCodes => "quantized_codes",
            Self::ChunkProjection => "chunk_projection",
            Self::VectorProjection => "vector_projection",
            Self::GraphProjection => "graph_projection",
            Self::CommunitySummary => "community_summary",
            Self::CertificationEvidence => "certification_evidence",
        }
    }
}

impl std::error::Error for super::BuildError {}

/// Returns the version of the pure build boundary.
#[must_use]
pub const fn build_contract_version() -> u16 {
    3
}
