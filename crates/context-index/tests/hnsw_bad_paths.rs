//! HNSW invalid-input and missing-node behavior tests.

use context_core::{DenseVector, DistanceMetric, SearchLimit};
use context_index::{
    CandidateMask, HnswConfig, HnswError, HnswGraph, HnswGraphSnapshot, HnswLevel, HnswNodeId,
    HnswPointId, LayerIndex,
};

fn vector(values: &[f32]) -> context_index::Result<DenseVector> {
    DenseVector::new(values.to_vec()).map_err(HnswError::from)
}

#[test]
fn hnsw_config_rejects_invalid_parameters() {
    assert_eq!(
        HnswConfig::new(0, 8, 8),
        Err(HnswError::InvalidParameter {
            parameter: "m",
            value: 0,
        })
    );
    assert_eq!(
        HnswConfig::new(1, 8, 8),
        Err(HnswError::InvalidParameter {
            parameter: "m",
            value: 1,
        })
    );
    assert!(HnswConfig::new(context_core::policy::MAX_HNSW_M + 1, 256, 8).is_err());
    assert!(HnswConfig::new(4, context_core::policy::MAX_HNSW_EF_CONSTRUCTION + 1, 8).is_err());
    assert!(HnswConfig::new(4, 8, context_core::policy::MAX_HNSW_EF_SEARCH + 1).is_err());
    assert_eq!(
        HnswConfig::new(4, 0, 8),
        Err(HnswError::InvalidParameter {
            parameter: "ef_construction",
            value: 0,
        })
    );
    assert_eq!(
        HnswConfig::new(4, 3, 8),
        Err(HnswError::InvalidParameter {
            parameter: "ef_construction",
            value: 3,
        })
    );
    assert_eq!(
        HnswConfig::new(4, 8, 0),
        Err(HnswError::InvalidParameter {
            parameter: "ef_search",
            value: 0,
        })
    );
}

#[test]
fn hnsw_missing_node_introspection_returns_none() -> context_index::Result<()> {
    let mut graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(2, 8, 8)?);
    graph.insert(HnswPointId::new(10), vector(&[0.0, 0.0])?)?;
    let missing = HnswNodeId::new(99);

    assert_eq!(graph.point_id(missing), None);
    assert_eq!(graph.layer_count(missing), None);
    assert_eq!(graph.neighbors(missing, LayerIndex::base()), None);
    Ok(())
}

#[test]
fn hnsw_search_handles_tiny_datasets() -> context_index::Result<()> {
    let mut graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(4, 16, 16)?);
    graph.insert(HnswPointId::new(10), vector(&[1.0, 0.0])?)?;

    let results = graph.search(
        &vector(&[0.0, 0.0])?,
        SearchLimit::new(3).map_err(HnswError::from)?,
    )?;

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].point_id(), HnswPointId::new(10));
    Ok(())
}

#[test]
fn cosine_insertion_rejects_zero_vectors_without_mutating_the_graph() -> context_index::Result<()> {
    let mut graph = HnswGraph::new(DistanceMetric::Cosine, HnswConfig::new(4, 16, 16)?);
    let before = graph.snapshot();

    assert!(matches!(
        graph.insert(HnswPointId::new(10), vector(&[0.0, 0.0])?),
        Err(HnswError::Core(_))
    ));
    assert_eq!(graph.snapshot(), before);
    Ok(())
}

#[test]
fn hnsw_search_with_missing_mask_points_returns_empty_results() -> context_index::Result<()> {
    let mut graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(4, 16, 16)?);
    graph.insert(HnswPointId::new(10), vector(&[1.0, 0.0])?)?;
    let mask = CandidateMask::only([HnswPointId::new(99)]);

    let results = graph.search_with_mask(
        &vector(&[0.0, 0.0])?,
        SearchLimit::new(3).map_err(HnswError::from)?,
        &mask,
    )?;

    assert_eq!(results, Vec::new());
    Ok(())
}

#[test]
fn hierarchy_snapshot_rejects_every_truncated_prefix_and_header_corruption()
-> context_index::Result<()> {
    let (_, encoded) = hierarchy_snapshot_fixture()?;

    for prefix_len in 0..encoded.len() {
        assert!(matches!(
            HnswGraphSnapshot::from_bytes(&encoded[..prefix_len]),
            Err(HnswError::InvalidSnapshot { .. })
        ));
    }

    for (offset, value) in [(0, b'X'), (4, 2), (6, 1)] {
        let mut corrupted = encoded.clone();
        corrupted[offset] = value;
        assert!(matches!(
            HnswGraphSnapshot::from_bytes(&corrupted),
            Err(HnswError::InvalidSnapshot { .. })
        ));
    }

    let mut trailing = encoded;
    trailing.push(0);
    assert!(matches!(
        HnswGraphSnapshot::from_bytes(&trailing),
        Err(HnswError::InvalidSnapshot { .. })
    ));
    Ok(())
}

#[test]
fn hierarchy_restore_rejects_noncontiguous_ids_duplicate_points_and_invalid_vectors()
-> context_index::Result<()> {
    let (config, encoded) = hierarchy_snapshot_fixture()?;

    let mut wrong_node_id = encoded.clone();
    wrong_node_id[32..40].copy_from_slice(&9_u64.to_le_bytes());
    assert_invalid_restore(config, &wrong_node_id)?;

    // Two base-layer, two-dimensional nodes with one neighbor each occupy
    // 42 bytes per record; the second point id begins at byte 82.
    let mut duplicate_point_id = encoded.clone();
    duplicate_point_id[82..90].copy_from_slice(&10_u64.to_le_bytes());
    assert_invalid_restore(config, &duplicate_point_id)?;

    let mut non_finite_vector = encoded;
    non_finite_vector[56..60].copy_from_slice(&f32::NAN.to_bits().to_le_bytes());
    assert!(matches!(
        HnswGraphSnapshot::from_bytes(&non_finite_vector),
        Err(HnswError::InvalidSnapshot { .. })
    ));
    Ok(())
}

#[test]
fn hierarchy_restore_rejects_a_disconnected_induced_layer() -> context_index::Result<()> {
    let config = HnswConfig::new(2, 8, 8)?;
    let mut encoded = Vec::new();
    encoded.extend_from_slice(b"HSG1");
    encoded.extend_from_slice(&1_u16.to_le_bytes());
    encoded.extend_from_slice(&0_u16.to_le_bytes());
    encoded.extend_from_slice(&0_u64.to_le_bytes());
    encoded.extend_from_slice(&0_u64.to_le_bytes());
    encoded.extend_from_slice(&2_u64.to_le_bytes());
    for (node_id, point_id, x) in [(0_u64, 10_u64, 0.0_f32), (1, 20, 1.0)] {
        encoded.extend_from_slice(&node_id.to_le_bytes());
        encoded.extend_from_slice(&point_id.to_le_bytes());
        encoded.extend_from_slice(&2_u32.to_le_bytes());
        encoded.extend_from_slice(&1_u16.to_le_bytes());
        encoded.extend_from_slice(&0_u16.to_le_bytes());
        encoded.extend_from_slice(&x.to_bits().to_le_bytes());
        encoded.extend_from_slice(&0.0_f32.to_bits().to_le_bytes());
        encoded.extend_from_slice(&0_u16.to_le_bytes());
    }

    assert_invalid_restore(config, &encoded)
}

fn hierarchy_snapshot_fixture() -> context_index::Result<(HnswConfig, Vec<u8>)> {
    let config = HnswConfig::new(2, 8, 8)?;
    let mut graph = HnswGraph::new(DistanceMetric::L2, config);
    graph.insert_at_level(
        HnswPointId::new(10),
        vector(&[0.0, 0.0])?,
        HnswLevel::base(),
    )?;
    graph.insert_at_level(
        HnswPointId::new(20),
        vector(&[1.0, 0.0])?,
        HnswLevel::base(),
    )?;
    Ok((config, graph.snapshot().to_bytes()?))
}

fn assert_invalid_restore(config: HnswConfig, bytes: &[u8]) -> context_index::Result<()> {
    let decoded = HnswGraphSnapshot::from_bytes(bytes)?;
    assert!(matches!(
        HnswGraph::from_snapshot(DistanceMetric::L2, config, decoded),
        Err(HnswError::InvalidSnapshot { .. })
    ));
    Ok(())
}
