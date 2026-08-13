//! Differential and hard-budget contracts for the pure lazy HNSW cursor.

#![allow(clippy::expect_used)]

use std::{collections::BTreeSet, mem::size_of};

use context_core::{DenseVector, DistanceMetric, SearchLimit};
use context_index::{
    CandidateMask, GraphMetadata, GraphNeighbors, GraphRead, GraphRecordId, GraphResult,
    GraphWrite, HnswCancellation, HnswComparisonBudget, HnswConfig, HnswCursorBudget,
    HnswCursorFrontier, HnswCursorTermination, HnswError, HnswNodeId, HnswPointId,
    InMemoryGraphStore, LayerIndex, NeverCancel, NewGraphNode,
    projected_hnsw_cursor_retained_bytes, search_graph_read, search_graph_read_with_mask,
    seed_graph_read_cursor, seed_graph_read_cursor_with_mask,
};

fn vector(values: &[f32]) -> DenseVector {
    DenseVector::new(values.to_vec()).expect("cursor vector fixture should be valid")
}

fn chain_graph(points: usize) -> InMemoryGraphStore {
    let mut graph = InMemoryGraphStore::new();
    let mut nodes = Vec::with_capacity(points);
    for index in 0..points {
        let coordinate =
            f32::from(u16::try_from(index + 1).expect("cursor fixture coordinate should fit"));
        let previous = nodes.last().copied().into_iter().collect();
        let node = graph
            .append_node(NewGraphNode::new(
                GraphRecordId::new(index as u64),
                HnswPointId::new(1_000 + index as u64),
                vector(&[coordinate, 0.0]),
                vec![previous],
            ))
            .expect("cursor graph append should succeed");
        if let Some(&prior) = nodes.last() {
            let prior_neighbors = if prior.get() == 0 {
                vec![node]
            } else {
                vec![HnswNodeId::new(prior.get() - 1), node]
            };
            graph
                .replace_neighbors(prior, LayerIndex::base(), prior_neighbors)
                .expect("cursor graph reciprocal link should succeed");
        }
        nodes.push(node);
    }
    graph
        .publish_entry_point(nodes.last().copied())
        .expect("cursor entry point should publish");
    graph
}

fn binary_chain_graph() -> InMemoryGraphStore {
    let mut graph = InMemoryGraphStore::new();
    let mut nodes = Vec::new();
    for (index, values) in [[1.0, 0.0], [0.0, 1.0], [1.0, 1.0], [0.0, 0.0]]
        .into_iter()
        .enumerate()
    {
        let previous = nodes.last().copied().into_iter().collect();
        let node = graph
            .append_node(NewGraphNode::new(
                GraphRecordId::new(index as u64),
                HnswPointId::new(2_000 + index as u64),
                vector(&values),
                vec![previous],
            ))
            .expect("binary cursor graph append should succeed");
        if let Some(&prior) = nodes.last() {
            let prior_neighbors = if prior.get() == 0 {
                vec![node]
            } else {
                vec![HnswNodeId::new(prior.get() - 1), node]
            };
            graph
                .replace_neighbors(prior, LayerIndex::base(), prior_neighbors)
                .expect("binary cursor graph reciprocal link should succeed");
        }
        nodes.push(node);
    }
    graph
        .publish_entry_point(nodes.last().copied())
        .expect("binary cursor entry point should publish");
    graph
}

#[test]
fn cursor_matches_eager_metrics_ties_and_masks() {
    let query = vector(&[1.0, 0.0]);
    let metrics = [
        DistanceMetric::L2,
        DistanceMetric::NegativeInnerProduct,
        DistanceMetric::Cosine,
        DistanceMetric::L1,
    ];
    for metric in metrics {
        let eager = search_graph_read(
            &mut chain_graph(8),
            metric,
            &query,
            config(),
            limit(),
            &mut NeverCancel,
        )
        .expect("eager metric traversal should succeed");
        let mut graph = chain_graph(8);
        let comparisons = HnswComparisonBudget::new(usize::MAX);
        let lazy = seed_graph_read_cursor(
            &mut graph,
            metric,
            &query,
            config(),
            limit(),
            &comparisons,
            HnswCursorBudget::unlimited(),
            &mut NeverCancel,
        )
        .and_then(|cursor| cursor.finish())
        .expect("lazy metric traversal should succeed");
        assert_eq!(lazy, eager, "metric {metric:?} diverged");
    }
    for metric in [DistanceMetric::Hamming, DistanceMetric::Jaccard] {
        let eager = search_graph_read(
            &mut binary_chain_graph(),
            metric,
            &query,
            config(),
            limit(),
            &mut NeverCancel,
        )
        .expect("eager binary traversal should succeed");
        let mut graph = binary_chain_graph();
        let comparisons = HnswComparisonBudget::new(usize::MAX);
        let lazy = seed_graph_read_cursor(
            &mut graph,
            metric,
            &query,
            config(),
            limit(),
            &comparisons,
            HnswCursorBudget::unlimited(),
            &mut NeverCancel,
        )
        .and_then(|cursor| cursor.finish())
        .expect("lazy binary traversal should succeed");
        assert_eq!(lazy, eager, "binary metric {metric:?} diverged");
    }

    let mask = CandidateMask::only([
        HnswPointId::new(1_001),
        HnswPointId::new(1_003),
        HnswPointId::new(1_006),
    ]);
    let eager = search_graph_read_with_mask(
        &mut chain_graph(8),
        DistanceMetric::L2,
        &query,
        config(),
        limit(),
        &mask,
        &mut NeverCancel,
    )
    .expect("masked eager traversal should succeed");
    for batch_size in [1, 31, 32, 255, 256] {
        let mut graph = chain_graph(8);
        let comparisons = HnswComparisonBudget::new(usize::MAX);
        let mut cancellation = NeverCancel;
        let mut cursor = seed_graph_read_cursor_with_mask(
            &mut graph,
            DistanceMetric::L2,
            &query,
            config(),
            limit(),
            &mask,
            3,
            &comparisons,
            HnswCursorBudget::unlimited(),
            &mut cancellation,
        )
        .expect("masked cursor seed should succeed");
        let initial = cursor.work();
        assert_eq!(cursor.peek(), cursor.peek());
        assert_eq!(cursor.work(), initial, "peek must not perform graph work");
        while cursor.termination().is_none() {
            cursor
                .advance(batch_size)
                .expect("masked cursor advance should succeed");
        }
        let lazy = cursor.finish().expect("masked cursor should finish");
        assert_eq!(lazy, eager);
    }
}

fn config() -> HnswConfig {
    HnswConfig::new(2, 8, 8).expect("cursor config should be valid")
}

fn limit() -> SearchLimit {
    SearchLimit::new(4).expect("cursor limit should be valid")
}

#[test]
fn arbitrary_pause_resume_batches_match_the_eager_wrapper() {
    let query = vector(&[0.0, 0.0]);
    let eager = search_graph_read(
        &mut chain_graph(8),
        DistanceMetric::L2,
        &query,
        config(),
        limit(),
        &mut NeverCancel,
    )
    .expect("eager traversal should succeed");

    for batch_size in [1, 2, 3, 7, 32, 256] {
        let mut graph = chain_graph(8);
        let comparison_budget = HnswComparisonBudget::new(usize::MAX);
        let mut cancellation = NeverCancel;
        let mut cursor = seed_graph_read_cursor(
            &mut graph,
            DistanceMetric::L2,
            &query,
            config(),
            limit(),
            &comparison_budget,
            HnswCursorBudget::unlimited(),
            &mut cancellation,
        )
        .expect("cursor seed should succeed");
        let mut popped = BTreeSet::new();
        while !cursor.exhausted() {
            if let Some(peeked) = cursor.peek() {
                let advanced = cursor
                    .advance(batch_size)
                    .expect("cursor advance should succeed");
                assert_eq!(advanced.frontiers().first().copied(), Some(peeked));
                for frontier in advanced.frontiers() {
                    assert!(popped.insert(frontier.node_id()));
                }
            }
        }
        assert_eq!(cursor.termination(), Some(HnswCursorTermination::Exhausted));
        let lazy = cursor.finish().expect("exhausted cursor should finish");
        assert_eq!(lazy.results(), eager.results());
        assert_eq!(lazy.work(), eager.work());
    }
}

#[test]
fn cursor_bounds_batch_expansion_memory_and_cancellation() {
    let query = vector(&[0.0, 0.0]);
    let comparison_budget = HnswComparisonBudget::new(usize::MAX);
    let mut graph = chain_graph(8);
    let mut cancellation = NeverCancel;
    let mut cursor = seed_graph_read_cursor(
        &mut graph,
        DistanceMetric::L2,
        &query,
        config(),
        limit(),
        &comparison_budget,
        HnswCursorBudget::new(1, usize::MAX, usize::MAX),
        &mut cancellation,
    )
    .expect("one expansion should seed");
    assert!(matches!(
        cursor.advance(0),
        Err(HnswError::InvalidParameter {
            parameter: "cursor_batch",
            value: 0
        })
    ));
    cursor
        .advance(256)
        .expect("the admitted expansion should run");
    assert_eq!(
        cursor.termination(),
        Some(HnswCursorTermination::ExpansionBudget)
    );
    assert!(!cursor.exhausted());

    let mut graph = chain_graph(8);
    let mut cancellation = NeverCancel;
    let retained_bytes = seed_graph_read_cursor(
        &mut graph,
        DistanceMetric::L2,
        &query,
        config(),
        limit(),
        &comparison_budget,
        HnswCursorBudget::unlimited(),
        &mut cancellation,
    )
    .map(|cursor| cursor.retained_bytes())
    .expect("unlimited cursor should report retained bytes");
    let mut graph = chain_graph(8);
    let mut cancellation = NeverCancel;
    let mut memory_cursor = seed_graph_read_cursor(
        &mut graph,
        DistanceMetric::L2,
        &query,
        config(),
        limit(),
        &comparison_budget,
        HnswCursorBudget::new(usize::MAX, usize::MAX, retained_bytes),
        &mut cancellation,
    )
    .expect("exact retained-state budget should seed");
    let memory_error = memory_cursor
        .advance(1)
        .expect_err("the returned frontier must be admitted before allocation");
    assert!(matches!(
        memory_error,
        HnswError::CursorMemoryBudgetExceeded { maximum, .. } if maximum == retained_bytes
    ));
    assert_eq!(
        memory_cursor.termination(),
        Some(HnswCursorTermination::MemoryBudget)
    );

    let mut graph = chain_graph(8);
    let comparisons = HnswComparisonBudget::new(2);
    let mut cancellation = NeverCancel;
    let mut comparison_cursor = seed_graph_read_cursor(
        &mut graph,
        DistanceMetric::L2,
        &query,
        config(),
        limit(),
        &comparisons,
        HnswCursorBudget::unlimited(),
        &mut cancellation,
    )
    .expect("two comparisons seed the nonempty base cursor");
    assert!(matches!(
        comparison_cursor.advance(1),
        Err(HnswError::ComparisonBudgetExceeded {
            maximum: 2,
            consumed: 2
        })
    ));
    assert_eq!(
        comparison_cursor.termination(),
        Some(HnswCursorTermination::ComparisonBudget)
    );

    let mut graph = chain_graph(8);
    let comparisons = HnswComparisonBudget::new(usize::MAX);
    let mut cancellation = NeverCancel;
    let mut edge_cursor = seed_graph_read_cursor(
        &mut graph,
        DistanceMetric::L2,
        &query,
        config(),
        limit(),
        &comparisons,
        HnswCursorBudget::new(usize::MAX, 0, usize::MAX),
        &mut cancellation,
    )
    .expect("zero edge budget still admits seed state");
    let edge_outcome = edge_cursor
        .advance(1)
        .expect("edge exhaustion is a visible terminal outcome");
    assert_eq!(
        edge_outcome.termination(),
        Some(HnswCursorTermination::EdgeBudget)
    );
    assert_eq!(edge_cursor.work().edges_examined(), 0);

    struct CancelOnThird {
        checks: usize,
    }
    impl HnswCancellation for CancelOnThird {
        fn check(&mut self) -> context_index::Result<()> {
            self.checks += 1;
            if self.checks >= 3 {
                Err(HnswError::Cancelled)
            } else {
                Ok(())
            }
        }
    }
    let mut graph = chain_graph(8);
    let mut cancellation = CancelOnThird { checks: 0 };
    let mut cancelled_cursor = seed_graph_read_cursor(
        &mut graph,
        DistanceMetric::L2,
        &query,
        config(),
        limit(),
        &comparison_budget,
        HnswCursorBudget::unlimited(),
        &mut cancellation,
    )
    .expect("the seed cancellation checkpoints permit initialization");
    let pending_before_cancel = cancelled_cursor.peek();
    assert_eq!(cancelled_cursor.advance(1), Err(HnswError::Cancelled));
    assert_eq!(cancelled_cursor.peek(), pending_before_cancel);
    assert_eq!(
        cancelled_cursor.termination(),
        Some(HnswCursorTermination::Cancelled)
    );
}

#[test]
fn seed_cancellation_precedes_every_graph_adapter_call() {
    struct CountingGraphRead {
        calls: usize,
    }

    impl GraphRead for CountingGraphRead {
        fn metadata(&mut self) -> GraphResult<GraphMetadata> {
            self.calls += 1;
            Ok(GraphMetadata::empty())
        }

        fn read_node(
            &mut self,
            _node_id: HnswNodeId,
        ) -> GraphResult<Option<context_index::GraphNodeRecord>> {
            self.calls += 1;
            Ok(None)
        }

        fn read_neighbors(
            &mut self,
            _node_id: HnswNodeId,
            _layer: LayerIndex,
        ) -> GraphResult<Option<GraphNeighbors>> {
            self.calls += 1;
            Ok(None)
        }
    }

    struct CancelImmediately;
    impl HnswCancellation for CancelImmediately {
        fn check(&mut self) -> context_index::Result<()> {
            Err(HnswError::Cancelled)
        }
    }

    let query = vector(&[0.0, 0.0]);
    let comparisons = HnswComparisonBudget::new(usize::MAX);
    let mut graph = CountingGraphRead { calls: 0 };
    assert!(matches!(
        seed_graph_read_cursor(
            &mut graph,
            DistanceMetric::L2,
            &query,
            config(),
            limit(),
            &comparisons,
            HnswCursorBudget::unlimited(),
            &mut CancelImmediately,
        ),
        Err(HnswError::Cancelled)
    ));
    assert_eq!(graph.calls, 0);
}

#[test]
fn exact_projected_memory_admits_full_heap_replacements() {
    let query = vector(&[0.0, 0.0]);
    let config = HnswConfig::new(2, 4, 4).expect("boundary config should be valid");
    let retained =
        projected_hnsw_cursor_retained_bytes(8, 4, false).expect("retained projection should fit");
    let required = retained + size_of::<HnswCursorFrontier>();
    let comparisons = HnswComparisonBudget::new(usize::MAX);
    let mut graph = chain_graph(8);
    let mut cancellation = NeverCancel;
    let mut cursor = seed_graph_read_cursor(
        &mut graph,
        DistanceMetric::L2,
        &query,
        config,
        limit(),
        &comparisons,
        HnswCursorBudget::new(usize::MAX, usize::MAX, required),
        &mut cancellation,
    )
    .expect("the exact complete-operation projection should admit seed");
    while cursor.termination().is_none() {
        cursor
            .advance(1)
            .expect("full-heap replacement must not grow past projection");
    }
    let outcome = cursor.finish().expect("the exact boundary should finish");
    assert_eq!(outcome.results().len(), 4);
    assert!(outcome.work().node_expansions() > 4);
}
