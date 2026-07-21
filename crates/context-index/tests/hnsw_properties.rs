//! Hierarchical HNSW invariant and state-machine properties.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::{BTreeSet, VecDeque};

use context_core::{DenseVector, DistanceMetric, SearchLimit};
use context_index::{
    CandidateMask, HnswCancellation, HnswConfig, HnswError, HnswGraph, HnswLevel, HnswLevelSeed,
    HnswNodeId, HnswPointId, LayerIndex, MAX_GRAPH_LAYERS,
};
use proptest::prelude::*;

fn vector(x: i16, y: i16) -> context_index::Result<DenseVector> {
    DenseVector::new(vec![f32::from(x), f32::from(y)]).map_err(HnswError::from)
}

fn assert_hierarchy(graph: &HnswGraph, config: HnswConfig, expected_levels: &[HnswLevel]) {
    let entry = graph
        .entry_point()
        .expect("a nonempty graph has an entry point");
    let maximum = expected_levels
        .iter()
        .copied()
        .max()
        .expect("the level list is nonempty");
    assert_eq!(graph.node_level(entry), Some(maximum));

    for (node_index, expected_level) in expected_levels.iter().copied().enumerate() {
        let node_id = HnswNodeId::new(node_index);
        assert_eq!(graph.node_level(node_id), Some(expected_level));
        assert_eq!(
            graph.layer_count(node_id),
            Some(expected_level.layer_count())
        );

        for layer_index in 0..expected_level.layer_count() {
            let layer = LayerIndex::new(layer_index);
            let neighbors = graph
                .neighbors(node_id, layer)
                .expect("a participating node owns every lower layer");
            assert!(neighbors.len() <= config.max_connections(layer));
            assert_eq!(
                neighbors.iter().copied().collect::<BTreeSet<_>>().len(),
                neighbors.len(),
                "neighbors are unique"
            );
            for neighbor in neighbors {
                assert_ne!(*neighbor, node_id);
                assert!(neighbor.get() < graph.len());
                assert!(
                    graph
                        .node_level(*neighbor)
                        .is_some_and(|level| level.get() >= layer_index),
                    "both endpoints participate in the layer"
                );
                assert!(
                    graph
                        .neighbors(*neighbor, layer)
                        .is_some_and(|reverse| reverse.contains(&node_id)),
                    "links are bidirectional"
                );
            }
        }
    }

    for layer_index in 0..=maximum.get() {
        let layer = LayerIndex::new(layer_index);
        let participants = expected_levels
            .iter()
            .enumerate()
            .filter_map(|(node, level)| {
                (level.get() >= layer_index).then_some(HnswNodeId::new(node))
            })
            .collect::<BTreeSet<_>>();
        let layer_entry = participants
            .iter()
            .copied()
            .find(|node| graph.node_level(*node) == Some(maximum))
            .or_else(|| participants.iter().next().copied())
            .expect("every checked layer has a participant");
        let mut reached = BTreeSet::new();
        let mut pending = VecDeque::from([layer_entry]);
        while let Some(node) = pending.pop_front() {
            if !reached.insert(node) {
                continue;
            }
            pending.extend(
                graph
                    .neighbors(node, layer)
                    .unwrap_or_default()
                    .iter()
                    .copied(),
            );
        }
        assert_eq!(reached, participants, "each induced layer stays connected");
    }
}

proptest! {
    #[test]
    fn arbitrary_bounded_levels_preserve_hierarchy_connectivity_and_work_bounds(
        inputs in prop::collection::vec((0_i16..50, 0_i16..50, 0_usize..6), 1..48)
    ) {
        let config = HnswConfig::new(4, 16, 12);
        prop_assert!(config.is_ok());
        let Ok(config) = config else {
            return Ok(());
        };
        let mut graph = HnswGraph::with_level_seed(
            DistanceMetric::L2,
            config,
            HnswLevelSeed::new(0x51a7_e5ed),
        );
        let mut levels = Vec::with_capacity(inputs.len());

        for (point_index, (x, y, raw_level)) in inputs.into_iter().enumerate() {
            let level = HnswLevel::new(raw_level);
            prop_assert!(level.is_ok());
            let Ok(level) = level else {
                return Ok(());
            };
            let item = vector(x, y);
            prop_assert!(item.is_ok());
            let Ok(item) = item else {
                return Ok(());
            };
            let outcome = graph.insert_at_level(
                HnswPointId::new(point_index as u64 + 1),
                item,
                level,
            );
            prop_assert!(outcome.is_ok());
            let Ok(outcome) = outcome else {
                return Ok(());
            };
            levels.push(level);

            let layer_factor = graph.max_level().map_or(1, HnswLevel::layer_count);
            let work_bound = layer_factor
                .saturating_mul(config.ef_construction())
                .saturating_mul(config.max_connections(LayerIndex::new(0)).saturating_add(1));
            prop_assert!(outcome.work().distance_evaluations() <= work_bound);
            prop_assert!(outcome.work().node_expansions() <= work_bound);
            prop_assert!(outcome.work().edges_examined() <= work_bound);
            assert_hierarchy(&graph, config, &levels);
        }
    }
}

proptest! {
    #[test]
    fn arbitrary_masks_exclude_logically_deleted_and_missing_points_without_mutation(
        raw_allowed in prop::collection::vec(0_u64..64, 0..64),
        query_x in -16_i16..48,
    ) {
        let config = HnswConfig::new(4, 16, 12);
        prop_assert!(config.is_ok());
        let Ok(config) = config else {
            return Ok(());
        };
        let mut graph = HnswGraph::new(DistanceMetric::L2, config);
        for (point_id, coordinate) in (1_u64..=32).zip(0_i16..32) {
            let inserted = graph.insert(HnswPointId::new(point_id), vector(coordinate, 0)?);
            prop_assert!(inserted.is_ok());
        }
        let before = graph.snapshot();
        let allowed = raw_allowed
            .iter()
            .map(|raw| HnswPointId::new(raw + 1))
            .collect::<BTreeSet<_>>();
        let mask = CandidateMask::only(allowed.iter().copied());
        let limit = SearchLimit::new(8).map_err(HnswError::from)?;
        let results = graph.search_with_mask(&vector(query_x, 0)?, limit, &mask)?;

        prop_assert!(results.iter().all(|result| allowed.contains(&result.point_id())));
        prop_assert_eq!(graph.snapshot(), before);
    }
}

#[test]
fn seeded_levels_and_full_snapshots_are_deterministic_without_pinning_one_layout()
-> context_index::Result<()> {
    let config = HnswConfig::new(4, 24, 16)?;
    let seed = HnswLevelSeed::new(0xd37e_5eed);
    let mut first = HnswGraph::with_level_seed(DistanceMetric::L2, config, seed);
    let mut second = HnswGraph::with_level_seed(DistanceMetric::L2, config, seed);

    for point in 0_u64..96 {
        let item = vector((point % 17) as i16, (point % 11) as i16)?;
        first.insert(HnswPointId::new(point + 1), item.clone())?;
        second.insert(HnswPointId::new(point + 1), item)?;
    }

    assert_eq!(first.snapshot(), second.snapshot());
    assert!(first.max_level().is_some_and(|level| level.get() > 0));
    assert!(
        first
            .max_level()
            .is_some_and(|level| level.get() < MAX_GRAPH_LAYERS)
    );

    let encoded = first.snapshot().to_bytes()?;
    let decoded = context_index::HnswGraphSnapshot::from_bytes(&encoded)?;
    let restored = HnswGraph::from_snapshot(DistanceMetric::L2, config, decoded)?;
    assert_eq!(restored.snapshot(), first.snapshot());
    Ok(())
}

#[derive(Debug)]
struct CancelAt {
    remaining: usize,
}

impl HnswCancellation for CancelAt {
    fn check(&mut self) -> context_index::Result<()> {
        if self.remaining == 0 {
            return Err(HnswError::Cancelled);
        }
        self.remaining -= 1;
        Ok(())
    }
}

#[test]
fn reusable_masks_and_cancellation_are_bounded_and_leave_graph_state_unchanged()
-> context_index::Result<()> {
    let config = HnswConfig::new(4, 16, 8)?;
    let mut graph = HnswGraph::new(DistanceMetric::L2, config);
    for (point_id, coordinate) in (1_u64..=32).zip(0_i16..32) {
        graph.insert(HnswPointId::new(point_id), vector(coordinate, 0)?)?;
    }
    let before = graph.snapshot();
    let mask = CandidateMask::only([
        HnswPointId::new(2),
        HnswPointId::new(17),
        HnswPointId::new(10_000),
    ]);
    let query = vector(0, 0)?;
    let limit = SearchLimit::new(8).map_err(HnswError::from)?;

    for checkpoint in 0..6 {
        let result = graph.search_with_control(
            &query,
            limit,
            &mask,
            &mut CancelAt {
                remaining: checkpoint,
            },
        );
        if let Ok(outcome) = result {
            assert!(outcome.work().cancellation_checks() <= checkpoint + 1);
            assert!(
                outcome
                    .results()
                    .iter()
                    .all(|result| matches!(result.point_id().get(), 2 | 17))
            );
        } else {
            assert_eq!(result, Err(HnswError::Cancelled));
        }
        assert_eq!(graph.snapshot(), before);
    }

    let first = graph.search_with_mask(&query, limit, &mask)?;
    let second = graph.search_with_mask(&query, limit, &mask)?;
    assert_eq!(first, second);
    assert_eq!(graph.snapshot(), before);
    Ok(())
}

#[test]
fn insertion_cancellation_is_atomic_at_every_reached_checkpoint() -> context_index::Result<()> {
    let config = HnswConfig::new(4, 16, 8)?;
    let mut observed_cancellation = false;
    let mut observed_success = false;

    for checkpoint in 0..128 {
        let mut graph = HnswGraph::new(DistanceMetric::L2, config);
        for (point_id, coordinate) in (1_u64..=24).zip(0_i16..24) {
            graph.insert(HnswPointId::new(point_id), vector(coordinate, 0)?)?;
        }
        let before = graph.snapshot();
        let result = graph.insert_at_level_with_control(
            HnswPointId::new(25),
            vector(25, 0)?,
            HnswLevel::new(3)?,
            &mut CancelAt {
                remaining: checkpoint,
            },
        );

        match result {
            Err(HnswError::Cancelled) => {
                observed_cancellation = true;
                assert_eq!(graph.snapshot(), before);
            }
            Ok(outcome) => {
                observed_success = true;
                assert_eq!(outcome.node_id(), HnswNodeId::new(24));
                break;
            }
            Err(error) => return Err(error),
        }
    }

    assert!(observed_cancellation);
    assert!(observed_success);
    Ok(())
}

#[test]
fn cancelled_first_insertion_does_not_establish_a_hidden_dimension() -> context_index::Result<()> {
    let config = HnswConfig::new(4, 16, 8)?;
    let mut graph = HnswGraph::new(DistanceMetric::L2, config);
    let before = graph.snapshot();

    assert_eq!(
        graph.insert_at_level_with_control(
            HnswPointId::new(1),
            vector(1, 2)?,
            HnswLevel::base(),
            &mut CancelAt { remaining: 0 },
        ),
        Err(HnswError::Cancelled)
    );
    assert_eq!(graph.snapshot(), before);

    graph.insert_at_level(
        HnswPointId::new(2),
        DenseVector::new(vec![1.0, 2.0, 3.0]).map_err(HnswError::from)?,
        HnswLevel::base(),
    )?;
    Ok(())
}

#[test]
fn duplicate_point_id_failure_is_atomic_while_duplicate_vectors_remain_supported()
-> context_index::Result<()> {
    let config = HnswConfig::new(3, 12, 8)?;
    let mut graph = HnswGraph::new(DistanceMetric::L2, config);
    graph.insert(HnswPointId::new(7), vector(1, 1)?)?;
    graph.insert(HnswPointId::new(8), vector(1, 1)?)?;
    let before = graph.snapshot();

    assert_eq!(
        graph.insert(HnswPointId::new(7), vector(2, 2)?),
        Err(HnswError::DuplicatePointId {
            point_id: HnswPointId::new(7),
        })
    );
    assert_eq!(graph.snapshot(), before);
    Ok(())
}
