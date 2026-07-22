//! Typed catalog domains converted to SQL text only at SPI boundaries.

use context_core::DistanceMetric;
use pgrx::prelude::PgSqlErrorCode;

use crate::error::raise_sql_error;

pub(crate) fn parse_distance_metric(value: &str) -> Option<DistanceMetric> {
    match value {
        "l2" => Some(DistanceMetric::L2),
        "inner_product" => Some(DistanceMetric::NegativeInnerProduct),
        "cosine" => Some(DistanceMetric::Cosine),
        "l1" => Some(DistanceMetric::L1),
        "hamming" => Some(DistanceMetric::Hamming),
        "jaccard" => Some(DistanceMetric::Jaccard),
        _ => None,
    }
}

fn parse_numeric_distance_metric(value: &str) -> Option<DistanceMetric> {
    parse_distance_metric(value).filter(|metric| {
        matches!(
            metric,
            DistanceMetric::L2
                | DistanceMetric::NegativeInnerProduct
                | DistanceMetric::Cosine
                | DistanceMetric::L1
        )
    })
}

pub(crate) fn distance_metric_from_sql(value: &str, subject: &str) -> DistanceMetric {
    parse_numeric_distance_metric(value).unwrap_or_else(|| {
        let subject = metric_subject_prefix(subject);
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_FEATURE_NOT_SUPPORTED,
            format!("unsupported {subject}distance metric: {value}"),
        )
    })
}

pub(crate) fn distance_metric_from_query(value: &str, subject: &str) -> DistanceMetric {
    parse_numeric_distance_metric(value).unwrap_or_else(|| {
        let subject = metric_subject_prefix(subject);
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("unsupported {subject}distance metric: {value}"),
        )
    })
}

fn metric_subject_prefix(subject: &str) -> String {
    if subject.is_empty() {
        String::new()
    } else {
        format!("{subject} ")
    }
}

pub(crate) fn distance_metric_from_catalog(value: String, subject: &str) -> DistanceMetric {
    parse_distance_metric(&value).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("unexpected {subject} distance metric in catalog: {value}"),
        )
    })
}

pub(crate) const fn distance_metric_label(metric: DistanceMetric) -> &'static str {
    match metric {
        DistanceMetric::L2 => "l2",
        DistanceMetric::InnerProduct | DistanceMetric::NegativeInnerProduct => "inner_product",
        DistanceMetric::Cosine => "cosine",
        DistanceMetric::L1 => "l1",
        DistanceMetric::Hamming => "hamming",
        DistanceMetric::Jaccard => "jaccard",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VectorStatus {
    Ready,
    Building,
    Disabled,
    Failed,
}

impl VectorStatus {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "ready" => Some(Self::Ready),
            "building" => Some(Self::Building),
            "disabled" => Some(Self::Disabled),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    pub(crate) const fn as_sql(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Building => "building",
            Self::Disabled => "disabled",
            Self::Failed => "failed",
        }
    }
}

pub(crate) fn vector_status_from_sql(value: &str) -> VectorStatus {
    VectorStatus::parse(value).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("unsupported vector status: {value}"),
        )
    })
}

pub(crate) fn vector_status_from_catalog(value: String) -> VectorStatus {
    VectorStatus::parse(&value).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("unexpected vector status in catalog: {value}"),
        )
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtifactKind {
    Index,
    Segment,
    SparseIndex,
    Mmap,
}

impl ArtifactKind {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "index" => Some(Self::Index),
            "segment" => Some(Self::Segment),
            "sparse_index" => Some(Self::SparseIndex),
            "mmap" => Some(Self::Mmap),
            _ => None,
        }
    }

    pub(crate) const fn as_sql(self) -> &'static str {
        match self {
            Self::Index => "index",
            Self::Segment => "segment",
            Self::SparseIndex => "sparse_index",
            Self::Mmap => "mmap",
        }
    }
}

pub(crate) fn artifact_kind_from_sql(value: &str) -> ArtifactKind {
    ArtifactKind::parse(value).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
            format!("unsupported pgContext artifact or projection target: {value}"),
        )
    })
}

pub(crate) fn artifact_kind_from_catalog(value: String) -> ArtifactKind {
    ArtifactKind::parse(&value).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("unexpected artifact kind in catalog: {value}"),
        )
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtifactLifecycleState {
    Validated,
    FileMaterialized,
    RebuildRequired,
    Retired,
}

impl ArtifactLifecycleState {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "validated" => Some(Self::Validated),
            "file_materialized" => Some(Self::FileMaterialized),
            "rebuild_required" => Some(Self::RebuildRequired),
            "retired" => Some(Self::Retired),
            _ => None,
        }
    }

    pub(crate) const fn as_sql(self) -> &'static str {
        match self {
            Self::Validated => "validated",
            Self::FileMaterialized => "file_materialized",
            Self::RebuildRequired => "rebuild_required",
            Self::Retired => "retired",
        }
    }
}

pub(crate) fn artifact_lifecycle_state_from_catalog(value: String) -> ArtifactLifecycleState {
    ArtifactLifecycleState::parse(&value).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            format!("unexpected artifact lifecycle state in catalog: {value}"),
        )
    })
}

#[cfg(test)]
mod tests {
    use context_core::DistanceMetric;

    use super::{
        ArtifactKind, ArtifactLifecycleState, VectorStatus, distance_metric_label,
        parse_distance_metric, parse_numeric_distance_metric,
    };

    #[test]
    fn catalog_distance_labels_round_trip_to_typed_metrics() {
        for metric in [
            DistanceMetric::L2,
            DistanceMetric::NegativeInnerProduct,
            DistanceMetric::Cosine,
            DistanceMetric::L1,
            DistanceMetric::Hamming,
            DistanceMetric::Jaccard,
        ] {
            assert_eq!(
                parse_distance_metric(distance_metric_label(metric)),
                Some(metric)
            );
        }
        assert_eq!(parse_distance_metric("typo"), None);
        assert_eq!(parse_numeric_distance_metric("hamming"), None);
        assert_eq!(parse_numeric_distance_metric("jaccard"), None);
    }

    #[test]
    fn lifecycle_and_artifact_labels_are_closed_domains() {
        assert_eq!(VectorStatus::parse("ready"), Some(VectorStatus::Ready));
        assert_eq!(VectorStatus::parse("other"), None);
        assert_eq!(ArtifactKind::parse("mmap"), Some(ArtifactKind::Mmap));
        assert_eq!(ArtifactKind::parse("native_index"), None);
        assert_eq!(
            ArtifactLifecycleState::parse("file_materialized"),
            Some(ArtifactLifecycleState::FileMaterialized)
        );
        assert_eq!(ArtifactLifecycleState::parse("published"), None);
    }
}
