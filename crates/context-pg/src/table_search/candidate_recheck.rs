//! Batched candidate recheck for collection-backed search.

use std::{cmp::Reverse, collections::BinaryHeap};

use context_core::{DistanceMetric, SearchLimit};
use context_index::{HnswGraph, HnswGraphNodeSnapshot, HnswNodeId, HnswPointId};
use context_storage::{
    HnswGraphArtifactRecord, HnswGraphPayloadError, QuantizedHnswGraphView,
    decode_hnsw_graph_payload_versioned,
};
use pgrx::datum::DatumWithOid;
use pgrx::prelude::*;

use crate::Vector;
use crate::error::{raise_core_error, raise_sql_error};

use super::named::{resolve_registered_vector_by_name, vector_name_from_sql};
use super::{
    FilterPredicatePlan, distance_function, load_filter_fields, push_filter_parameter_args,
    quote_identifier, quote_qualified_identifier, require_collection_owner,
    require_table_select_privilege, resolve_collection, resolve_filter_plan,
    resolve_registered_vector, search_limit_from_sql, table_search_rows_from_spi,
    validate_search_drift,
};

#[pg_extern(schema = "pgcontext", name = "search")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn search_collection_candidates(
    collection: String,
    vector: Vector,
    candidate_point_ids: Vec<i64>,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f32),
    ),
> {
    let collection_name = match context_core::CollectionName::new(collection) {
        Ok(collection_name) => collection_name,
        Err(error) => raise_core_error(error),
    };
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector(&collection_name, collection.collection_id);
    validate_search_drift(collection.collection_id, &mut registered_vector);
    require_table_select_privilege(&registered_vector);

    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    crate::collection_limits::enforce_candidate_budget(
        collection.collection_id,
        &collection_name,
        candidate_point_ids.len(),
    );
    let rows = recheck_candidate_points(
        collection.collection_id,
        &registered_vector,
        vector,
        candidate_point_ids,
        None,
        limit,
    );
    TableIterator::new(rows)
}

#[pg_extern(schema = "pgcontext", name = "search")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn search_collection_named_vector_candidates(
    collection: String,
    vector_name: String,
    vector: Vector,
    candidate_point_ids: Vec<i64>,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f32),
    ),
> {
    let collection_name = match context_core::CollectionName::new(collection) {
        Ok(collection_name) => collection_name,
        Err(error) => raise_core_error(error),
    };
    let vector_name = vector_name_from_sql(vector_name);
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector_by_name(&collection_name, collection.collection_id, &vector_name);
    validate_search_drift(collection.collection_id, &mut registered_vector);
    require_table_select_privilege(&registered_vector);

    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    crate::collection_limits::enforce_candidate_budget(
        collection.collection_id,
        &collection_name,
        candidate_point_ids.len(),
    );
    let rows = recheck_candidate_points(
        collection.collection_id,
        &registered_vector,
        vector,
        candidate_point_ids,
        None,
        limit,
    );
    TableIterator::new(rows)
}

#[pg_extern(schema = "pgcontext", name = "search")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn search_collection_filtered_candidates(
    collection: String,
    vector: Vector,
    filter: Option<String>,
    candidate_point_ids: Vec<i64>,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f32),
    ),
> {
    let collection_name = match context_core::CollectionName::new(collection) {
        Ok(collection_name) => collection_name,
        Err(error) => raise_core_error(error),
    };
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector(&collection_name, collection.collection_id);
    validate_search_drift(collection.collection_id, &mut registered_vector);
    require_table_select_privilege(&registered_vector);

    let fields = load_filter_fields(collection.collection_id);
    let filter_plan = resolve_filter_plan(&fields, filter.as_deref(), 4);
    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    crate::collection_limits::enforce_candidate_budget(
        collection.collection_id,
        &collection_name,
        candidate_point_ids.len(),
    );
    let rows = recheck_candidate_points(
        collection.collection_id,
        &registered_vector,
        vector,
        candidate_point_ids,
        filter_plan,
        limit,
    );
    TableIterator::new(rows)
}

#[pg_extern(schema = "pgcontext", name = "search")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn search_collection_named_vector_filtered_candidates(
    collection: String,
    vector_name: String,
    vector: Vector,
    filter: Option<String>,
    candidate_point_ids: Vec<i64>,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f32),
    ),
> {
    let collection_name = match context_core::CollectionName::new(collection) {
        Ok(collection_name) => collection_name,
        Err(error) => raise_core_error(error),
    };
    let vector_name = vector_name_from_sql(vector_name);
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector_by_name(&collection_name, collection.collection_id, &vector_name);
    validate_search_drift(collection.collection_id, &mut registered_vector);
    require_table_select_privilege(&registered_vector);

    let fields = load_filter_fields(collection.collection_id);
    let filter_plan = resolve_filter_plan(&fields, filter.as_deref(), 5);
    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    crate::collection_limits::enforce_candidate_budget(
        collection.collection_id,
        &collection_name,
        candidate_point_ids.len(),
    );
    let rows = recheck_candidate_points(
        collection.collection_id,
        &registered_vector,
        vector,
        candidate_point_ids,
        filter_plan,
        limit,
    );
    TableIterator::new(rows)
}

#[pg_extern(schema = "pgcontext")]
#[search_path(pg_catalog, pgcontext, public)]
pub fn search_mmap_hnsw_artifact(
    collection: String,
    artifact_name: String,
    vector: Vector,
    max_mapped_bytes: i64,
    candidate_limit: i32,
    limit: i32,
) -> TableIterator<
    'static,
    (
        name!(point_id, i64),
        name!(source_key, String),
        name!(score, f32),
    ),
> {
    let collection_name = match context_core::CollectionName::new(collection) {
        Ok(collection_name) => collection_name,
        Err(error) => raise_core_error(error),
    };
    let collection = resolve_collection(&collection_name);
    require_collection_owner(&collection, &collection_name);
    let mut registered_vector =
        resolve_registered_vector(&collection_name, collection.collection_id);
    validate_search_drift(collection.collection_id, &mut registered_vector);
    require_table_select_privilege(&registered_vector);

    let limit = search_limit_from_sql(limit);
    crate::collection_limits::enforce_search_limit(
        collection.collection_id,
        &collection_name,
        limit.get(),
    );
    let candidate_limit = search_limit_from_sql(candidate_limit);
    crate::collection_limits::enforce_candidate_budget(
        collection.collection_id,
        &collection_name,
        candidate_limit.get(),
    );

    let query = match vector.to_dense() {
        Ok(query) => query,
        Err(error) => raise_core_error(error),
    };
    let metric = registered_vector.metric;
    // The payload load stays behind the security-definer SQL boundary of
    // `pgcontext.artifact_segment_mmap_payload`, which owns data-directory
    // resolution and reader-pin bookkeeping. The search function itself keeps
    // invoker rights so source-table ACLs and RLS stay authoritative for the
    // recheck below.
    let payload =
        load_mmap_artifact_payload(collection_name.as_str(), &artifact_name, max_mapped_bytes);
    let quantized_view = QuantizedHnswGraphView::attach(&payload)
        .unwrap_or_else(|error| raise_hnsw_graph_payload_error(error));
    let (generation_high_water, mut candidates) = if let Some(view) = quantized_view {
        if candidate_limit.get() <= limit.get() {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_INVALID_PARAMETER_VALUE,
                format!(
                    "quantized mmap HNSW candidate_limit {} must exceed final limit {}",
                    candidate_limit.get(),
                    limit.get()
                ),
            );
        }
        let generation_high_water = (0..view.len())
            .filter_map(|node_id| view.node(node_id))
            .map(|node| node.point_id())
            .max()
            .unwrap_or_default();
        let candidates =
            mmap_quantized_hnsw_candidates(&view, &query, metric, candidate_limit.get());
        (generation_high_water, candidates)
    } else {
        let graph_payload = decode_hnsw_graph_payload_versioned(&payload)
            .unwrap_or_else(|error| raise_hnsw_graph_payload_error(error));
        let generation_high_water = graph_payload
            .records()
            .iter()
            .map(HnswGraphArtifactRecord::point_id)
            .max()
            .unwrap_or_default();
        let candidates = mmap_hnsw_candidates(
            graph_payload.into_records(),
            &query,
            metric,
            candidate_limit.get(),
        );
        (generation_high_water, candidates)
    };
    candidates.extend(mmap_delta_candidates(
        collection.collection_id,
        &registered_vector,
        &vector,
        generation_high_water,
        candidate_limit.get(),
    ));
    candidates.sort_by(|(left_id, left_score), (right_id, right_score)| {
        left_score
            .total_cmp(right_score)
            .then_with(|| left_id.cmp(right_id))
    });
    candidates.dedup_by_key(|(point_id, _)| *point_id);
    candidates.truncate(candidate_limit.get());
    let candidate_point_ids = candidates
        .into_iter()
        .map(|(point_id, _)| point_id)
        .collect();

    let rows = recheck_candidate_points(
        collection.collection_id,
        &registered_vector,
        vector,
        candidate_point_ids,
        None,
        limit,
    );
    TableIterator::new(rows)
}

fn recheck_candidate_points(
    collection_id: i64,
    registered_vector: &super::SearchVector,
    query: Vector,
    candidate_point_ids: Vec<i64>,
    filter_plan: Option<FilterPredicatePlan>,
    limit: SearchLimit,
) -> Vec<(i64, String, f32)> {
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let vector_column = quote_identifier(&registered_vector.vector_column_name);
    let distance_function = distance_function(registered_vector.metric);
    let filter_sql = filter_plan
        .as_ref()
        .map(|plan| format!(" AND {}", plan.sql))
        .unwrap_or_default();
    let sql = format!(
        "WITH candidate_points AS MATERIALIZED (
             SELECT DISTINCT points.point_id,
                    points.source_key
               FROM pgcontext._visible_collection_points AS points
               JOIN {table_name} AS source ON source.id::text = points.source_key
              WHERE points.collection_id = $2
                AND points.deleted_at IS NULL
                AND points.point_id = ANY($3::bigint[])
                {filter_sql}
         )
         SELECT candidates.point_id,
                candidates.source_key,
                pgcontext.{distance_function}(source.{vector_column}, $1) AS score
           FROM candidate_points AS candidates
           JOIN {table_name} AS source ON source.id::text = candidates.source_key
          ORDER BY score ASC, candidates.point_id ASC
          LIMIT $4"
    );
    let limit = i64::try_from(limit.get()).unwrap_or(i64::MAX);
    let parameter_values = filter_plan
        .as_ref()
        .map(|plan| plan.parameters.as_slice())
        .unwrap_or(&[]);
    let mut args = Vec::<DatumWithOid<'_>>::with_capacity(4 + parameter_values.len());
    args.push(query.into());
    args.push(collection_id.into());
    args.push(candidate_point_ids.into());
    args.push(limit.into());
    push_filter_parameter_args(&mut args, parameter_values);

    Spi::connect(|client| {
        let rows = match client.select(&sql, Some(limit), &args) {
            Ok(rows) => rows,
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to recheck candidate points: {error}"),
            ),
        };
        table_search_rows_from_spi(rows, "candidate recheck")
    })
}

fn load_mmap_artifact_payload(
    collection: &str,
    artifact_name: &str,
    max_mapped_bytes: i64,
) -> Vec<u8> {
    crate::artifact_segments::with_mmap_payload_access(|| {
        match Spi::get_one_with_args::<Vec<u8>>(
            "SELECT payload
               FROM pgcontext.artifact_segment_mmap_payload($1, $2, $3)",
            &[
                collection.into(),
                artifact_name.into(),
                max_mapped_bytes.into(),
            ],
        ) {
            Ok(Some(payload)) => payload,
            Ok(None) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("mmap artifact payload query returned no row for artifact {artifact_name}"),
            ),
            Err(error) => raise_sql_error(
                PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                format!("failed to load mmap artifact payload: {error}"),
            ),
        }
    })
}

fn mmap_hnsw_candidates(
    records: Vec<HnswGraphArtifactRecord>,
    query: &context_core::DenseVector,
    metric: DistanceMetric,
    candidate_limit: usize,
) -> Vec<(i64, f32)> {
    if records
        .iter()
        .any(|record| !record.base_neighbors().is_empty())
    {
        return mmap_hnsw_graph_candidates(records, query, metric, candidate_limit);
    }

    let mut candidates = records
        .iter()
        .map(|record| {
            let score = match metric.distance(record.vector(), query) {
                Ok(score) => score,
                Err(error) => raise_core_error(error),
            };
            let point_id = i64::try_from(record.point_id()).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "HNSW graph artifact point id exceeds PostgreSQL bigint range",
                )
            });
            (point_id, score)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(
        |(left_point_id, left_score), (right_point_id, right_score)| {
            left_score
                .total_cmp(right_score)
                .then_with(|| left_point_id.cmp(right_point_id))
        },
    );
    candidates.into_iter().take(candidate_limit).collect()
}

fn mmap_hnsw_graph_candidates(
    records: Vec<HnswGraphArtifactRecord>,
    query: &context_core::DenseVector,
    metric: DistanceMetric,
    candidate_limit: usize,
) -> Vec<(i64, f32)> {
    let snapshots = records
        .into_iter()
        .map(|record| {
            let (node_id, point_id, vector, neighbors) = record.into_parts();
            HnswGraphNodeSnapshot::new(
                HnswNodeId::new(node_id as usize),
                HnswPointId::new(point_id),
                vector,
                neighbors
                    .into_iter()
                    .map(|neighbor| HnswNodeId::new(neighbor as usize))
                    .collect(),
            )
        })
        .collect();
    let config = crate::settings::hnsw_config_from_gucs();
    let graph = HnswGraph::from_base_layer_snapshots(metric, config, snapshots)
        .unwrap_or_else(|error| raise_hnsw_graph_reconstruction_error(error));
    let limit = SearchLimit::new(candidate_limit).unwrap_or_else(|error| raise_core_error(error));
    graph
        .search(query, limit)
        .unwrap_or_else(|error| raise_hnsw_graph_reconstruction_error(error))
        .into_iter()
        .map(|result| {
            let point_id = i64::try_from(result.point_id().get()).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "HNSW graph artifact point id exceeds PostgreSQL bigint range",
                )
            });
            (point_id, result.score())
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct EncodedCandidate {
    node_id: usize,
    score: f32,
}

impl Eq for EncodedCandidate {}

impl Ord for EncodedCandidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.score
            .total_cmp(&other.score)
            .then_with(|| self.node_id.cmp(&other.node_id))
    }
}

impl PartialOrd for EncodedCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

fn mmap_quantized_hnsw_candidates(
    graph: &QuantizedHnswGraphView<'_>,
    query: &context_core::DenseVector,
    metric: DistanceMetric,
    candidate_limit: usize,
) -> Vec<(i64, f32)> {
    if query.dimension() != graph.dimensions() {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_EXCEPTION,
            format!(
                "quantized HNSW query dimensions mismatch: expected {}, got {}",
                graph.dimensions(),
                query.dimension()
            ),
        );
    }
    let config = crate::settings::hnsw_config_from_gucs();
    let search_width = config.ef_search().max(candidate_limit);
    let has_edges = (0..graph.len()).any(|node_id| {
        graph
            .node(node_id)
            .is_some_and(|node| node.neighbors().next().is_some())
    });
    let candidates = if has_edges {
        traverse_quantized_base_layer(graph, query, metric, search_width)
    } else {
        (0..graph.len())
            .map(|node_id| score_quantized_node(graph, query, metric, node_id))
            .collect()
    };
    let mut candidates = candidates
        .into_iter()
        .map(|candidate| {
            let node = graph.node(candidate.node_id).unwrap_or_else(|| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "quantized HNSW candidate node is missing",
                )
            });
            let point_id = i64::try_from(node.point_id()).unwrap_or_else(|_| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "HNSW graph artifact point id exceeds PostgreSQL bigint range",
                )
            });
            (point_id, candidate.score)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(
        |(left_point_id, left_score), (right_point_id, right_score)| {
            left_score
                .total_cmp(right_score)
                .then_with(|| left_point_id.cmp(right_point_id))
        },
    );
    candidates.truncate(candidate_limit);
    candidates
}

fn traverse_quantized_base_layer(
    graph: &QuantizedHnswGraphView<'_>,
    query: &context_core::DenseVector,
    metric: DistanceMetric,
    search_width: usize,
) -> Vec<EncodedCandidate> {
    let entry = score_quantized_node(graph, query, metric, 0);
    let mut pending = BinaryHeap::from([Reverse(entry)]);
    let mut nearest = BinaryHeap::from([entry]);
    let mut visited = vec![false; graph.len()];
    visited[0] = true;

    while let Some(Reverse(candidate)) = pending.pop() {
        let worst = nearest.peek().map_or(f32::INFINITY, |item| item.score);
        if nearest.len() >= search_width && candidate.score > worst {
            break;
        }
        let node = graph.node(candidate.node_id).unwrap_or_else(|| {
            raise_sql_error(
                PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                "quantized HNSW traversal node is missing",
            )
        });
        for neighbor in node.neighbors() {
            let neighbor = neighbor as usize;
            let Some(was_visited) = visited.get_mut(neighbor) else {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
                    "quantized HNSW neighbor exceeds graph node count",
                );
            };
            if *was_visited {
                continue;
            }
            *was_visited = true;
            let scored = score_quantized_node(graph, query, metric, neighbor);
            let should_add = nearest.len() < search_width
                || nearest
                    .peek()
                    .is_some_and(|current_worst| scored < *current_worst);
            if should_add {
                pending.push(Reverse(scored));
                nearest.push(scored);
                if nearest.len() > search_width {
                    nearest.pop();
                }
            }
        }
    }
    nearest.into_sorted_vec()
}

fn score_quantized_node(
    graph: &QuantizedHnswGraphView<'_>,
    query: &context_core::DenseVector,
    metric: DistanceMetric,
    node_id: usize,
) -> EncodedCandidate {
    let node = graph.node(node_id).unwrap_or_else(|| {
        raise_sql_error(
            PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
            "quantized HNSW node is missing",
        )
    });
    let score = graph
        .codebook()
        .approximate_distance(query, node.code(), metric)
        .unwrap_or_else(|error| raise_hnsw_graph_payload_error(error));
    EncodedCandidate { node_id, score }
}

fn mmap_delta_candidates(
    collection_id: i64,
    registered_vector: &super::SearchVector,
    query: &Vector,
    generation_high_water: u64,
    candidate_limit: usize,
) -> Vec<(i64, f32)> {
    let high_water = i64::try_from(generation_high_water).unwrap_or(i64::MAX);
    let table_name = quote_qualified_identifier(
        &registered_vector.schema_name,
        &registered_vector.table_name,
    );
    let vector_column = quote_identifier(&registered_vector.vector_column_name);
    let distance_function = distance_function(registered_vector.metric);
    let sql = format!(
        "SELECT points.point_id,
                pgcontext.{distance_function}(source.{vector_column}, $1) AS score
           FROM pgcontext._visible_collection_points AS points
           JOIN {table_name} AS source ON source.id::text = points.source_key
          WHERE points.collection_id = $2
            AND points.deleted_at IS NULL
            AND points.point_id > $3
          ORDER BY score, points.point_id
          LIMIT $4"
    );
    let limit = i64::try_from(candidate_limit).unwrap_or(i64::MAX);
    Spi::connect(|client| {
        let rows = client
            .select(
                &sql,
                Some(limit),
                &[
                    query.clone().into(),
                    collection_id.into(),
                    high_water.into(),
                    limit.into(),
                ],
            )
            .unwrap_or_else(|error| {
                raise_sql_error(
                    PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                    format!("failed to search mmap mutable delta: {error}"),
                )
            });
        rows.into_iter()
            .map(|row| {
                (
                    row.get::<i64>(1).ok().flatten().unwrap_or_else(|| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            "mmap mutable delta returned a null point id",
                        )
                    }),
                    row.get::<f32>(2).ok().flatten().unwrap_or_else(|| {
                        raise_sql_error(
                            PgSqlErrorCode::ERRCODE_INTERNAL_ERROR,
                            "mmap mutable delta returned a null score",
                        )
                    }),
                )
            })
            .collect()
    })
}

fn raise_hnsw_graph_reconstruction_error(error: context_index::HnswError) -> ! {
    raise_sql_error(
        PgSqlErrorCode::ERRCODE_DATA_CORRUPTED,
        format!("invalid mmap HNSW graph: {error}"),
    )
}

fn raise_hnsw_graph_payload_error(error: HnswGraphPayloadError) -> ! {
    raise_sql_error(PgSqlErrorCode::ERRCODE_DATA_CORRUPTED, error.to_string())
}
