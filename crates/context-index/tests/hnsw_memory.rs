//! HNSW memory-estimator tests.

use std::mem::size_of;

use context_core::{DenseVector, DistanceMetric};
use context_index::{HnswConfig, HnswError, HnswGraph, HnswNodeId, HnswPointId};

fn vector(values: &[f32]) -> context_index::Result<DenseVector> {
    DenseVector::new(values.to_vec()).map_err(HnswError::from)
}

#[test]
fn hnsw_memory_estimate_is_empty_for_empty_graph() -> context_index::Result<()> {
    let graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(2, 8, 8)?);
    let estimate = graph.memory_estimate();

    assert_eq!(estimate.node_count(), 0);
    assert_eq!(estimate.vector_bytes(), 0);
    assert_eq!(estimate.link_bytes(), 0);
    assert_eq!(estimate.total_bytes(), 0);
    Ok(())
}

#[test]
fn hnsw_memory_estimate_counts_vectors_and_links() -> context_index::Result<()> {
    let mut graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(2, 8, 8)?);
    graph.insert(HnswPointId::new(10), vector(&[0.0, 0.0])?)?;
    graph.insert(HnswPointId::new(20), vector(&[1.0, 0.0])?)?;
    graph.insert(HnswPointId::new(30), vector(&[0.0, 1.0])?)?;

    let estimate = graph.memory_estimate();
    let link_count = (0..graph.len())
        .map(|index| {
            graph
                .neighbors(HnswNodeId::new(index), context_index::LayerIndex::base())
                .map_or(0, <[_]>::len)
        })
        .sum::<usize>();

    assert_eq!(estimate.node_count(), 3);
    assert_eq!(estimate.vector_bytes(), 3 * 2 * size_of::<f32>());
    assert_eq!(estimate.link_bytes(), link_count * size_of::<HnswNodeId>());
    assert_eq!(
        estimate.total_bytes(),
        estimate.vector_bytes() + estimate.link_bytes()
    );
    Ok(())
}
