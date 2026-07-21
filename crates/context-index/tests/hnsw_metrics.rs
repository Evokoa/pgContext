//! Metric dispatch and ordering contracts for pure HNSW.

use context_core::{DenseVector, DistanceMetric, ExactSearchItem, SearchLimit, exact_top_k};
use context_index::{HnswConfig, HnswError, HnswGraph, HnswPointId};

fn vector(values: &[f32]) -> Result<DenseVector, HnswError> {
    DenseVector::new(values.to_vec()).map_err(HnswError::from)
}

#[test]
fn hnsw_build_and_search_match_exact_order_for_every_v1_metric() -> context_index::Result<()> {
    let fixtures = [
        (10_u64, [1.0, 0.0]),
        (20, [0.0, 1.0]),
        (30, [2.0, 1.0]),
        (40, [1.0, 2.0]),
        (50, [3.0, 1.0]),
        (60, [1.0, 3.0]),
    ];
    let query = vector(&[2.0, 2.0])?;
    let limit = SearchLimit::new(fixtures.len()).map_err(HnswError::from)?;

    for metric in [
        DistanceMetric::L2,
        DistanceMetric::NegativeInnerProduct,
        DistanceMetric::Cosine,
        DistanceMetric::L1,
    ] {
        let mut graph = HnswGraph::new(metric, HnswConfig::new(4, 32, 32)?);
        let mut exact_items = Vec::new();
        for (point_id, values) in fixtures {
            let item = vector(&values)?;
            graph.insert(HnswPointId::new(point_id), item.clone())?;
            exact_items.push(ExactSearchItem::new(point_id, item));
        }

        let actual = graph.search(&query, limit)?;
        let expected = exact_top_k(&query, &exact_items, metric, limit)
            .collect::<Result<Vec<_>, _>>()
            .map_err(HnswError::from)?;

        assert_eq!(actual.len(), expected.len(), "metric {metric:?}");
        for (actual, expected) in actual.iter().zip(expected) {
            assert_eq!(actual.point_id().get(), expected.point_id());
            assert_eq!(actual.score(), expected.score());
        }
        assert_eq!(graph.search(&query, limit)?, actual, "metric {metric:?}");
    }

    Ok(())
}

#[test]
fn hnsw_rejects_raw_inner_product_before_build_or_traversal() -> context_index::Result<()> {
    let mut graph = HnswGraph::new(DistanceMetric::InnerProduct, HnswConfig::new(4, 16, 16)?);
    let query = vector(&[1.0, 1.0])?;
    let limit = SearchLimit::new(1).map_err(HnswError::from)?;

    assert_eq!(
        graph.search(&query, limit),
        Err(HnswError::UnsupportedMetric {
            metric: "inner_product"
        })
    );
    assert_eq!(
        graph.insert(HnswPointId::new(1), query),
        Err(HnswError::UnsupportedMetric {
            metric: "inner_product"
        })
    );
    assert!(graph.is_empty());

    Ok(())
}
