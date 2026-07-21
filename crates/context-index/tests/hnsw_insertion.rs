//! HNSW insertion behavior tests.

use context_core::{DenseVector, DistanceMetric};
use context_index::{
    HnswConfig, HnswError, HnswGraph, HnswGraphNodeSnapshot, HnswLevel, HnswNodeId, HnswPointId,
    LayerIndex,
};

fn vector(values: &[f32]) -> context_index::Result<DenseVector> {
    DenseVector::new(values.to_vec()).map_err(HnswError::from)
}

#[test]
fn hnsw_inserts_the_first_node_without_neighbors() -> context_index::Result<()> {
    let graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(2, 8, 8)?);
    let (graph, node_id) = insert_one(graph, 10, &[0.0, 0.0])?;

    assert_eq!(node_id, HnswNodeId::new(0));
    assert_eq!(graph.len(), 1);
    assert_eq!(graph.entry_point(), Some(node_id));
    assert_eq!(graph.point_id(node_id), Some(HnswPointId::new(10)));
    assert_eq!(graph.layer_count(node_id), Some(1));
    assert_eq!(graph.neighbors(node_id, LayerIndex::base()), Some(&[][..]));
    Ok(())
}

#[test]
fn hnsw_insertion_connects_new_nodes_bidirectionally() -> context_index::Result<()> {
    let graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(2, 8, 8)?);
    let (graph, first) = insert_one(graph, 10, &[0.0, 0.0])?;
    let (graph, second) = insert_one(graph, 20, &[1.0, 0.0])?;

    assert_eq!(
        graph.neighbors(first, LayerIndex::base()),
        Some(&[second][..])
    );
    assert_eq!(
        graph.neighbors(second, LayerIndex::base()),
        Some(&[first][..])
    );
    Ok(())
}

#[test]
fn hnsw_base_layer_uses_m0_twice_the_upper_layer_degree() -> context_index::Result<()> {
    let graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(2, 8, 8)?);
    let (graph, first) = insert_one(graph, 10, &[0.0, 0.0])?;
    let (graph, second) = insert_one(graph, 20, &[10.0, 0.0])?;
    let (graph, third) = insert_one(graph, 30, &[1.0, 0.0])?;
    let (graph, fourth) = insert_one(graph, 40, &[2.0, 0.0])?;
    let (graph, fifth) = insert_one(graph, 50, &[3.0, 0.0])?;
    let (graph, sixth) = insert_one(graph, 60, &[4.0, 0.0])?;

    let mut observed_more_than_m = false;
    for node in [first, second, third, fourth, fifth, sixth] {
        let Some(neighbors) = graph.neighbors(node, LayerIndex::base()) else {
            return Err(HnswError::InvalidSnapshot {
                reason: "inserted node has no base layer",
            });
        };
        assert!(neighbors.len() <= 4);
        observed_more_than_m |= neighbors.len() > 2;
        for neighbor in neighbors {
            assert!(
                graph
                    .neighbors(*neighbor, LayerIndex::base())
                    .is_some_and(|reverse| reverse.contains(&node)),
                "pruned edges remain bidirectional"
            );
        }
    }
    assert!(
        observed_more_than_m,
        "base layer never used its M0 capacity"
    );
    Ok(())
}

#[test]
fn hnsw_neighbor_heuristic_prefers_directional_diversity() -> context_index::Result<()> {
    let mut graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(2, 8, 8)?);
    let level = HnswLevel::new(1)?;
    let diverse = graph
        .insert_at_level(HnswPointId::new(10), vector(&[0.0, 2.0])?, level)?
        .node_id();
    let redundant = graph
        .insert_at_level(HnswPointId::new(20), vector(&[1.1, 0.0])?, level)?
        .node_id();
    let closest = graph
        .insert_at_level(HnswPointId::new(30), vector(&[1.0, 0.0])?, level)?
        .node_id();
    let inserted = graph
        .insert_at_level(HnswPointId::new(40), vector(&[0.0, 0.0])?, level)?
        .node_id();

    let upper =
        graph
            .neighbors(inserted, LayerIndex::new(1))
            .ok_or(HnswError::InvalidSnapshot {
                reason: "inserted node has no upper layer",
            })?;
    assert!(upper.contains(&closest));
    assert!(upper.contains(&diverse));
    assert!(!upper.contains(&redundant));
    Ok(())
}

#[test]
fn hnsw_insertion_rejects_dimension_mismatch() -> context_index::Result<()> {
    let graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(2, 8, 8)?);
    let (mut graph, _) = insert_one(graph, 10, &[0.0, 0.0])?;

    let result = graph.insert(HnswPointId::new(20), vector(&[1.0, 0.0, 0.0])?);

    assert!(matches!(
        result,
        Err(HnswError::DimensionMismatch { left: 2, right: 3 })
    ));
    Ok(())
}

#[test]
fn ef_construction_changes_bounded_candidate_exploration() -> context_index::Result<()> {
    let mut narrow = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(4, 4, 8)?);
    let mut broad = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(4, 32, 8)?);
    let mut narrow_distances = 0_usize;
    let mut broad_distances = 0_usize;

    for point_index in 0_u16..64 {
        let level = HnswLevel::new(
            usize::try_from(point_index.trailing_zeros().min(4)).map_err(|_| {
                HnswError::InvalidParameter {
                    parameter: "level",
                    value: usize::MAX,
                }
            })?,
        )?;
        let values = [
            f32::from(point_index % 11),
            f32::from((point_index * 7) % 13),
        ];
        let point_id = HnswPointId::new(u64::from(point_index) + 1);
        narrow_distances += narrow
            .insert_at_level(point_id, vector(&values)?, level)?
            .work()
            .distance_evaluations();
        broad_distances += broad
            .insert_at_level(point_id, vector(&values)?, level)?
            .work()
            .distance_evaluations();
    }

    assert!(broad_distances > narrow_distances);
    Ok(())
}

#[test]
fn hnsw_insertion_allows_duplicate_vectors_as_distinct_nodes() -> context_index::Result<()> {
    let graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(2, 8, 8)?);
    let (graph, first) = insert_one(graph, 10, &[1.0, 1.0])?;
    let (graph, second) = insert_one(graph, 20, &[1.0, 1.0])?;
    let (graph, third) = insert_one(graph, 30, &[1.0, 1.0])?;

    assert_eq!(first, HnswNodeId::new(0));
    assert_eq!(second, HnswNodeId::new(1));
    assert_eq!(third, HnswNodeId::new(2));
    assert_eq!(graph.point_id(first), Some(HnswPointId::new(10)));
    assert_eq!(graph.point_id(second), Some(HnswPointId::new(20)));
    assert_eq!(graph.point_id(third), Some(HnswPointId::new(30)));
    assert_eq!(
        graph.neighbors(third, LayerIndex::base()),
        Some(&[first, second][..])
    );
    Ok(())
}

#[test]
fn hnsw_duplicate_vector_pruning_is_deterministic_by_node_id() -> context_index::Result<()> {
    let graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(2, 8, 8)?);
    let (graph, first) = insert_one(graph, 10, &[1.0, 1.0])?;
    let (graph, second) = insert_one(graph, 20, &[1.0, 1.0])?;
    let (graph, third) = insert_one(graph, 30, &[1.0, 1.0])?;

    assert_eq!(
        graph.neighbors(first, LayerIndex::base()),
        Some(&[second, third][..])
    );
    assert_eq!(
        graph.neighbors(second, LayerIndex::base()),
        Some(&[first, third][..])
    );
    assert_eq!(
        graph.neighbors(third, LayerIndex::base()),
        Some(&[first, second][..])
    );
    Ok(())
}

#[test]
fn hnsw_graph_round_trips_base_layer_snapshots() -> context_index::Result<()> {
    let graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(2, 8, 8)?);
    let (graph, first) = insert_one(graph, 10, &[0.0, 0.0])?;
    let (graph, second) = insert_one(graph, 20, &[1.0, 0.0])?;
    let (graph, third) = insert_one(graph, 30, &[0.0, 1.0])?;

    let rebuilt = HnswGraph::from_base_layer_snapshots(
        DistanceMetric::L2,
        HnswConfig::new(2, 8, 8)?,
        graph.node_snapshots(),
    )?;

    assert_eq!(rebuilt.entry_point(), Some(first));
    assert_eq!(rebuilt.point_id(first), Some(HnswPointId::new(10)));
    assert_eq!(rebuilt.point_id(second), Some(HnswPointId::new(20)));
    assert_eq!(rebuilt.point_id(third), Some(HnswPointId::new(30)));
    assert_eq!(
        rebuilt.neighbors(third, LayerIndex::base()),
        graph.neighbors(third, LayerIndex::base())
    );
    Ok(())
}

#[test]
fn hnsw_graph_reload_preserves_persisted_entry_point() -> context_index::Result<()> {
    let graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(2, 8, 8)?);
    let (graph, _) = insert_one(graph, 10, &[0.0, 0.0])?;
    let (graph, second) = insert_one(graph, 20, &[1.0, 0.0])?;

    let rebuilt = HnswGraph::from_persisted_snapshots(
        DistanceMetric::L2,
        HnswConfig::new(2, 8, 8)?,
        Some(second),
        graph.node_snapshots(),
    )?;

    assert_eq!(rebuilt.entry_point(), Some(second));
    Ok(())
}

#[test]
fn hnsw_graph_reload_rejects_invalid_persisted_entry_point() -> context_index::Result<()> {
    let graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(2, 8, 8)?);
    let (graph, _) = insert_one(graph, 10, &[0.0, 0.0])?;

    assert!(matches!(
        HnswGraph::from_persisted_snapshots(
            DistanceMetric::L2,
            HnswConfig::new(2, 8, 8)?,
            Some(HnswNodeId::new(9)),
            graph.node_snapshots(),
        ),
        Err(HnswError::InvalidParameter {
            parameter: "entry_point",
            value: 9,
        })
    ));
    Ok(())
}

#[test]
fn hnsw_graph_snapshot_rejects_non_contiguous_node_ids() -> context_index::Result<()> {
    let snapshots = vec![HnswGraphNodeSnapshot::new(
        HnswNodeId::new(7),
        HnswPointId::new(10),
        vector(&[0.0, 0.0])?,
        Vec::new(),
    )];

    let result = HnswGraph::from_base_layer_snapshots(
        DistanceMetric::L2,
        HnswConfig::new(2, 8, 8)?,
        snapshots,
    );

    assert!(result.is_err(), "non-contiguous node id should fail");
    if let Err(error) = result {
        assert_eq!(
            error,
            HnswError::InvalidParameter {
                parameter: "node_id",
                value: 7
            }
        );
    }
    Ok(())
}

#[test]
fn hnsw_graph_snapshot_rejects_out_of_range_neighbors() -> context_index::Result<()> {
    let snapshots = vec![HnswGraphNodeSnapshot::new(
        HnswNodeId::new(0),
        HnswPointId::new(10),
        vector(&[0.0, 0.0])?,
        vec![HnswNodeId::new(1)],
    )];

    let result = HnswGraph::from_base_layer_snapshots(
        DistanceMetric::L2,
        HnswConfig::new(2, 8, 8)?,
        snapshots,
    );

    assert!(result.is_err(), "out-of-range neighbor should fail");
    if let Err(error) = result {
        assert_eq!(
            error,
            HnswError::InvalidParameter {
                parameter: "neighbor_id",
                value: 1
            }
        );
    }
    Ok(())
}

fn insert_one(
    mut graph: HnswGraph,
    point_id: u64,
    values: &[f32],
) -> context_index::Result<(HnswGraph, HnswNodeId)> {
    let node_id = graph.insert(HnswPointId::new(point_id), vector(values)?)?;
    Ok((graph, node_id))
}
