//! Storage-agnostic HNSW graph port contracts.

#![allow(clippy::expect_used, clippy::panic)]

use context_core::{DenseVector, DistanceMetric, SearchLimit};
use context_index::{
    CandidateMask, GraphError, GraphMetadata, GraphNeighbors, GraphNodeRecord, GraphNodeView,
    GraphRead, GraphRecordId, GraphResult, GraphWrite, HnswConfig, HnswError, HnswNodeId,
    HnswPointId, InMemoryGraphStore, LayerIndex, MAX_GRAPH_LAYERS, MAX_GRAPH_NEIGHBORS_PER_LAYER,
    NeverCancel, NewGraphNode, search_graph_read, search_graph_read_with_mask,
    search_graph_read_with_mask_budgeted,
};

fn vector(values: &[f32]) -> DenseVector {
    DenseVector::new(values.to_vec()).expect("graph-port vector fixture should be valid")
}

fn append(
    graph: &mut impl GraphWrite,
    record_id: u64,
    values: &[f32],
    layers: Vec<Vec<HnswNodeId>>,
) -> HnswNodeId {
    graph
        .append_node(NewGraphNode::new(
            GraphRecordId::new(record_id),
            HnswPointId::new(record_id + 1_000),
            vector(values),
            layers,
        ))
        .expect("graph-port append fixture should be valid")
}

#[test]
fn graph_ports_round_trip_owned_hierarchical_records() {
    let mut graph = InMemoryGraphStore::new();
    let first = append(&mut graph, 10, &[0.0, 0.0], vec![vec![], vec![]]);
    let second = append(&mut graph, 20, &[1.0, 0.0], vec![vec![first], vec![first]]);

    graph
        .replace_neighbors(first, LayerIndex::base(), vec![second])
        .expect("base-layer rewire should succeed");
    graph
        .replace_neighbors(first, LayerIndex::new(1), vec![second])
        .expect("upper-layer rewire should succeed");
    graph
        .publish_entry_point(Some(second))
        .expect("entry-point publication should succeed");

    let metadata = graph.metadata().expect("metadata read should succeed");
    assert_eq!(metadata.node_count(), 2);
    assert_eq!(metadata.dimensions(), Some(2));
    assert_eq!(metadata.entry_point(), Some(second));

    let record = graph
        .read_node(second)
        .expect("node read should succeed")
        .expect("second node should exist");
    assert_eq!(record.node_id(), second);
    assert_eq!(record.record_id(), GraphRecordId::new(20));
    assert_eq!(record.vector().as_slice(), &[1.0, 0.0]);
    assert_eq!(record.layer_count(), 2);
    assert_eq!(
        graph
            .read_neighbors(second, LayerIndex::base())
            .expect("base adjacency read should succeed")
            .expect("base adjacency should exist")
            .neighbors(),
        &[first]
    );
    assert_eq!(
        graph
            .read_neighbors(second, LayerIndex::new(1))
            .expect("upper adjacency read should succeed")
            .expect("upper adjacency should exist")
            .neighbors(),
        &[first]
    );
}

#[test]
fn graph_read_traversal_returns_owned_point_identities() {
    let mut graph = InMemoryGraphStore::new();
    let first = append(&mut graph, 10, &[0.0, 0.0], vec![vec![]]);
    let second = append(&mut graph, 20, &[2.0, 0.0], vec![vec![first]]);
    graph
        .replace_neighbors(first, LayerIndex::base(), vec![second])
        .expect("base-layer rewire should succeed");
    graph
        .publish_entry_point(Some(second))
        .expect("entry-point publication should succeed");

    let outcome = search_graph_read(
        &mut graph,
        DistanceMetric::L2,
        &vector(&[0.0, 0.0]),
        HnswConfig::new(2, 4, 4).expect("test config should be valid"),
        SearchLimit::new(2).expect("test limit should be valid"),
        &mut NeverCancel,
    )
    .expect("graph-read traversal should succeed");

    assert_eq!(
        outcome
            .results()
            .iter()
            .map(|result| result.point_id().get())
            .collect::<Vec<_>>(),
        vec![1010, 1020]
    );
}

#[test]
fn graph_read_traversal_prefers_buffer_scoped_hot_path_methods() {
    struct HotPathGraph {
        owned_node_reads: usize,
        owned_neighbor_reads: usize,
        node_visits: usize,
        neighbor_decodes: usize,
    }

    impl GraphRead for HotPathGraph {
        fn metadata(&mut self) -> GraphResult<GraphMetadata> {
            GraphMetadata::new(2, Some(HnswNodeId::new(1)), Some(2))
        }

        fn read_node(&mut self, _node_id: HnswNodeId) -> GraphResult<Option<GraphNodeRecord>> {
            self.owned_node_reads += 1;
            panic!("traversal must not materialize owned node records")
        }

        fn with_node<R>(
            &mut self,
            node_id: HnswNodeId,
            visitor: impl FnOnce(GraphNodeView<'_>) -> R,
        ) -> GraphResult<Option<R>> {
            const FIRST: [f32; 2] = [0.0, 0.0];
            const SECOND: [f32; 2] = [2.0, 0.0];
            let (point_id, values) = match node_id.get() {
                0 => (HnswPointId::new(1010), FIRST.as_slice()),
                1 => (HnswPointId::new(1020), SECOND.as_slice()),
                _ => return Ok(None),
            };
            self.node_visits += 1;
            GraphNodeView::new(2, node_id, point_id, values, 1).map(|view| Some(visitor(view)))
        }

        fn read_neighbors(
            &mut self,
            _node_id: HnswNodeId,
            _layer: LayerIndex,
        ) -> GraphResult<Option<GraphNeighbors>> {
            self.owned_neighbor_reads += 1;
            panic!("traversal must not allocate owned adjacency")
        }

        fn read_neighbors_into(
            &mut self,
            node_id: HnswNodeId,
            layer: LayerIndex,
            output: &mut Vec<HnswNodeId>,
        ) -> GraphResult<bool> {
            assert_eq!(layer, LayerIndex::base());
            output.clear();
            match node_id.get() {
                0 => output.push(HnswNodeId::new(1)),
                1 => output.push(HnswNodeId::new(0)),
                _ => return Ok(false),
            }
            self.neighbor_decodes += 1;
            Ok(true)
        }
    }

    let mut graph = HotPathGraph {
        owned_node_reads: 0,
        owned_neighbor_reads: 0,
        node_visits: 0,
        neighbor_decodes: 0,
    };
    let outcome = search_graph_read(
        &mut graph,
        DistanceMetric::L2,
        &vector(&[0.0, 0.0]),
        HnswConfig::new(2, 4, 4).expect("test config should be valid"),
        SearchLimit::new(2).expect("test limit should be valid"),
        &mut NeverCancel,
    )
    .expect("hot-path traversal should succeed");

    assert_eq!(outcome.results().len(), 2);
    assert_eq!(graph.owned_node_reads, 0);
    assert_eq!(graph.owned_neighbor_reads, 0);
    assert!(graph.node_visits >= 2);
    assert!(graph.neighbor_decodes >= 1);
}

#[test]
fn graph_read_traversal_uses_the_selected_metric() {
    let mut graph = InMemoryGraphStore::new();
    let first = append(&mut graph, 10, &[1.0, 0.0], vec![vec![]]);
    let second = append(&mut graph, 20, &[3.0, 0.0], vec![vec![first]]);
    graph
        .replace_neighbors(first, LayerIndex::base(), vec![second])
        .expect("base-layer rewire should succeed");
    graph
        .publish_entry_point(Some(first))
        .expect("entry-point publication should succeed");

    let outcome = search_graph_read(
        &mut graph,
        DistanceMetric::NegativeInnerProduct,
        &vector(&[1.0, 0.0]),
        HnswConfig::new(2, 4, 4).expect("test config should be valid"),
        SearchLimit::new(2).expect("test limit should be valid"),
        &mut NeverCancel,
    )
    .expect("graph-read metric traversal should succeed");

    assert_eq!(
        outcome
            .results()
            .iter()
            .map(|result| result.point_id().get())
            .collect::<Vec<_>>(),
        vec![1020, 1010]
    );
}

#[test]
fn graph_read_mask_keeps_connectors_but_excludes_masked_results() {
    let mut graph = InMemoryGraphStore::new();
    let first = append(&mut graph, 10, &[0.0, 0.0], vec![vec![]]);
    let second = append(&mut graph, 20, &[2.0, 0.0], vec![vec![first]]);
    graph
        .replace_neighbors(first, LayerIndex::base(), vec![second])
        .expect("base-layer rewire should succeed");
    graph
        .publish_entry_point(Some(second))
        .expect("entry-point publication should succeed");

    let outcome = search_graph_read_with_mask(
        &mut graph,
        DistanceMetric::L2,
        &vector(&[0.0, 0.0]),
        HnswConfig::new(2, 4, 4).expect("test config should be valid"),
        SearchLimit::new(2).expect("test limit should be valid"),
        &CandidateMask::only([HnswPointId::new(1_010)]),
        &mut NeverCancel,
    )
    .expect("masked graph-read traversal should succeed");

    assert_eq!(
        outcome
            .results()
            .iter()
            .map(|result| result.point_id().get())
            .collect::<Vec<_>>(),
        vec![1_010]
    );
    assert!(outcome.work().node_expansions() > 0);
}

#[test]
fn graph_read_mask_budgeted_rejects_below_the_explicit_limit_but_accepts_above_the_default() {
    let mut graph = InMemoryGraphStore::new();
    let first = append(&mut graph, 10, &[0.0, 0.0], vec![vec![]]);
    let second = append(&mut graph, 20, &[2.0, 0.0], vec![vec![first]]);
    graph
        .replace_neighbors(first, LayerIndex::base(), vec![second])
        .expect("base-layer rewire should succeed");
    graph
        .publish_entry_point(Some(second))
        .expect("entry-point publication should succeed");

    let above_default_count = context_core::policy::DEFAULT_HNSW_CANDIDATE_MASK_POINTS + 1;
    let mask = CandidateMask::only(
        (0..above_default_count as u64).map(|offset| HnswPointId::new(1_010 + offset)),
    );

    let rejected = search_graph_read_with_mask_budgeted(
        &mut graph,
        DistanceMetric::L2,
        &vector(&[0.0, 0.0]),
        HnswConfig::new(2, 4, 4).expect("test config should be valid"),
        SearchLimit::new(2).expect("test limit should be valid"),
        &mask,
        above_default_count - 1,
        &mut NeverCancel,
    );
    assert_eq!(
        rejected,
        Err(HnswError::RecallBudgetExceeded {
            max: above_default_count - 1,
            actual: above_default_count,
        })
    );

    let accepted = search_graph_read_with_mask_budgeted(
        &mut graph,
        DistanceMetric::L2,
        &vector(&[0.0, 0.0]),
        HnswConfig::new(2, 4, 4).expect("test config should be valid"),
        SearchLimit::new(2).expect("test limit should be valid"),
        &mask,
        above_default_count,
        &mut NeverCancel,
    )
    .expect("mask above the library default should succeed under an explicit higher budget");
    assert_eq!(accepted.results()[0].point_id().get(), 1_010);
}

#[test]
fn graph_read_mask_fills_results_in_one_filter_aware_traversal() {
    let mut graph = InMemoryGraphStore::new();
    let entry = append(&mut graph, 10, &[0.0, 0.0], vec![vec![]]);
    let connector = append(&mut graph, 20, &[1.0, 0.0], vec![vec![entry]]);
    let first_allowed = append(&mut graph, 30, &[2.0, 0.0], vec![vec![connector]]);
    let second_allowed = append(&mut graph, 40, &[3.0, 0.0], vec![vec![first_allowed]]);
    graph
        .replace_neighbors(entry, LayerIndex::base(), vec![connector])
        .expect("entry connector should publish");
    graph
        .replace_neighbors(connector, LayerIndex::base(), vec![entry, first_allowed])
        .expect("connector links should publish");
    graph
        .replace_neighbors(
            first_allowed,
            LayerIndex::base(),
            vec![connector, second_allowed],
        )
        .expect("first allowed links should publish");
    graph
        .replace_neighbors(second_allowed, LayerIndex::base(), vec![first_allowed])
        .expect("second allowed reciprocal link should publish");
    graph
        .publish_entry_point(Some(entry))
        .expect("entry-point publication should succeed");

    let outcome = search_graph_read_with_mask(
        &mut graph,
        DistanceMetric::L2,
        &vector(&[0.0, 0.0]),
        HnswConfig::new(2, 4, 2).expect("test config should be valid"),
        SearchLimit::new(2).expect("test limit should be valid"),
        &CandidateMask::only([HnswPointId::new(1_030), HnswPointId::new(1_040)]),
        &mut NeverCancel,
    )
    .expect("filter-aware graph-read traversal should succeed");

    assert_eq!(
        outcome
            .results()
            .iter()
            .map(|result| result.point_id().get())
            .collect::<Vec<_>>(),
        vec![1_030, 1_040]
    );
    assert_eq!(outcome.work().node_expansions(), 4);
}

#[test]
fn sparse_graph_read_mask_uses_acorn_second_hop_for_a_better_hidden_match() {
    let mut graph = InMemoryGraphStore::new();
    let entry = append(&mut graph, 10, &[10.0, 0.0], vec![vec![]]);
    let far_connector = append(&mut graph, 20, &[100.0, 0.0], vec![vec![entry]]);
    let hidden_match = append(&mut graph, 30, &[1.0, 0.0], vec![vec![far_connector]]);
    for record_id in 40..=90_u64 {
        append(&mut graph, record_id, &[200.0, 0.0], vec![vec![]]);
    }
    graph
        .replace_neighbors(entry, LayerIndex::base(), vec![far_connector])
        .expect("entry connector should publish");
    graph
        .replace_neighbors(far_connector, LayerIndex::base(), vec![entry, hidden_match])
        .expect("far connector links should publish");
    graph
        .replace_neighbors(hidden_match, LayerIndex::base(), vec![far_connector])
        .expect("hidden reciprocal link should publish");
    graph
        .publish_entry_point(Some(entry))
        .expect("entry-point publication should succeed");

    let outcome = search_graph_read_with_mask(
        &mut graph,
        DistanceMetric::L2,
        &vector(&[0.0, 0.0]),
        HnswConfig::new(2, 4, 1).expect("test config should be valid"),
        SearchLimit::new(1).expect("test limit should be valid"),
        &CandidateMask::only([HnswPointId::new(1_010), HnswPointId::new(1_030)]),
        &mut NeverCancel,
    )
    .expect("ACORN traversal should succeed");

    assert_eq!(outcome.results()[0].point_id().get(), 1_030);
    assert!(outcome.work().edges_examined() >= 3);
}

#[test]
fn graph_read_ef_one_keeps_descending_until_no_closer_connector_exists() {
    let mut graph = InMemoryGraphStore::new();
    let nearest = append(&mut graph, 10, &[0.0, 0.0], vec![vec![]]);
    let middle = append(&mut graph, 20, &[10.0, 0.0], vec![vec![nearest]]);
    let entry = append(&mut graph, 30, &[20.0, 0.0], vec![vec![middle]]);
    graph
        .replace_neighbors(nearest, LayerIndex::base(), vec![middle])
        .expect("nearest reciprocal link should publish");
    graph
        .replace_neighbors(middle, LayerIndex::base(), vec![nearest, entry])
        .expect("middle reciprocal links should publish");
    graph
        .publish_entry_point(Some(entry))
        .expect("far entry point should publish");

    let outcome = search_graph_read(
        &mut graph,
        DistanceMetric::L2,
        &vector(&[0.0, 0.0]),
        HnswConfig::new(2, 4, 1).expect("test config should be valid"),
        SearchLimit::new(1).expect("test limit should be valid"),
        &mut NeverCancel,
    )
    .expect("ef=1 traversal should succeed");

    assert_eq!(outcome.results()[0].point_id().get(), 1_010);
    assert!(outcome.work().node_expansions() >= 3);
}

#[test]
fn graph_reads_return_owned_records_that_outlive_the_adapter() {
    let (record, adjacency) = {
        let mut graph = InMemoryGraphStore::new();
        let first = append(&mut graph, 10, &[0.0, 0.0], vec![vec![]]);
        let record = graph
            .read_node(first)
            .expect("node read should succeed")
            .expect("first node should exist");
        let adjacency = graph
            .read_neighbors(first, LayerIndex::base())
            .expect("adjacency read should succeed")
            .expect("base adjacency should exist");
        graph
            .replace_neighbors(first, LayerIndex::base(), vec![])
            .expect("an empty rewire should succeed");
        (record, adjacency)
    };

    assert_eq!(record.record_id(), GraphRecordId::new(10));
    assert_eq!(record.vector().as_slice(), &[0.0, 0.0]);
    assert_eq!(record.layer_count(), 1);
    assert!(adjacency.neighbors().is_empty());
}

#[test]
fn borrowed_backing_storage_cannot_escape_through_owned_port_values() {
    struct BorrowedGraph<'a> {
        vector: &'a DenseVector,
        neighbors: &'a [HnswNodeId],
    }

    impl GraphRead for BorrowedGraph<'_> {
        fn metadata(&mut self) -> GraphResult<GraphMetadata> {
            GraphMetadata::new(1, Some(HnswNodeId::new(0)), Some(self.vector.dimension()))
        }

        fn read_node(&mut self, node_id: HnswNodeId) -> GraphResult<Option<GraphNodeRecord>> {
            if node_id != HnswNodeId::new(0) {
                return Ok(None);
            }
            GraphNodeRecord::new(
                1,
                node_id,
                GraphRecordId::new(10),
                HnswPointId::new(77),
                self.vector.clone(),
                1,
            )
            .map(Some)
        }

        fn read_neighbors(
            &mut self,
            node_id: HnswNodeId,
            layer: LayerIndex,
        ) -> GraphResult<Option<GraphNeighbors>> {
            if node_id != HnswNodeId::new(0) {
                return Ok(None);
            }
            if layer != LayerIndex::base() {
                return Err(GraphError::LayerNotFound { node_id, layer });
            }
            GraphNeighbors::new(1, node_id, layer, self.neighbors.to_vec()).map(Some)
        }
    }

    let (record, adjacency) = {
        let backing_vector = vector(&[1.0, 2.0]);
        let backing_neighbors = Vec::<HnswNodeId>::new();
        let mut adapter = BorrowedGraph {
            vector: &backing_vector,
            neighbors: &backing_neighbors,
        };
        let record = adapter
            .read_node(HnswNodeId::new(0))
            .expect("borrowed adapter read should succeed")
            .expect("borrowed adapter node should exist");
        let adjacency = adapter
            .read_neighbors(HnswNodeId::new(0), LayerIndex::base())
            .expect("borrowed adjacency read should succeed")
            .expect("borrowed adjacency should exist");
        (record, adjacency)
    };

    assert_eq!(record.vector().as_slice(), &[1.0, 2.0]);
    assert!(adjacency.neighbors().is_empty());
}

#[test]
fn graph_store_assigns_contiguous_ids_and_reports_missing_reads() {
    let mut graph = InMemoryGraphStore::new();
    assert_eq!(
        graph.metadata().expect("empty metadata should read"),
        GraphMetadata::empty()
    );
    assert_eq!(
        graph
            .read_node(HnswNodeId::new(99))
            .expect("missing reads are not adapter failures"),
        None
    );
    assert_eq!(
        graph
            .read_neighbors(HnswNodeId::new(99), LayerIndex::base())
            .expect("missing adjacency reads are not adapter failures"),
        None
    );

    assert_eq!(
        append(&mut graph, 10, &[0.0], vec![vec![]]),
        HnswNodeId::new(0)
    );
    assert_eq!(
        append(&mut graph, 20, &[0.0], vec![vec![]]),
        HnswNodeId::new(1)
    );
}

#[test]
fn graph_store_rejects_oversized_topology_without_mutating() {
    let mut graph = InMemoryGraphStore::new();
    let first = append(&mut graph, 10, &[0.0, 0.0], vec![vec![]]);
    let before = graph.clone();

    let too_many_layers = vec![Vec::new(); MAX_GRAPH_LAYERS + 1];
    assert_eq!(
        graph.append_node(NewGraphNode::new(
            GraphRecordId::new(20),
            HnswPointId::new(20),
            vector(&[1.0, 0.0]),
            too_many_layers,
        )),
        Err(GraphError::TooManyLayers {
            node_id: HnswNodeId::new(1),
            maximum: MAX_GRAPH_LAYERS,
            actual: MAX_GRAPH_LAYERS + 1,
        })
    );
    assert_eq!(graph, before);

    let too_many_neighbors = vec![first; MAX_GRAPH_NEIGHBORS_PER_LAYER + 1];
    assert_eq!(
        graph.replace_neighbors(first, LayerIndex::base(), too_many_neighbors),
        Err(GraphError::TooManyNeighbors {
            node_id: first,
            layer: LayerIndex::base(),
            maximum: MAX_GRAPH_NEIGHBORS_PER_LAYER,
            actual: MAX_GRAPH_NEIGHBORS_PER_LAYER + 1,
        })
    );
    assert_eq!(graph, before);

    let out_of_bounds = LayerIndex::new(MAX_GRAPH_LAYERS);
    assert_eq!(
        GraphNeighbors::new(1, first, out_of_bounds, vec![]),
        Err(GraphError::LayerOutOfBounds {
            layer: out_of_bounds,
            maximum: MAX_GRAPH_LAYERS - 1,
        })
    );
    assert_eq!(
        graph.read_neighbors(first, out_of_bounds),
        Err(GraphError::LayerOutOfBounds {
            layer: out_of_bounds,
            maximum: MAX_GRAPH_LAYERS - 1,
        })
    );
    assert_eq!(
        graph.replace_neighbors(first, out_of_bounds, vec![]),
        Err(GraphError::LayerOutOfBounds {
            layer: out_of_bounds,
            maximum: MAX_GRAPH_LAYERS - 1,
        })
    );
    assert_eq!(graph, before);
}

#[test]
fn graph_store_rejects_invalid_appends_without_mutating() {
    let mut graph = InMemoryGraphStore::new();
    let first = append(&mut graph, 10, &[0.0, 0.0], vec![vec![]]);
    let before = graph.clone();

    assert_eq!(
        graph.read_neighbors(first, LayerIndex::new(1)),
        Err(GraphError::LayerNotFound {
            node_id: first,
            layer: LayerIndex::new(1),
        })
    );
    assert_eq!(graph, before);

    let cases = [
        (
            NewGraphNode::new(
                GraphRecordId::new(20),
                HnswPointId::new(20),
                vector(&[1.0]),
                vec![vec![]],
            ),
            GraphError::DimensionMismatch {
                expected: 2,
                actual: 1,
            },
        ),
        (
            NewGraphNode::new(
                GraphRecordId::new(20),
                HnswPointId::new(20),
                vector(&[1.0, 0.0]),
                vec![vec![HnswNodeId::new(1)]],
            ),
            GraphError::SelfNeighbor {
                node_id: HnswNodeId::new(1),
            },
        ),
        (
            NewGraphNode::new(
                GraphRecordId::new(20),
                HnswPointId::new(20),
                vector(&[1.0, 0.0]),
                vec![vec![first, first]],
            ),
            GraphError::DuplicateNeighbor {
                node_id: HnswNodeId::new(1),
                neighbor_id: first,
                layer: LayerIndex::base(),
            },
        ),
        (
            NewGraphNode::new(
                GraphRecordId::new(20),
                HnswPointId::new(20),
                vector(&[1.0, 0.0]),
                vec![vec![HnswNodeId::new(99)]],
            ),
            GraphError::NeighborNotFound {
                node_id: HnswNodeId::new(1),
                neighbor_id: HnswNodeId::new(99),
                layer: LayerIndex::base(),
            },
        ),
    ];

    for (node, expected) in cases {
        assert_eq!(graph.append_node(node), Err(expected));
        assert_eq!(graph, before);
    }
}

#[test]
fn graph_store_rejects_invalid_rewires_and_publication_without_mutating() {
    let mut graph = InMemoryGraphStore::new();
    let first = append(&mut graph, 10, &[0.0, 0.0], vec![vec![]]);
    let second = append(&mut graph, 20, &[1.0, 0.0], vec![vec![first]]);
    let before = graph.clone();

    assert_eq!(
        graph.replace_neighbors(first, LayerIndex::new(1), vec![second]),
        Err(GraphError::LayerNotFound {
            node_id: first,
            layer: LayerIndex::new(1),
        })
    );
    assert_eq!(graph, before);

    assert_eq!(
        graph.replace_neighbors(first, LayerIndex::base(), vec![first]),
        Err(GraphError::SelfNeighbor { node_id: first })
    );
    assert_eq!(graph, before);

    assert_eq!(
        graph.publish_entry_point(Some(HnswNodeId::new(99))),
        Err(GraphError::EntryPointNotFound {
            node_id: HnswNodeId::new(99),
        })
    );
    assert_eq!(graph, before);
}

#[test]
fn generic_port_consumer_observes_the_same_owned_contract() {
    fn read_record(graph: &mut impl GraphRead, node_id: HnswNodeId) -> GraphNodeRecord {
        graph
            .read_node(node_id)
            .expect("generic port read should succeed")
            .expect("generic port node should exist")
    }

    let mut graph = InMemoryGraphStore::new();
    let node_id = append(&mut graph, 42, &[1.0, 2.0], vec![vec![]]);
    assert_eq!(
        read_record(&mut graph, node_id).record_id(),
        GraphRecordId::new(42)
    );
}
