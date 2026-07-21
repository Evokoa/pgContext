//! HNSW recall tests against exact search.

use context_core::{DenseVector, DistanceMetric, ExactSearchItem, SearchLimit, exact_top_k};
use context_index::{CandidateMask, HnswConfig, HnswError, HnswGraph, HnswPointId, NeverCancel};

fn vector(values: &[f32]) -> context_index::Result<DenseVector> {
    DenseVector::new(values.to_vec()).map_err(HnswError::from)
}

#[test]
fn hnsw_search_matches_exact_top_k_for_fixed_fixture() -> context_index::Result<()> {
    let metric = DistanceMetric::L2;
    let mut graph = HnswGraph::new(metric, HnswConfig::new(4, 16, 16)?);
    let fixtures = [
        (10, [0.0, 0.0]),
        (20, [1.0, 0.0]),
        (30, [0.0, 1.0]),
        (40, [3.0, 0.0]),
        (50, [0.0, 3.0]),
        (60, [2.0, 2.0]),
    ];
    let mut exact_items = Vec::new();
    for (point_id, values) in fixtures {
        let vector = vector(&values)?;
        graph.insert(HnswPointId::new(point_id), vector.clone())?;
        exact_items.push(ExactSearchItem::new(point_id, vector));
    }

    let query = vector(&[0.2, 0.1])?;
    let limit = SearchLimit::new(3).map_err(HnswError::from)?;
    let exact = exact_top_k(&query, &exact_items, metric, limit)
        .collect::<context_core::Result<Vec<_>>>()
        .map_err(HnswError::from)?;
    let hnsw = graph.search(&query, limit)?;

    assert_eq!(
        hnsw.iter()
            .map(|point| point.point_id().get())
            .collect::<Vec<_>>(),
        exact
            .iter()
            .map(context_core::ScoredPoint::point_id)
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn hnsw_search_returns_empty_results_for_empty_index() -> context_index::Result<()> {
    let graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(4, 16, 16)?);

    let results = graph.search(
        &vector(&[0.0, 0.0])?,
        SearchLimit::new(3).map_err(HnswError::from)?,
    )?;

    assert_eq!(results, Vec::new());
    Ok(())
}

#[test]
fn controlled_hierarchical_search_matches_exact_and_reports_bounded_work()
-> context_index::Result<()> {
    let metric = DistanceMetric::L2;
    let config = HnswConfig::new(4, 16, 8)?;
    let mut graph = HnswGraph::new(metric, config);
    let mut exact_items = Vec::new();
    for point_id in 0_u8..48 {
        let item = vector(&[f32::from(point_id % 9), f32::from(point_id / 9)])?;
        let point_id = u64::from(point_id + 1);
        graph.insert(HnswPointId::new(point_id), item.clone())?;
        exact_items.push(ExactSearchItem::new(point_id, item));
    }
    let query = vector(&[3.0, 2.0])?;
    let limit = SearchLimit::new(3).map_err(HnswError::from)?;
    let exact = exact_top_k(&query, &exact_items, metric, limit)
        .collect::<context_core::Result<Vec<_>>>()
        .map_err(HnswError::from)?;
    let outcome =
        graph.search_with_control(&query, limit, &CandidateMask::all(), &mut NeverCancel)?;
    let layers = graph.max_level().map_or(1, |level| level.layer_count());
    let per_layer_bound =
        1 + config.ef_search() * config.max_connections(context_index::LayerIndex::new(0));

    assert_eq!(
        outcome
            .results()
            .iter()
            .map(|point| point.point_id().get())
            .collect::<Vec<_>>(),
        exact
            .iter()
            .map(context_core::ScoredPoint::point_id)
            .collect::<Vec<_>>(),
    );
    assert!(outcome.work().node_expansions() <= layers * config.ef_search());
    assert!(outcome.work().edges_examined() <= layers * per_layer_bound);
    assert!(outcome.work().distance_evaluations() <= layers * per_layer_bound);
    assert!(outcome.work().cancellation_checks() >= 1);
    Ok(())
}
