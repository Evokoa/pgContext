//! Runs the frozen Phase 15 eager/cursor differential and overhead lane.

#![allow(
    clippy::cast_precision_loss,
    clippy::print_stdout,
    reason = "the certification binary reports bounded deterministic measurements"
)]

use std::{
    cmp::{Ordering, Reverse},
    collections::BinaryHeap,
    time::Instant,
};

use context_core::{DenseVector, DistanceMetric, SearchLimit};
use context_index::{
    GraphRead, GraphRecordId, GraphWrite, HnswComparisonBudget, HnswConfig, HnswError, HnswNodeId,
    HnswPointId, InMemoryGraphStore, LayerIndex, NeverCancel, NewGraphNode,
    projected_hnsw_cursor_retained_bytes, projected_legacy_eager_hnsw_bytes,
    seed_graph_read_cursor,
};
use context_test::{
    P15_DATASET_REVISION, P15_DATASET_SHA256, P15_DEFAULT_ADVANCE_BATCH, P15_DEFAULT_MEMORY_BYTES,
    P15_FIXTURE_DIMENSIONS, P15_FIXTURE_ROWS, P15_GRAPH_SOURCES, P15_MAX_ADVANCE_BATCH,
    P15_MAX_COMPARISONS, P15_MAX_EDGES, P15_MAX_MEMORY_BYTES, P15_MAX_NODE_EXPANSIONS,
    P15_MAX_P50_LATENCY_RATIO_BPS, P15_MAX_RETAINED_MEMORY_RATIO_BPS, P15_METRICS,
    P15_PRE_P15_ORACLE_FNV64, P15_QUERY_COUNT, P15_TIMING_REPEATS, P15_TOP_K,
    P15_WORKLOAD_REVISION, P15_WORKLOAD_SHA256, p15_lazy_hnsw_manifest_hash,
};

const FIXTURE_SEED: u64 = 0x7031_355f_6375_7273;
// Measure the default batch immediately after the eager baseline so untimed
// batch-invariance passes cannot warm the graph in the cursor's favor.
const CERTIFIED_BATCHES: [usize; 4] = [32, 1, 7, 256];

fn main() -> context_index::Result<()> {
    let config = HnswConfig::new(2, 64, 64)?;
    let limit = SearchLimit::new(P15_TOP_K).map_err(HnswError::from)?;
    let mut graph = fixture_graph()?;
    let queries = fixture_queries()?;
    let mut eager_samples = Vec::with_capacity(P15_QUERY_COUNT);
    let mut cursor_samples = Vec::with_capacity(P15_QUERY_COUNT);
    let mut oracle_hash = 0xcbf2_9ce4_8422_2325_u64;

    for (index, query) in queries.iter().enumerate() {
        let mut legacy_timings = Vec::with_capacity(P15_TIMING_REPEATS);
        let mut cursor_timings = Vec::with_capacity(P15_TIMING_REPEATS);
        let eager_started = Instant::now();
        let eager = legacy_oracle_search(&mut graph, query, config, limit)?;
        legacy_timings.push(elapsed_nanos(eager_started));
        oracle_hash = hash_legacy_outcome(oracle_hash, index, &eager);

        let mut certified = None;
        for batch_size in CERTIFIED_BATCHES {
            let cursor_started = Instant::now();
            let lazy = run_cursor(&mut graph, query, config, limit, batch_size)?;
            if batch_size == P15_DEFAULT_ADVANCE_BATCH {
                cursor_timings.push(elapsed_nanos(cursor_started));
                certified = Some(lazy.clone());
            }
            require_legacy_parity(&eager, &lazy)?;
        }
        let lazy = certified.ok_or(HnswError::InvalidSnapshot {
            reason: "certified cursor batches omitted the default batch",
        })?;
        for repeat in 1..P15_TIMING_REPEATS {
            if (index + repeat).is_multiple_of(2) {
                let started = Instant::now();
                let repeated = legacy_oracle_search(&mut graph, query, config, limit)?;
                legacy_timings.push(elapsed_nanos(started));
                require_identical_legacy(&eager, &repeated)?;
                let started = Instant::now();
                let repeated =
                    run_cursor(&mut graph, query, config, limit, P15_DEFAULT_ADVANCE_BATCH)?;
                cursor_timings.push(elapsed_nanos(started));
                require_legacy_parity(&eager, &repeated)?;
            } else {
                let started = Instant::now();
                let repeated =
                    run_cursor(&mut graph, query, config, limit, P15_DEFAULT_ADVANCE_BATCH)?;
                cursor_timings.push(elapsed_nanos(started));
                require_legacy_parity(&eager, &repeated)?;
                let started = Instant::now();
                let repeated = legacy_oracle_search(&mut graph, query, config, limit)?;
                legacy_timings.push(elapsed_nanos(started));
                require_identical_legacy(&eager, &repeated)?;
            }
        }
        eager_samples.push(percentile_50(&mut legacy_timings));
        cursor_samples.push(percentile_50(&mut cursor_timings));
        println!(
            "lazy_hnsw_equivalence\tquery={} results={} comparisons={} expansions={} edges={}",
            index,
            lazy.results().len(),
            lazy.work().distance_evaluations(),
            lazy.work().node_expansions(),
            lazy.work().edges_examined(),
        );
    }

    let eager_p50 = percentile_50(&mut eager_samples);
    let cursor_p50 = percentile_50(&mut cursor_samples);
    let latency_ratio_bps = ratio_bps(cursor_p50, eager_p50);
    let eager_bytes = projected_legacy_eager_hnsw_bytes(
        P15_FIXTURE_ROWS,
        config.ef_search().max(P15_TOP_K),
        false,
    )?;
    let cursor_bytes = projected_hnsw_cursor_retained_bytes(
        P15_FIXTURE_ROWS,
        config.ef_search().max(P15_TOP_K),
        false,
    )?;
    let memory_ratio_bps = ratio_bps(cursor_bytes as u64, eager_bytes as u64);
    let passed = latency_ratio_bps <= u64::from(P15_MAX_P50_LATENCY_RATIO_BPS)
        && memory_ratio_bps <= u64::from(P15_MAX_RETAINED_MEMORY_RATIO_BPS);

    println!(
        "lazy_hnsw_manifest\tmanifest_hash={:016x} dataset_revision={} dataset_sha256={} workload_revision={} workload_sha256={}",
        p15_lazy_hnsw_manifest_hash(),
        P15_DATASET_REVISION,
        P15_DATASET_SHA256,
        P15_WORKLOAD_REVISION,
        P15_WORKLOAD_SHA256,
    );
    if oracle_hash != P15_PRE_P15_ORACLE_FNV64 {
        return Err(HnswError::InvalidSnapshot {
            reason: "pre-P15 oracle output/work hash drifted",
        });
    }
    println!("lazy_hnsw_oracle\tpre_p15_fnv64={oracle_hash:016x} exact=true");
    println!(
        "lazy_hnsw_batch_invariance\tqueries={} batches=1,7,32,256 exact=true",
        P15_QUERY_COUNT
    );
    println!(
        "lazy_hnsw_budget\tdefault_batch={} max_batch={} max_comparisons={} max_node_expansions={} max_edges={} default_memory_bytes={} max_memory_bytes={}",
        P15_DEFAULT_ADVANCE_BATCH,
        P15_MAX_ADVANCE_BATCH,
        P15_MAX_COMPARISONS,
        P15_MAX_NODE_EXPANSIONS,
        P15_MAX_EDGES,
        P15_DEFAULT_MEMORY_BYTES,
        P15_MAX_MEMORY_BYTES,
    );
    println!(
        "lazy_hnsw_standalone_scope\tmetric=l2 graph_source=in_memory independent_pre_p15_oracle=true required_metrics={} required_graph_sources={}",
        P15_METRICS.join(","),
        P15_GRAPH_SOURCES.join(","),
    );
    println!(
        "lazy_hnsw_resources\teager_p50_ns={} cursor_p50_ns={} latency_ratio_bps={} eager_bytes={} cursor_bytes={} memory_ratio_bps={}",
        eager_p50, cursor_p50, latency_ratio_bps, eager_bytes, cursor_bytes, memory_ratio_bps,
    );
    println!(
        "lazy_hnsw_samples\teager_ns={} cursor_ns={}",
        join_samples(&eager_samples),
        join_samples(&cursor_samples),
    );
    println!(
        "lazy_hnsw_decision\tdecision={}",
        if passed { "pass" } else { "no_go" }
    );
    if !passed {
        return Err(HnswError::InvalidSnapshot {
            reason: "lazy cursor exceeded the frozen overhead threshold",
        });
    }
    Ok(())
}

fn run_cursor(
    graph: &mut InMemoryGraphStore,
    query: &DenseVector,
    config: HnswConfig,
    limit: SearchLimit,
    batch_size: usize,
) -> context_index::Result<context_index::HnswSearchOutcome> {
    let comparison_budget = HnswComparisonBudget::new(P15_MAX_COMPARISONS);
    let mut cancellation = NeverCancel;
    let mut cursor = seed_graph_read_cursor(
        graph,
        DistanceMetric::L2,
        query,
        config,
        limit,
        &comparison_budget,
        context_index::HnswCursorBudget::new(
            P15_MAX_NODE_EXPANSIONS,
            P15_MAX_EDGES,
            P15_MAX_MEMORY_BYTES,
        ),
        &mut cancellation,
    )?;
    while cursor.termination().is_none() {
        cursor.advance(batch_size)?;
    }
    cursor.finish()
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct LegacyCandidate {
    node_id: HnswNodeId,
    score: f32,
}

impl Eq for LegacyCandidate {}

impl Ord for LegacyCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| self.node_id.cmp(&other.node_id))
    }
}

impl PartialOrd for LegacyCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct LegacyWork {
    comparisons: usize,
    expansions: usize,
    edges: usize,
    cancellation_checks: usize,
}

#[derive(Debug, PartialEq)]
struct LegacyOutcome {
    results: Vec<(HnswPointId, f32)>,
    work: LegacyWork,
}

fn require_identical_legacy(
    expected: &LegacyOutcome,
    actual: &LegacyOutcome,
) -> context_index::Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(HnswError::InvalidSnapshot {
            reason: "pre-P15 timing oracle changed between repetitions",
        })
    }
}

fn legacy_oracle_search(
    graph: &mut impl GraphRead,
    query: &DenseVector,
    config: HnswConfig,
    limit: SearchLimit,
) -> context_index::Result<LegacyOutcome> {
    let metadata = graph.metadata()?;
    let Some(entry) = metadata.entry_point() else {
        return Ok(LegacyOutcome {
            results: Vec::new(),
            work: LegacyWork::default(),
        });
    };
    let dimensions = metadata.dimensions().ok_or(HnswError::InvalidSnapshot {
        reason: "legacy oracle graph dimensions are missing",
    })?;
    if query.dimension() != dimensions {
        return Err(HnswError::DimensionMismatch {
            left: dimensions,
            right: query.dimension(),
        });
    }
    graph.prepare_query(DistanceMetric::L2, query)?;
    let comparison_budget = HnswComparisonBudget::new(P15_MAX_COMPARISONS);
    let mut work = LegacyWork {
        cancellation_checks: 1,
        ..LegacyWork::default()
    };
    comparison_budget.reserve_comparison()?;
    work.comparisons += 1;
    let entry_layers = legacy_score(graph, query, entry)?.2;
    if entry_layers != 1 {
        return Err(HnswError::InvalidSnapshot {
            reason: "standalone legacy oracle fixture unexpectedly has upper layers",
        });
    }
    comparison_budget.reserve_comparison()?;
    work.comparisons += 1;
    let entry_candidate = LegacyCandidate {
        node_id: entry,
        score: legacy_score(graph, query, entry)?.0,
    };
    let mut pending = BinaryHeap::from([Reverse(entry_candidate)]);
    let mut nearest = BinaryHeap::from([entry_candidate]);
    let mut visited = vec![false; metadata.node_count()];
    visited[entry.get()] = true;
    let mut neighbors = Vec::new();
    let ef = config.ef_search().max(limit.get());
    while let Some(Reverse(candidate)) = pending.pop() {
        work.cancellation_checks += 1;
        work.expansions += 1;
        let worst = nearest.peek().map_or(f32::INFINITY, |item| item.score);
        if nearest.len() >= ef && candidate.score > worst {
            break;
        }
        if !graph.read_neighbors_into(candidate.node_id, LayerIndex::base(), &mut neighbors)? {
            return Err(HnswError::InvalidSnapshot {
                reason: "legacy oracle adjacency is missing",
            });
        }
        for neighbor in neighbors.iter().copied() {
            work.edges += 1;
            let Some(seen) = visited.get_mut(neighbor.get()) else {
                return Err(HnswError::InvalidSnapshot {
                    reason: "legacy oracle neighbor exceeds graph bounds",
                });
            };
            if *seen {
                continue;
            }
            *seen = true;
            comparison_budget.reserve_comparison()?;
            work.comparisons += 1;
            let scored = LegacyCandidate {
                node_id: neighbor,
                score: legacy_score(graph, query, neighbor)?.0,
            };
            let should_add = nearest.len() < ef
                || nearest
                    .peek()
                    .is_some_and(|current_worst| scored < *current_worst);
            if should_add {
                pending.push(Reverse(scored));
                nearest.push(scored);
                if nearest.len() > ef {
                    nearest.pop();
                }
            }
        }
    }
    let candidates = nearest.into_sorted_vec();
    let mut results = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        comparison_budget.reserve_comparison()?;
        let point_id = graph
            .with_node(candidate.node_id, |node| node.point_id())?
            .ok_or(HnswError::InvalidSnapshot {
                reason: "legacy oracle result node is missing",
            })?;
        results.push((point_id, candidate.score));
    }
    results.sort_by(|left, right| {
        left.1
            .total_cmp(&right.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    results.truncate(limit.get());
    Ok(LegacyOutcome { results, work })
}

fn legacy_score(
    graph: &mut impl GraphRead,
    query: &DenseVector,
    node_id: HnswNodeId,
) -> context_index::Result<(f32, HnswPointId, usize)> {
    graph
        .score_node(node_id, DistanceMetric::L2, query)?
        .map(|score| (score.score(), score.point_id(), score.layer_count()))
        .ok_or(HnswError::InvalidSnapshot {
            reason: "legacy oracle node is missing",
        })
}

fn require_legacy_parity(
    legacy: &LegacyOutcome,
    lazy: &context_index::HnswSearchOutcome,
) -> context_index::Result<()> {
    let results_equal = legacy.results.len() == lazy.results().len()
        && legacy
            .results
            .iter()
            .zip(lazy.results())
            .all(|((point_id, score), actual)| {
                *point_id == actual.point_id() && score.to_bits() == actual.score().to_bits()
            });
    let actual = lazy.work();
    let work_equal = legacy.work.comparisons == actual.distance_evaluations()
        && legacy.work.expansions == actual.node_expansions()
        && legacy.work.edges == actual.edges_examined()
        && legacy.work.cancellation_checks == actual.cancellation_checks();
    if results_equal && work_equal {
        Ok(())
    } else {
        Err(HnswError::InvalidSnapshot {
            reason: "lazy cursor diverged from the frozen pre-P15 traversal oracle",
        })
    }
}

fn hash_legacy_outcome(mut hash: u64, query_index: usize, outcome: &LegacyOutcome) -> u64 {
    hash = fnv1a(hash, &query_index.to_le_bytes());
    for (point_id, score) in &outcome.results {
        hash = fnv1a(hash, &point_id.get().to_le_bytes());
        hash = fnv1a(hash, &score.to_bits().to_le_bytes());
    }
    for value in [
        outcome.work.comparisons,
        outcome.work.expansions,
        outcome.work.edges,
        outcome.work.cancellation_checks,
    ] {
        hash = fnv1a(hash, &value.to_le_bytes());
    }
    hash
}

fn fnv1a(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn fixture_graph() -> context_index::Result<InMemoryGraphStore> {
    let mut graph = InMemoryGraphStore::new();
    let mut prior = None;
    for row in 0..P15_FIXTURE_ROWS {
        let node = graph.append_node(NewGraphNode::new(
            GraphRecordId::new(row as u64),
            HnswPointId::new(row as u64),
            fixture_vector(row)?,
            vec![prior.into_iter().collect()],
        ))?;
        if let Some(previous) = prior {
            let neighbors = if previous.get() == 0 {
                vec![node]
            } else {
                vec![HnswNodeId::new(previous.get() - 1), node]
            };
            graph.replace_neighbors(previous, LayerIndex::base(), neighbors)?;
        }
        prior = Some(node);
    }
    graph.publish_entry_point(prior)?;
    Ok(graph)
}

fn fixture_queries() -> context_index::Result<Vec<DenseVector>> {
    (0..P15_QUERY_COUNT)
        .map(|query| fixture_vector((query * 79) % P15_FIXTURE_ROWS))
        .collect()
}

fn fixture_vector(row: usize) -> context_index::Result<DenseVector> {
    let values = (0..P15_FIXTURE_DIMENSIONS)
        .map(|dimension| {
            let mixed = splitmix64(
                FIXTURE_SEED ^ (row as u64).rotate_left(17) ^ (dimension as u64).rotate_left(41),
            );
            let fraction = (mixed >> 40) as f32 / ((1_u32 << 24) - 1) as f32;
            fraction.mul_add(2.0, -1.0)
        })
        .collect();
    DenseVector::new(values).map_err(HnswError::from)
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn percentile_50(samples: &mut [u64]) -> u64 {
    samples.sort_unstable();
    samples[samples.len() / 2]
}

fn ratio_bps(numerator: u64, denominator: u64) -> u64 {
    numerator
        .saturating_mul(10_000)
        .div_ceil(denominator.max(1))
}

fn join_samples(samples: &[u64]) -> String {
    samples
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",")
}
