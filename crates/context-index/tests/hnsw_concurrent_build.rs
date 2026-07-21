//! Parallel HNSW bulk-build correctness and quality tests.

#![allow(
    clippy::expect_used,
    reason = "test fixtures expect on operations the test itself just set up"
)]
#![allow(
    clippy::cast_precision_loss,
    reason = "seeded fixture values and recall ratios stay far below the f32/f64 mantissa limits"
)]

use context_core::{DenseVector, DistanceMetric, ExactSearchItem, SearchLimit, exact_top_k};
use context_index::{
    ConcurrentHnswBuilder, HnswConfig, HnswError, HnswGraph, HnswGraphSnapshot, HnswPointId,
};

fn vector(seed: u64, dimension: usize) -> context_index::Result<DenseVector> {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(1);
    let values = (0..dimension)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % 1000) as f32 / 1000.0
        })
        .collect::<Vec<_>>();
    DenseVector::new(values).map_err(HnswError::from)
}

fn fixture_points(count: u64, dimension: usize) -> context_index::Result<Vec<(u64, DenseVector)>> {
    (0..count)
        .map(|index| vector(index, dimension).map(|vector| (index, vector)))
        .collect()
}

fn build_concurrently(
    metric: DistanceMetric,
    config: HnswConfig,
    points: &[(u64, DenseVector)],
    workers: usize,
) -> context_index::Result<HnswGraph> {
    let builder = ConcurrentHnswBuilder::new(metric, config, points.len());
    std::thread::scope(|scope| {
        for chunk in points.chunks(points.len().div_ceil(workers)) {
            let builder = &builder;
            scope.spawn(move || {
                for (point_id, vector) in chunk {
                    builder
                        .insert(HnswPointId::new(*point_id), vector.clone())
                        .expect("concurrent insert should succeed for a fresh point id");
                }
            });
        }
    });
    builder.finish()
}

#[test]
fn concurrent_build_produces_a_structurally_valid_graph() -> context_index::Result<()> {
    let metric = DistanceMetric::L2;
    let config = HnswConfig::new(8, 32, 32)?;
    let points = fixture_points(300, 6)?;

    // `finish` itself revalidates every structural invariant through
    // `HnswGraph::from_snapshot` (contiguous node ids, unique point ids,
    // in-range neighbor links, a valid entry point, reciprocal edges, and
    // induced-layer connectivity), so a successful build already proves
    // structural validity; the snapshot round-trip below re-proves it on
    // the finished graph.
    let graph = build_concurrently(metric, config, &points, 4)?;
    assert_eq!(graph.len(), points.len());

    let snapshot: HnswGraphSnapshot = graph.snapshot();
    let reloaded = HnswGraph::from_snapshot(metric, config, snapshot)?;
    assert_eq!(reloaded.len(), points.len());

    Ok(())
}

#[test]
fn concurrent_build_survives_a_many_worker_stress_run() -> context_index::Result<()> {
    // Small vectors + many workers + repeated rounds maximize plan/commit
    // and prune/reciprocal-add interleavings — the schedules that produce
    // stale-`selected` asymmetric edges and would fail `finish`'s
    // reciprocity validation without the repair pass.
    let metric = DistanceMetric::L2;
    let config = HnswConfig::new(4, 16, 16)?;
    for round in 0..8 {
        let points = fixture_points(200 + round * 17, 3)?;
        let graph = build_concurrently(metric, config, &points, 8)?;
        assert_eq!(graph.len(), points.len());
    }
    Ok(())
}

#[test]
fn concurrent_build_reaches_reasonable_recall_against_exact_search() -> context_index::Result<()> {
    let metric = DistanceMetric::L2;
    let config = HnswConfig::new(16, 64, 64)?;
    let dimension = 8;
    let points = fixture_points(500, dimension)?;

    let graph = build_concurrently(metric, config, &points, 4)?;

    let exact_items = points
        .iter()
        .map(|(point_id, vector)| ExactSearchItem::new(*point_id, vector.clone()))
        .collect::<Vec<_>>();
    let limit = SearchLimit::new(10).map_err(HnswError::from)?;

    let mut total_overlap = 0usize;
    let mut total_expected = 0usize;
    for query_seed in 500..520 {
        let query = vector(query_seed, dimension)?;
        let exact = exact_top_k(&query, &exact_items, metric, limit)
            .collect::<context_core::Result<Vec<_>>>()
            .map_err(HnswError::from)?;
        let approx = graph.search(&query, limit)?;
        let approx_ids = approx
            .iter()
            .map(|result| result.point_id().get())
            .collect::<Vec<_>>();
        total_overlap += exact
            .iter()
            .filter(|item| approx_ids.contains(&item.point_id()))
            .count();
        total_expected += exact.len();
    }

    let recall = total_overlap as f64 / total_expected as f64;
    assert!(
        recall >= 0.8,
        "concurrent build recall too low: {recall} ({total_overlap}/{total_expected})"
    );
    Ok(())
}

#[test]
fn concurrent_build_matches_sequential_build_for_a_single_thread() -> context_index::Result<()> {
    let metric = DistanceMetric::L2;
    let config = HnswConfig::new(8, 32, 32)?;
    let points = fixture_points(120, 5)?;

    let mut sequential = HnswGraph::new(metric, config);
    for (point_id, vector) in &points {
        sequential.insert(HnswPointId::new(*point_id), vector.clone())?;
    }

    let builder = ConcurrentHnswBuilder::new(metric, config, points.len());
    for (point_id, vector) in &points {
        builder.insert(HnswPointId::new(*point_id), vector.clone())?;
    }
    let concurrent = builder.finish()?;

    // A single "worker" (no actual concurrency, insertion order preserved)
    // must reproduce the exact sequential graph shape: the per-node-locked
    // wiring must be a pure reimplementation of the single-threaded
    // algorithm.
    assert_eq!(concurrent.snapshot(), sequential.snapshot());
    Ok(())
}

#[test]
fn concurrent_build_rejects_duplicates_capacity_overflow_and_dimension_drift()
-> context_index::Result<()> {
    let metric = DistanceMetric::L2;
    let config = HnswConfig::new(4, 16, 16)?;
    let builder = ConcurrentHnswBuilder::new(metric, config, 2);

    builder.insert(HnswPointId::new(1), vector(1, 4)?)?;
    assert!(matches!(
        builder.insert(HnswPointId::new(1), vector(2, 4)?),
        Err(HnswError::DuplicatePointId { .. })
    ));
    assert!(matches!(
        builder.insert(HnswPointId::new(2), vector(2, 3)?),
        Err(HnswError::DimensionMismatch { .. })
    ));
    builder.insert(HnswPointId::new(2), vector(2, 4)?)?;
    assert!(matches!(
        builder.insert(HnswPointId::new(3), vector(3, 4)?),
        Err(HnswError::InvalidParameter { .. })
    ));

    let graph = builder.finish()?;
    assert_eq!(graph.len(), 2);
    Ok(())
}
