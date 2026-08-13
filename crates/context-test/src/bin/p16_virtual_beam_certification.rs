//! Runs the frozen Phase 16 graph-off correctness and overhead lane.

#![allow(
    clippy::cast_precision_loss,
    clippy::print_stdout,
    reason = "the certification binary reports bounded deterministic measurements"
)]

use std::{cmp::Ordering, collections::BTreeMap, fmt, mem::size_of, time::Instant};

use context_core::{DenseVector, DistanceMetric, OccurrenceId, PointId, SearchLimit};
use context_index::{
    GraphRecordId, GraphWrite, HnswComparisonBudget, HnswConfig, HnswCursorBudget, HnswError,
    HnswNodeId, HnswPointId, InMemoryGraphStore, LayerIndex, NeverCancel, NewGraphNode,
    projected_hnsw_cursor_retained_bytes, seed_graph_read_cursor,
};
use context_query::{
    AuthorizationContextToken, BeamBudget, BeamCompletion, BeamExpansionBatch,
    BeamExpansionProvider, BeamProviderRequest, BeamScoreComponents, BeamSeed, Cancellation,
    PathPatternState, PortBudget, QueryClock, QueryError, VirtualBeamEngine,
};
use context_test::{
    P15_DEFAULT_ADVANCE_BATCH, P15_FIXTURE_DIMENSIONS, P15_FIXTURE_ROWS, P15_MAX_COMPARISONS,
    P15_MAX_EDGES, P15_MAX_MEMORY_BYTES, P15_MAX_NODE_EXPANSIONS, P16_BEAM_CONTRACT,
    P16_DATASET_REVISION, P16_DATASET_SHA256, P16_DEFAULT_BEAM_WIDTH, P16_DEFAULT_EXPANSION_BATCH,
    P16_EXPANDED_BEAM_ORACLE_FNV64, P16_FIXTURE_SEEDS, P16_GRAPH_OFF_ORACLE_FNV64,
    P16_MAX_ADMITTED_STATES, P16_MAX_ELAPSED_MICROS, P16_MAX_EXACT_RERANKS, P16_MAX_HOPS,
    P16_MAX_P50_LATENCY_RATIO_BPS, P16_MAX_PARENT_BYTES, P16_MAX_RETAINED_BYTES,
    P16_MAX_RETAINED_MEMORY_RATIO_BPS, P16_MAX_VECTOR_EXPANSIONS, P16_MAX_VISITED_KEYS,
    P16_QUERY_COUNT, P16_TIMING_REPEATS, P16_TOP_K, P16_WORKLOAD_REVISION, P16_WORKLOAD_SHA256,
    p16_virtual_beam_manifest_hash,
};

const FIXTURE_SEED: u64 = 0x7031_355f_6375_7273;

#[derive(Debug)]
enum CertificationError {
    Index(HnswError),
    Query(QueryError),
    Invariant(&'static str),
}

impl fmt::Display for CertificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Index(error) => error.fmt(formatter),
            Self::Query(error) => error.fmt(formatter),
            Self::Invariant(reason) => formatter.write_str(reason),
        }
    }
}

impl std::error::Error for CertificationError {}

impl From<HnswError> for CertificationError {
    fn from(error: HnswError) -> Self {
        Self::Index(error)
    }
}

impl From<QueryError> for CertificationError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

type CertificationResult<T> = Result<T, CertificationError>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CertifiedHit {
    occurrence_id: u64,
    point_id: u64,
    score_bits: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CertifiedWork {
    comparisons: usize,
    expansions: usize,
    edges: usize,
    cancellation_checks: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DirectOutcome {
    hits: Vec<CertifiedHit>,
    work: CertifiedWork,
}

struct EmptyProvider;

impl BeamExpansionProvider for EmptyProvider {
    fn expand(
        &mut self,
        _request: &BeamProviderRequest,
        _budget: PortBudget,
    ) -> context_query::Result<BeamExpansionBatch> {
        Ok(BeamExpansionBatch::new(Vec::new(), 0, 0))
    }
}

struct NeverCancelled;

impl Cancellation for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

struct FrozenClock;

impl QueryClock for FrozenClock {
    fn now_micros(&self) -> u64 {
        0
    }
}

fn main() -> CertificationResult<()> {
    let config = HnswConfig::new(2, 64, 64)?;
    let limit = SearchLimit::new(P16_TOP_K).map_err(HnswError::from)?;
    let mut graph = fixture_graph()?;
    let queries = fixture_queries()?;
    let mut direct_samples = Vec::with_capacity(P16_QUERY_COUNT);
    let mut beam_samples = Vec::with_capacity(P16_QUERY_COUNT);
    let mut oracle_hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut parity_hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut max_beam_bytes = 0_usize;
    let expanded_oracle_hash = certify_expanded_beam_oracle()?;

    for (query_index, query) in queries.iter().enumerate() {
        let mut direct_timings = Vec::with_capacity(P16_TIMING_REPEATS);
        let mut beam_timings = Vec::with_capacity(P16_TIMING_REPEATS);

        let started = Instant::now();
        let direct = run_direct(&mut graph, query, config, limit)?;
        direct_timings.push(elapsed_nanos(started));
        oracle_hash = hash_direct_outcome(oracle_hash, query_index, &direct);

        let started = Instant::now();
        let (beam, beam_bytes) = run_beam(&mut graph, query, config, limit)?;
        beam_timings.push(elapsed_nanos(started));
        require_parity(&direct, &beam)?;
        parity_hash = hash_beam_outcome(parity_hash, query_index, &beam);
        max_beam_bytes = max_beam_bytes.max(beam_bytes);

        for repeat in 1..P16_TIMING_REPEATS {
            if (query_index + repeat).is_multiple_of(2) {
                let started = Instant::now();
                let repeated = run_direct(&mut graph, query, config, limit)?;
                direct_timings.push(elapsed_nanos(started));
                require_identical_direct(&direct, &repeated)?;

                let started = Instant::now();
                let (repeated, bytes) = run_beam(&mut graph, query, config, limit)?;
                beam_timings.push(elapsed_nanos(started));
                require_parity(&direct, &repeated)?;
                max_beam_bytes = max_beam_bytes.max(bytes);
            } else {
                let started = Instant::now();
                let (repeated, bytes) = run_beam(&mut graph, query, config, limit)?;
                beam_timings.push(elapsed_nanos(started));
                require_parity(&direct, &repeated)?;
                max_beam_bytes = max_beam_bytes.max(bytes);

                let started = Instant::now();
                let repeated = run_direct(&mut graph, query, config, limit)?;
                direct_timings.push(elapsed_nanos(started));
                require_identical_direct(&direct, &repeated)?;
            }
        }
        direct_samples.push(percentile_50(&mut direct_timings));
        beam_samples.push(percentile_50(&mut beam_timings));
    }

    let direct_p50 = percentile_50(&mut direct_samples);
    let beam_p50 = percentile_50(&mut beam_samples);
    let latency_ratio_bps = ratio_bps(beam_p50, direct_p50);
    let cursor_bytes = projected_hnsw_cursor_retained_bytes(
        P15_FIXTURE_ROWS,
        config.ef_search().max(P16_TOP_K),
        false,
    )?;
    let direct_bytes = cursor_bytes
        .checked_add(P16_TOP_K * size_of::<CertifiedHit>())
        .ok_or(CertificationError::Invariant(
            "direct retained-byte projection overflowed",
        ))?;
    let beam_bytes =
        cursor_bytes
            .checked_add(max_beam_bytes)
            .ok_or(CertificationError::Invariant(
                "beam retained-byte projection overflowed",
            ))?;
    let memory_ratio_bps = ratio_bps(beam_bytes as u64, direct_bytes as u64);
    let passed = latency_ratio_bps <= u64::from(P16_MAX_P50_LATENCY_RATIO_BPS)
        && memory_ratio_bps <= u64::from(P16_MAX_RETAINED_MEMORY_RATIO_BPS);

    println!(
        "virtual_beam_manifest\tmanifest_hash={:016x} contract={} dataset_revision={} dataset_sha256={} workload_revision={} workload_sha256={}",
        p16_virtual_beam_manifest_hash(),
        P16_BEAM_CONTRACT,
        P16_DATASET_REVISION,
        P16_DATASET_SHA256,
        P16_WORKLOAD_REVISION,
        P16_WORKLOAD_SHA256,
    );
    println!(
        "virtual_beam_oracle\tgraph_off_fnv64={oracle_hash:016x} parity_fnv64={parity_hash:016x} independent=true"
    );
    if oracle_hash != P16_GRAPH_OFF_ORACLE_FNV64 {
        return Err(CertificationError::Invariant(
            "P16 exhaustive graph-off oracle hash drifted",
        ));
    }
    println!(
        "virtual_beam_expanded_oracle\treference_fnv64={expanded_oracle_hash:016x} independent=true expansions=true dominance=true cycles=true paths=true provider_order=true"
    );
    if P16_EXPANDED_BEAM_ORACLE_FNV64 != 0 && expanded_oracle_hash != P16_EXPANDED_BEAM_ORACLE_FNV64
    {
        return Err(CertificationError::Invariant(
            "P16 expanded-beam reference hash drifted",
        ));
    }
    println!(
        "virtual_beam_properties\tqueries={} seeds_per_query={} top_k={} ordered_score_exact=true",
        P16_QUERY_COUNT, P16_FIXTURE_SEEDS, P16_TOP_K,
    );
    println!(
        "virtual_beam_provider_contract\tprovider=empty_vector topology=false provider_calls_per_query=1 response_records=0"
    );
    println!(
        "virtual_beam_budget\tbeam_width={} expansion_batch={} admitted_states={} visited_keys={} vector_expansions={} exact_reranks={} parent_bytes={} retained_bytes={} hops={} elapsed_micros={}",
        P16_DEFAULT_BEAM_WIDTH,
        P16_DEFAULT_EXPANSION_BATCH,
        P16_MAX_ADMITTED_STATES,
        P16_MAX_VISITED_KEYS,
        P16_MAX_VECTOR_EXPANSIONS,
        P16_MAX_EXACT_RERANKS,
        P16_MAX_PARENT_BYTES,
        P16_MAX_RETAINED_BYTES,
        P16_MAX_HOPS,
        P16_MAX_ELAPSED_MICROS,
    );
    println!(
        "virtual_beam_graph_off_parity\tqueries={} occurrence_point_score_work_exact=true",
        P16_QUERY_COUNT,
    );
    println!(
        "virtual_beam_resources\tdirect_p50_ns={} beam_p50_ns={} latency_ratio_bps={} direct_bytes={} beam_bytes={} memory_ratio_bps={}",
        direct_p50, beam_p50, latency_ratio_bps, direct_bytes, beam_bytes, memory_ratio_bps,
    );
    println!(
        "virtual_beam_samples\tdirect_ns={} beam_ns={}",
        join_samples(&direct_samples),
        join_samples(&beam_samples),
    );
    println!(
        "virtual_beam_decision\tdecision={}",
        if passed { "pass" } else { "no_go" },
    );
    if !passed {
        return Err(CertificationError::Invariant(
            "virtual beam exceeded a frozen graph-off overhead threshold",
        ));
    }
    Ok(())
}

fn run_direct(
    graph: &mut InMemoryGraphStore,
    query: &DenseVector,
    config: HnswConfig,
    limit: SearchLimit,
) -> CertificationResult<DirectOutcome> {
    let outcome = run_cursor(graph, query, config, limit)?;
    let hits = outcome
        .results()
        .iter()
        .map(|result| {
            Ok(CertifiedHit {
                occurrence_id: result.point_id().get().checked_add(1).ok_or(
                    CertificationError::Invariant("fixture occurrence identity overflowed"),
                )?,
                point_id: result.point_id().get(),
                score_bits: (-f64::from(result.score())).to_bits(),
            })
        })
        .collect::<CertificationResult<Vec<_>>>()?;
    let work = outcome.work();
    Ok(DirectOutcome {
        hits,
        work: CertifiedWork {
            comparisons: work.distance_evaluations(),
            expansions: work.node_expansions(),
            edges: work.edges_examined(),
            cancellation_checks: work.cancellation_checks(),
        },
    })
}

fn run_beam(
    graph: &mut InMemoryGraphStore,
    query: &DenseVector,
    config: HnswConfig,
    limit: SearchLimit,
) -> CertificationResult<(DirectOutcome, usize)> {
    let outcome = run_cursor(graph, query, config, limit)?;
    let authorization = AuthorizationContextToken::new(1).ok_or(CertificationError::Invariant(
        "fixture authorization token is zero",
    ))?;
    let seeds = outcome
        .results()
        .iter()
        .map(|result| {
            let occurrence = result
                .point_id()
                .get()
                .checked_add(1)
                .and_then(OccurrenceId::new)
                .ok_or(CertificationError::Invariant(
                    "fixture occurrence identity overflowed",
                ))?;
            Ok(BeamSeed::new(
                occurrence,
                PointId::new(result.point_id().get()),
                None,
                PathPatternState::default(),
                authorization,
                BeamScoreComponents::new(-f64::from(result.score()), 0.0, 0.0, None)?,
            ))
        })
        .collect::<CertificationResult<Vec<_>>>()?;
    if seeds.len() != P16_FIXTURE_SEEDS {
        return Err(CertificationError::Invariant(
            "P15 graph-off seed count changed",
        ));
    }
    let beam = VirtualBeamEngine::new(BeamBudget::default_internal(P16_TOP_K)?).run(
        &seeds,
        &mut EmptyProvider,
        &NeverCancelled,
        &FrozenClock,
    )?;
    if beam.completion() != BeamCompletion::Exhausted
        || beam.diagnostics().provider_calls() != 1
        || beam.diagnostics().vector_expansions() != 0
        || beam.diagnostics().exact_reranks() != 0
        || beam.hits().iter().any(|hit| hit.path().len() != 1)
    {
        return Err(CertificationError::Invariant(
            "graph-off beam did not terminate as a seed-only execution",
        ));
    }
    let hits = beam
        .hits()
        .iter()
        .map(|hit| CertifiedHit {
            occurrence_id: hit.occurrence_id().get(),
            point_id: hit.point_id().get(),
            score_bits: hit.scores().ranking().to_bits(),
        })
        .collect();
    let work = outcome.work();
    Ok((
        DirectOutcome {
            hits,
            work: CertifiedWork {
                comparisons: work.distance_evaluations(),
                expansions: work.node_expansions(),
                edges: work.edges_examined(),
                cancellation_checks: work.cancellation_checks(),
            },
        },
        beam.diagnostics().retained_bytes(),
    ))
}

fn run_cursor(
    graph: &mut InMemoryGraphStore,
    query: &DenseVector,
    config: HnswConfig,
    limit: SearchLimit,
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
        HnswCursorBudget::new(P15_MAX_NODE_EXPANSIONS, P15_MAX_EDGES, P15_MAX_MEMORY_BYTES),
        &mut cancellation,
    )?;
    while cursor.termination().is_none() {
        cursor.advance(P15_DEFAULT_ADVANCE_BATCH)?;
    }
    cursor.finish()
}

fn require_parity(expected: &DirectOutcome, actual: &DirectOutcome) -> CertificationResult<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(CertificationError::Invariant(
            "P16 graph-off output or P15 work diverged",
        ))
    }
}

fn require_identical_direct(
    expected: &DirectOutcome,
    actual: &DirectOutcome,
) -> CertificationResult<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(CertificationError::Invariant(
            "P15 direct timing outcome changed between repetitions",
        ))
    }
}

fn hash_direct_outcome(mut hash: u64, query_index: usize, outcome: &DirectOutcome) -> u64 {
    hash = fnv1a(hash, &query_index.to_le_bytes());
    for hit in &outcome.hits {
        hash = fnv1a(hash, &hit.occurrence_id.to_le_bytes());
        hash = fnv1a(hash, &hit.point_id.to_le_bytes());
        hash = fnv1a(hash, &hit.score_bits.to_le_bytes());
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

fn hash_beam_outcome(mut hash: u64, query_index: usize, outcome: &DirectOutcome) -> u64 {
    hash = hash_direct_outcome(hash, query_index, outcome);
    fnv1a(hash, &[1, 0, 0])
}

#[derive(Clone, Copy)]
struct ReferenceEdge {
    child: u64,
    score: f64,
}

#[derive(Clone)]
struct ExpandedProvider {
    edges: BTreeMap<u64, Vec<ReferenceEdge>>,
    reverse: bool,
}

impl BeamExpansionProvider for ExpandedProvider {
    fn expand(
        &mut self,
        request: &BeamProviderRequest,
        _budget: PortBudget,
    ) -> context_query::Result<BeamExpansionBatch> {
        let mut expansions = Vec::new();
        for parent in request.parents() {
            if let Some(edges) = self.edges.get(&parent.occurrence_id().get()) {
                for edge in edges {
                    if expansions.len() == request.max_expansions() {
                        break;
                    }
                    expansions.push(context_query::BeamExpansion::new(
                        parent.state_id(),
                        OccurrenceId::new(edge.child).ok_or(QueryError::InvalidInput {
                            field: "reference_occurrence",
                            reason: "must be nonzero".to_owned(),
                        })?,
                        PointId::new(edge.child),
                        None,
                        None,
                        PathPatternState::default(),
                        edge.score,
                        0.0,
                        None,
                    )?);
                }
            }
        }
        if self.reverse {
            expansions.reverse();
        }
        let work = expansions.len();
        Ok(BeamExpansionBatch::new(expansions, work, 0))
    }
}

#[derive(Clone)]
struct ReferenceState {
    occurrence_id: u64,
    point_id: u64,
    parent: Option<usize>,
    hop: u16,
    score: f64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReferenceHit {
    occurrence_id: u64,
    point_id: u64,
    score_bits: u64,
    path: Vec<u64>,
}

struct ReferenceOutcome {
    hits: Vec<ReferenceHit>,
    provider_calls: usize,
    vector_expansions: usize,
    dominated: usize,
    cycles: usize,
}

fn certify_expanded_beam_oracle() -> CertificationResult<u64> {
    let edges = reference_edges();
    let expected = run_reference_beam(&edges);
    let authorization = AuthorizationContextToken::new(1).ok_or(CertificationError::Invariant(
        "reference authorization token is zero",
    ))?;
    let seeds = [
        BeamSeed::new(
            OccurrenceId::new(1).ok_or(CertificationError::Invariant(
                "reference seed identity is zero",
            ))?,
            PointId::new(1),
            None,
            PathPatternState::default(),
            authorization,
            BeamScoreComponents::new(0.0, 0.0, 0.0, None)?,
        ),
        BeamSeed::new(
            OccurrenceId::new(2).ok_or(CertificationError::Invariant(
                "reference seed identity is zero",
            ))?,
            PointId::new(2),
            None,
            PathPatternState::default(),
            authorization,
            BeamScoreComponents::new(0.0, 0.0, 0.0, None)?,
        ),
    ];
    let mut canonical = None;
    for reverse in [false, true] {
        let outcome = VirtualBeamEngine::new(BeamBudget::new(
            4,
            4,
            32,
            32,
            64,
            64,
            1 << 20,
            2 << 20,
            8,
            5,
            10_000,
        )?)
        .run(
            &seeds,
            &mut ExpandedProvider {
                edges: edges.clone(),
                reverse,
            },
            &NeverCancelled,
            &FrozenClock,
        )?;
        let actual = outcome
            .hits()
            .iter()
            .map(|hit| ReferenceHit {
                occurrence_id: hit.occurrence_id().get(),
                point_id: hit.point_id().get(),
                score_bits: hit.scores().ranking().to_bits(),
                path: hit
                    .path()
                    .iter()
                    .map(|step| step.occurrence_id().get())
                    .collect(),
            })
            .collect::<Vec<_>>();
        if actual != expected.hits
            || outcome.diagnostics().provider_calls() != expected.provider_calls
            || outcome.diagnostics().vector_expansions() != expected.vector_expansions
            || outcome.diagnostics().pruning().dominated() != expected.dominated
            || outcome.diagnostics().pruning().cycle() != expected.cycles
        {
            return Err(CertificationError::Invariant(
                "expanded beam diverged from independent reference traversal",
            ));
        }
        if canonical.as_ref().is_some_and(|prior| prior != &actual) {
            return Err(CertificationError::Invariant(
                "expanded beam changed with provider order",
            ));
        }
        canonical = Some(actual);
    }
    Ok(hash_reference_outcome(&expected))
}

fn reference_edges() -> BTreeMap<u64, Vec<ReferenceEdge>> {
    BTreeMap::from([
        (
            1,
            vec![
                ReferenceEdge {
                    child: 3,
                    score: 2.0,
                },
                ReferenceEdge {
                    child: 4,
                    score: 1.0,
                },
            ],
        ),
        (
            2,
            vec![
                ReferenceEdge {
                    child: 3,
                    score: 3.0,
                },
                ReferenceEdge {
                    child: 2,
                    score: 9.0,
                },
            ],
        ),
        (
            3,
            vec![
                ReferenceEdge {
                    child: 5,
                    score: 1.0,
                },
                ReferenceEdge {
                    child: 1,
                    score: 9.0,
                },
            ],
        ),
        (
            4,
            vec![ReferenceEdge {
                child: 5,
                score: 2.0,
            }],
        ),
    ])
}

fn run_reference_beam(edges: &BTreeMap<u64, Vec<ReferenceEdge>>) -> ReferenceOutcome {
    let mut states = vec![
        ReferenceState {
            occurrence_id: 1,
            point_id: 1,
            parent: None,
            hop: 0,
            score: 0.0,
        },
        ReferenceState {
            occurrence_id: 2,
            point_id: 2,
            parent: None,
            hop: 0,
            score: 0.0,
        },
    ];
    let mut dominant = BTreeMap::from([(1_u64, 0_usize), (2, 1)]);
    let mut frontier = vec![0_usize, 1];
    let mut provider_calls = 0_usize;
    let mut vector_expansions = 0_usize;
    let mut dominated = 0_usize;
    let mut cycles = 0_usize;
    while !frontier.is_empty() {
        frontier.sort_unstable_by(|left, right| compare_reference(&states, *left, *right));
        let parents = std::mem::take(&mut frontier);
        provider_calls += 1;
        let mut expanded = Vec::new();
        for parent in parents {
            for edge in edges
                .get(&states[parent].occurrence_id)
                .into_iter()
                .flatten()
            {
                expanded.push((parent, *edge));
            }
        }
        vector_expansions += expanded.len();
        expanded.sort_unstable_by(|(left_parent, left), (right_parent, right)| {
            let left_score = states[*left_parent].score + left.score;
            let right_score = states[*right_parent].score + right.score;
            right_score
                .total_cmp(&left_score)
                .then_with(|| left.child.cmp(&right.child))
                .then_with(|| left_parent.cmp(right_parent))
        });
        for (parent, edge) in expanded {
            if reference_ancestry_contains(&states, parent, edge.child) {
                cycles += 1;
                continue;
            }
            let candidate = ReferenceState {
                occurrence_id: edge.child,
                point_id: edge.child,
                parent: Some(parent),
                hop: states[parent].hop + 1,
                score: states[parent].score + edge.score,
            };
            if let Some(existing_id) = dominant.get(&edge.child).copied() {
                let existing = &states[existing_id];
                let ordering = existing
                    .score
                    .total_cmp(&candidate.score)
                    .then_with(|| candidate.hop.cmp(&existing.hop))
                    .then_with(|| candidate.parent.cmp(&existing.parent));
                if ordering != Ordering::Less {
                    dominated += 1;
                    continue;
                }
            }
            let state_id = states.len();
            states.push(candidate);
            dominant.insert(edge.child, state_id);
            frontier.push(state_id);
        }
    }
    let mut selected = dominant.values().copied().collect::<Vec<_>>();
    selected.sort_unstable_by(|left, right| compare_reference(&states, *left, *right));
    let hits = selected
        .into_iter()
        .map(|state_id| {
            let state = &states[state_id];
            let mut path = Vec::new();
            let mut cursor = Some(state_id);
            while let Some(current) = cursor {
                path.push(states[current].occurrence_id);
                cursor = states[current].parent;
            }
            path.reverse();
            ReferenceHit {
                occurrence_id: state.occurrence_id,
                point_id: state.point_id,
                score_bits: state.score.to_bits(),
                path,
            }
        })
        .collect();
    ReferenceOutcome {
        hits,
        provider_calls,
        vector_expansions,
        dominated,
        cycles,
    }
}

fn compare_reference(states: &[ReferenceState], left: usize, right: usize) -> Ordering {
    states[right]
        .score
        .total_cmp(&states[left].score)
        .then_with(|| states[left].occurrence_id.cmp(&states[right].occurrence_id))
        .then_with(|| left.cmp(&right))
}

fn reference_ancestry_contains(
    states: &[ReferenceState],
    parent: usize,
    occurrence_id: u64,
) -> bool {
    let mut cursor = Some(parent);
    while let Some(current) = cursor {
        if states[current].occurrence_id == occurrence_id {
            return true;
        }
        cursor = states[current].parent;
    }
    false
}

fn hash_reference_outcome(outcome: &ReferenceOutcome) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for hit in &outcome.hits {
        hash = fnv1a(hash, &hit.occurrence_id.to_le_bytes());
        hash = fnv1a(hash, &hit.point_id.to_le_bytes());
        hash = fnv1a(hash, &hit.score_bits.to_le_bytes());
        for occurrence_id in &hit.path {
            hash = fnv1a(hash, &occurrence_id.to_le_bytes());
        }
        hash = fnv1a(hash, &[0xff]);
    }
    for value in [
        outcome.provider_calls,
        outcome.vector_expansions,
        outcome.dominated,
        outcome.cycles,
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
    (0..P16_QUERY_COUNT)
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
