//! Filtered HNSW baseline benchmark runner.

#![allow(
    clippy::print_stdout,
    reason = "benchmark runners report measurements on stdout"
)]

use std::{collections::BTreeSet, time::Instant};

use context_core::{DistanceMetric, ExactSearchItem, SearchLimit, exact_top_k};
use context_index::{CandidateMask, HnswConfig, HnswGraph, HnswPointId};
use context_test::{BenchmarkDatasetSpec, BenchmarkRow, PackedPointFilter, RecallSummary};

const LIMIT: usize = 10;

fn main() -> context_index::Result<()> {
    let spec = BenchmarkDatasetSpec::small();
    let rows = spec
        .rows_iter()
        .collect::<context_core::Result<Vec<_>>>()
        .map_err(context_index::HnswError::from)?;
    let query = spec
        .query_vector()
        .map_err(context_index::HnswError::from)?;
    let graph = build_graph(spec, &rows)?;
    let limit = SearchLimit::new(LIMIT).map_err(context_index::HnswError::from)?;

    for filter in [
        TenantFilter::new("narrow", &["tenant-0000"]),
        TenantFilter::new("medium", &["tenant-0000", "tenant-0001", "tenant-0002"]),
        TenantFilter::new(
            "broad",
            &[
                "tenant-0000",
                "tenant-0001",
                "tenant-0002",
                "tenant-0003",
                "tenant-0004",
                "tenant-0005",
                "tenant-0006",
                "tenant-0007",
            ],
        ),
        TenantFilter::new("empty", &["tenant-0099"]),
    ] {
        run_filter(spec, &rows, &graph, &query, limit, filter)?;
    }

    Ok(())
}

fn build_graph(
    spec: BenchmarkDatasetSpec,
    rows: &[BenchmarkRow],
) -> context_index::Result<HnswGraph> {
    let mut graph = HnswGraph::new(DistanceMetric::L2, HnswConfig::new(16, 64, 64)?);
    for row in rows {
        graph.insert(HnswPointId::new(row.point_id), row.vector.clone())?;
    }
    if graph.len() != spec.rows() {
        return Err(context_index::HnswError::InvalidParameter {
            parameter: "graph_len",
            value: graph.len(),
        });
    }
    Ok(graph)
}

fn run_filter(
    spec: BenchmarkDatasetSpec,
    rows: &[BenchmarkRow],
    graph: &HnswGraph,
    query: &context_core::DenseVector,
    limit: SearchLimit,
    filter: TenantFilter,
) -> context_index::Result<()> {
    let filter_started = Instant::now();
    let point_filter = PackedPointFilter::from_rows(rows, |row| filter.matches(&row.tenant_id));
    let filter_elapsed = filter_started.elapsed();
    let allowed = point_filter.allowed_point_ids();
    let allowed_lookup = allowed.iter().copied().collect::<BTreeSet<_>>();
    let exact = filtered_exact_ids(rows, query, &allowed_lookup, limit)?;
    let mask = CandidateMask::only(allowed.iter().copied().map(HnswPointId::new));

    let started = Instant::now();
    let hnsw = graph.search_with_mask(query, limit, &mask)?;
    let elapsed = started.elapsed();
    let recall = RecallSummary::from_point_ids(
        exact.iter().copied(),
        hnsw.iter().map(|point| point.point_id().get()),
    );
    let survival_rate = ratio(allowed.len(), rows.len());

    println!(
        "dataset={:?} filter={} rows={} allowed={} survival_rate={:.6} filter_ms={} bitmap_bytes={} search_ms={} recall={:.6} intersection={} exact_count={} candidate_count={}",
        spec.size(),
        filter.name,
        rows.len(),
        allowed.len(),
        survival_rate,
        filter_elapsed.as_millis(),
        point_filter.bitmap_bytes(),
        elapsed.as_millis(),
        recall.recall(),
        recall.intersection_count(),
        recall.exact_count(),
        recall.candidate_count(),
    );

    Ok(())
}
fn filtered_exact_ids(
    rows: &[BenchmarkRow],
    query: &context_core::DenseVector,
    allowed: &BTreeSet<u64>,
    limit: SearchLimit,
) -> context_index::Result<Vec<u64>> {
    let items = rows
        .iter()
        .filter(|row| allowed.contains(&row.point_id))
        .map(|row| ExactSearchItem::new(row.point_id, row.vector.clone()))
        .collect::<Vec<_>>();
    let exact = exact_top_k(query, &items, DistanceMetric::L2, limit)
        .collect::<context_core::Result<Vec<_>>>()
        .map_err(context_index::HnswError::from)?;
    Ok(exact
        .iter()
        .map(context_core::ScoredPoint::point_id)
        .collect())
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        return 0.0;
    }
    f64::from(u32::try_from(numerator).unwrap_or(u32::MAX))
        / f64::from(u32::try_from(denominator).unwrap_or(u32::MAX))
}

#[derive(Clone, Copy)]
struct TenantFilter {
    name: &'static str,
    tenant_ids: &'static [&'static str],
}

impl TenantFilter {
    const fn new(name: &'static str, tenant_ids: &'static [&'static str]) -> Self {
        Self { name, tenant_ids }
    }

    fn matches(self, tenant_id: &str) -> bool {
        self.tenant_ids.contains(&tenant_id)
    }
}
