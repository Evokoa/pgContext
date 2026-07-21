//! HNSW candidate-mask behavior tests.

use context_core::{DenseVector, DistanceMetric, SearchLimit};
use context_index::{
    CandidateMask, CandidatePrefilter, CandidatePrefilterKind, HnswConfig, HnswError, HnswGraph,
    HnswPointId,
};

fn vector(values: &[f32]) -> context_index::Result<DenseVector> {
    DenseVector::new(values.to_vec()).map_err(HnswError::from)
}

#[test]
fn hnsw_search_candidate_mask_restricts_returned_points() -> context_index::Result<()> {
    let graph = fixture_graph()?;
    let query = vector(&[0.0, 0.0])?;
    let mask = CandidateMask::only([HnswPointId::new(30), HnswPointId::new(40)]);

    let results =
        graph.search_with_mask(&query, SearchLimit::new(3).map_err(HnswError::from)?, &mask)?;

    assert_eq!(
        results
            .iter()
            .map(|point| point.point_id())
            .collect::<Vec<_>>(),
        vec![HnswPointId::new(30), HnswPointId::new(40)]
    );
    Ok(())
}

#[test]
fn hnsw_search_candidate_mask_can_exclude_every_point() -> context_index::Result<()> {
    let graph = fixture_graph()?;
    let query = vector(&[0.0, 0.0])?;
    let mask = CandidateMask::only([]);

    let results =
        graph.search_with_mask(&query, SearchLimit::new(3).map_err(HnswError::from)?, &mask)?;

    assert_eq!(results, Vec::new());
    Ok(())
}

#[test]
fn hnsw_search_prefilter_uses_packed_bitmap_for_dense_points() -> context_index::Result<()> {
    let graph = fixture_graph()?;
    let query = vector(&[0.0, 0.0])?;
    let prefilter = CandidatePrefilter::from_points([
        HnswPointId::new(20),
        HnswPointId::new(30),
        HnswPointId::new(30),
        HnswPointId::new(40),
    ]);

    assert_eq!(prefilter.kind(), CandidatePrefilterKind::PackedBitmap);

    let results = graph.search_with_prefilter(
        &query,
        SearchLimit::new(4).map_err(HnswError::from)?,
        &prefilter,
    )?;

    assert_eq!(
        results
            .iter()
            .map(|point| point.point_id())
            .collect::<Vec<_>>(),
        vec![
            HnswPointId::new(20),
            HnswPointId::new(30),
            HnswPointId::new(40)
        ]
    );
    Ok(())
}

#[test]
fn hnsw_search_prefilter_uses_sorted_points_for_sparse_ranges() -> context_index::Result<()> {
    let graph = fixture_graph()?;
    let query = vector(&[0.0, 0.0])?;
    let prefilter =
        CandidatePrefilter::from_points([HnswPointId::new(10), HnswPointId::new(10_000)]);

    assert_eq!(prefilter.kind(), CandidatePrefilterKind::Sorted);

    let results = graph.search_with_prefilter(
        &query,
        SearchLimit::new(4).map_err(HnswError::from)?,
        &prefilter,
    )?;

    assert_eq!(
        results
            .iter()
            .map(|point| point.point_id())
            .collect::<Vec<_>>(),
        vec![HnswPointId::new(10)]
    );
    Ok(())
}

#[test]
fn hnsw_search_rejects_candidate_mask_above_budget_even_on_empty_graph() -> context_index::Result<()>
{
    let graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(4, 16, 16)?);
    let query = vector(&[0.0, 0.0])?;
    let max = context_core::policy::DEFAULT_HNSW_CANDIDATE_MASK_POINTS;
    let mask = CandidateMask::only((0..=max).map(|point_id| HnswPointId::new(point_id as u64)));

    let result =
        graph.search_with_mask(&query, SearchLimit::new(3).map_err(HnswError::from)?, &mask);

    assert_eq!(
        result,
        Err(HnswError::RecallBudgetExceeded {
            max,
            actual: max + 1
        })
    );
    assert_eq!(
        HnswError::RecallBudgetExceeded {
            max,
            actual: max + 1
        }
        .context_error(),
        context_core::ContextError::RecallBudgetExceeded
    );
    Ok(())
}

#[test]
fn hnsw_search_rejects_prefilter_above_budget_even_on_empty_graph() -> context_index::Result<()> {
    let graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(4, 16, 16)?);
    let query = vector(&[0.0, 0.0])?;
    let max = context_core::policy::DEFAULT_HNSW_CANDIDATE_MASK_POINTS;
    let prefilter = CandidatePrefilter::from_points(
        (0..=max).map(|point_id| HnswPointId::new(point_id as u64)),
    );

    let result = graph.search_with_prefilter(
        &query,
        SearchLimit::new(3).map_err(HnswError::from)?,
        &prefilter,
    );

    assert_eq!(
        result,
        Err(HnswError::RecallBudgetExceeded {
            max,
            actual: max + 1
        })
    );
    Ok(())
}

#[test]
fn hnsw_search_candidate_mask_accepts_a_caller_supplied_budget_above_the_default()
-> context_index::Result<()> {
    let graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(4, 16, 16)?);
    let query = vector(&[0.0, 0.0])?;
    let default_max = context_core::policy::DEFAULT_HNSW_CANDIDATE_MASK_POINTS;
    let above_default = default_max + 1;
    let mask = CandidateMask::only((0..above_default as u64).map(HnswPointId::new));

    // The default-budget entry point still rejects a mask above the default.
    let result =
        graph.search_with_mask(&query, SearchLimit::new(3).map_err(HnswError::from)?, &mask);
    assert_eq!(
        result,
        Err(HnswError::RecallBudgetExceeded {
            max: default_max,
            actual: above_default,
        })
    );

    // An explicit, caller-supplied budget above the default accepts the same mask.
    assert_eq!(mask.validate_budget_with_limit(above_default), Ok(()));
    Ok(())
}

fn fixture_graph() -> context_index::Result<HnswGraph> {
    let mut graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(4, 16, 16)?);
    for (point_id, values) in [
        (10, [0.0, 0.0]),
        (20, [1.0, 0.0]),
        (30, [2.0, 0.0]),
        (40, [3.0, 0.0]),
    ] {
        graph.insert(HnswPointId::new(point_id), vector(&values)?)?;
    }
    Ok(graph)
}
