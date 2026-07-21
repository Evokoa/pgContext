//! SQL object lifecycle registry entries.

use super::{
    SqlContractObject, SqlLifecycle,
    contract_catalog_objects::{CATALOG_SQL_CONTRACT_OBJECTS, CATALOG_SQL_CONTRACT_OBJECTS_LEN},
};

const FUNCTION_SQL_CONTRACT_OBJECTS_LEN: usize = 239;
const SQL_CONTRACT_OBJECTS_LEN: usize =
    CATALOG_SQL_CONTRACT_OBJECTS_LEN + FUNCTION_SQL_CONTRACT_OBJECTS_LEN;

static SQL_CONTRACT_OBJECTS_ARRAY: [SqlContractObject; SQL_CONTRACT_OBJECTS_LEN] =
    build_sql_contract_objects();

/// SQL objects intentionally covered by the first release registry.
///
/// The registry classifies installed objects by compatibility lifecycle. Tests
/// compare this list with PostgreSQL catalogs so SQL-visible additions require a
/// conscious stable, experimental, or internal decision.
pub(crate) const SQL_CONTRACT_OBJECTS: &[SqlContractObject] = &SQL_CONTRACT_OBJECTS_ARRAY;

const fn build_sql_contract_objects() -> [SqlContractObject; SQL_CONTRACT_OBJECTS_LEN] {
    let mut objects = [CATALOG_SQL_CONTRACT_OBJECTS[0]; SQL_CONTRACT_OBJECTS_LEN];
    let mut output_index = 0;
    let mut input_index = 0;

    while input_index < CATALOG_SQL_CONTRACT_OBJECTS_LEN {
        objects[output_index] = CATALOG_SQL_CONTRACT_OBJECTS[input_index];
        output_index += 1;
        input_index += 1;
    }

    input_index = 0;
    while input_index < FUNCTION_SQL_CONTRACT_OBJECTS_LEN {
        objects[output_index] = FUNCTION_SQL_CONTRACT_OBJECTS[input_index];
        output_index += 1;
        input_index += 1;
    }

    objects
}

#[rustfmt::skip]
const FUNCTION_SQL_CONTRACT_OBJECTS: &[SqlContractObject; FUNCTION_SQL_CONTRACT_OBJECTS_LEN] = &[
    SqlContractObject::function(
        "collection_info",
        "collection_name text",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "collection_limits",
        "collection_name text",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "collection_vectors",
        "collection_name text",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "collection_sparse_vectors",
        "collection_name text",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("collection_aliases", "", SqlLifecycle::Stable),
    SqlContractObject::function(
        "clear_payload",
        "collection_name text, source_keys text[]",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "backfill_points",
        "collection_name text, batch_size integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "bulk_delete_points",
        "collection_name text, source_keys text[], batch_size integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "bulk_upsert_points",
        "collection_name text, source_keys text[], batch_size integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "configure_collection_limits",
        "collection_name text, strict_mode boolean, max_dimensions integer, max_vectors integer, max_points bigint, max_filter_nodes integer, max_search_limit integer, max_candidate_budget integer, query_timeout_ms integer, max_index_memory_bytes bigint",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "configure_vector",
        "collection_name text, vector_name text, hnsw_options jsonb, quantization_options jsonb, status text",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "configure_sparse_vector",
        "collection_name text, vector_name text, storage_options jsonb, index_options jsonb, status text",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("build_jobs", "collection text", SqlLifecycle::Experimental),
    SqlContractObject::function(
        "_reject_build_job_progress_regression",
        "",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "_enforce_build_job_terminal_state",
        "",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "cosine_distance",
        "\"left\" vector, \"right\" vector",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "create_collection",
        "collection_name text",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "create_collection",
        "collection_name text, table_name text",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "create_collection_alias",
        "alias_name text, target_collection_name text",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "create_embedding_migration",
        "collection text, source_model_name text, source_model_version text, target_model_name text, target_model_version text, total_points bigint",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function("count", "collection text", SqlLifecycle::Stable),
    SqlContractObject::function(
        "count",
        "collection text, filter text",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "delete_points",
        "collection_name text, source_keys text[]",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "delete_payload",
        "collection_name text, source_keys text[], payload_keys text[]",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "discover",
        "collection text, context_point_ids bigint[], \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "drop_collection",
        "collection_name text",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "drop_collection_alias",
        "alias_name text",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function("embedding_migrations", "", SqlLifecycle::Stable),
    SqlContractObject::function(
        "estimate_index_memory",
        "index_name text",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "explore",
        "collection text, context_point_ids bigint[], \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "explain",
        "collection text, text_column text",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "facet",
        "collection text, field text, filter text, \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "grouped_search",
        "collection text, vector vector, group_by text, group_limit integer, \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "grouped_search",
        "collection text, vector_name text, vector vector, group_by text, group_limit integer, \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function("hamming_distance", "\"left\" bit, \"right\" bit", SqlLifecycle::Experimental),
    SqlContractObject::function("hamming_distance", "\"left\" bit varying, \"right\" bit varying", SqlLifecycle::Experimental),
    SqlContractObject::function("avg", "vector", SqlLifecycle::Stable),
    SqlContractObject::function("avg", "halfvec", SqlLifecycle::Experimental),
    SqlContractObject::function("avg", "sparsevec", SqlLifecycle::Experimental),
    SqlContractObject::function("bit_and", "bitvec", SqlLifecycle::Experimental),
    SqlContractObject::function("bit_or", "bitvec", SqlLifecycle::Experimental),
    SqlContractObject::function("bitvec", "input text", SqlLifecycle::Experimental),
    SqlContractObject::function(
        "bitvec_and_transition",
        "state boolean[], value bitvec",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "bitvec_bits_final",
        "state boolean[]",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function("bitvec_cmp", "\"left\" bitvec, \"right\" bitvec", SqlLifecycle::Internal),
    SqlContractObject::function("bitvec_dims", "vector bitvec", SqlLifecycle::Experimental),
    SqlContractObject::function(
        "bitvec_enforce_typmod",
        "vector bitvec, typmod integer, _explicit boolean",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function("bitvec_eq", "\"left\" bitvec, \"right\" bitvec", SqlLifecycle::Internal),
    SqlContractObject::function(
        "bitvec_from_bool_array",
        "bits boolean[]",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function("bitvec_ge", "\"left\" bitvec, \"right\" bitvec", SqlLifecycle::Internal),
    SqlContractObject::function("bitvec_gt", "\"left\" bitvec, \"right\" bitvec", SqlLifecycle::Internal),
    SqlContractObject::function("bitvec_hamming_distance", "\"left\" bitvec, \"right\" bitvec", SqlLifecycle::Experimental),
    SqlContractObject::function("bitvec_jaccard_distance", "\"left\" bitvec, \"right\" bitvec", SqlLifecycle::Experimental),
    SqlContractObject::function("bitvec_le", "\"left\" bitvec, \"right\" bitvec", SqlLifecycle::Internal),
    SqlContractObject::function("bitvec_lt", "\"left\" bitvec, \"right\" bitvec", SqlLifecycle::Internal),
    SqlContractObject::function("bitvec_ne", "\"left\" bitvec, \"right\" bitvec", SqlLifecycle::Internal),
    SqlContractObject::function(
        "bitvec_or_transition",
        "state boolean[], value bitvec",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function("bitvec_typmod_in", "modifiers cstring[]", SqlLifecycle::Internal),
    SqlContractObject::function("bitvec_typmod_out", "typmod integer", SqlLifecycle::Internal),
    SqlContractObject::function(
        "bitvec_to_bool_array",
        "vector bitvec",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "binary_quantize",
        "vector vector",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("sum", "vector", SqlLifecycle::Stable),
    SqlContractObject::function("sum", "halfvec", SqlLifecycle::Experimental),
    SqlContractObject::function("sum", "sparsevec", SqlLifecycle::Experimental),
    SqlContractObject::function("halfvec", "input text", SqlLifecycle::Experimental),
    SqlContractObject::function("halfvec_cmp", "\"left\" halfvec, \"right\" halfvec", SqlLifecycle::Internal),
    SqlContractObject::function(
        "halfvec_cosine_distance",
        "\"left\" halfvec, \"right\" halfvec",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("halfvec_dims", "vector halfvec", SqlLifecycle::Experimental),
    SqlContractObject::function("halfvec_enforce_typmod", "vector halfvec, typmod integer, _explicit boolean", SqlLifecycle::Internal),
    SqlContractObject::function("halfvec_eq", "\"left\" halfvec, \"right\" halfvec", SqlLifecycle::Internal),
    SqlContractObject::function("halfvec_avg_final", "state real[]", SqlLifecycle::Internal),
    SqlContractObject::function(
        "halfvec_from_double_array",
        "\"values\" double precision[]",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "halfvec_from_integer_array",
        "\"values\" integer[]",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "halfvec_from_real_array",
        "\"values\" real[]",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function("halfvec_ge", "\"left\" halfvec, \"right\" halfvec", SqlLifecycle::Internal),
    SqlContractObject::function("halfvec_gt", "\"left\" halfvec, \"right\" halfvec", SqlLifecycle::Internal),
    SqlContractObject::function("halfvec_typmod_in", "modifiers cstring[]", SqlLifecycle::Internal),
    SqlContractObject::function("halfvec_typmod_out", "typmod integer", SqlLifecycle::Internal),
    SqlContractObject::function(
        "halfvec_inner_product",
        "\"left\" halfvec, \"right\" halfvec",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "halfvec_l1_distance",
        "\"left\" halfvec, \"right\" halfvec",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("halfvec_le", "\"left\" halfvec, \"right\" halfvec", SqlLifecycle::Internal),
    SqlContractObject::function("halfvec_sum_final", "state real[]", SqlLifecycle::Internal),
    SqlContractObject::function(
        "halfvec_sum_transition",
        "state real[], value halfvec",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "halfvec_to_real_array",
        "vector halfvec",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "halfvec_to_vector",
        "vector halfvec",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("halfvec_lt", "\"left\" halfvec, \"right\" halfvec", SqlLifecycle::Internal),
    SqlContractObject::function(
        "halfvec_l2_distance",
        "\"left\" halfvec, \"right\" halfvec",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("halfvec_ne", "\"left\" halfvec, \"right\" halfvec", SqlLifecycle::Internal),
    SqlContractObject::function(
        "halfvec_negative_inner_product",
        "\"left\" halfvec, \"right\" halfvec",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("hnsw_handler", "internal", SqlLifecycle::Experimental),
    SqlContractObject::function("index_advisor", "collection text", SqlLifecycle::Stable),
    SqlContractObject::function("index_diagnostics", "index_name text", SqlLifecycle::Stable),
    SqlContractObject::function("index_status", "index_name text", SqlLifecycle::Stable),
    SqlContractObject::function(
        "inner_product",
        "\"left\" vector, \"right\" vector",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "hnsw_l2_distance",
        "\"left\" vector, \"right\" vector",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function("jaccard_distance", "\"left\" bit, \"right\" bit", SqlLifecycle::Experimental),
    SqlContractObject::function("jaccard_distance", "\"left\" bit varying, \"right\" bit varying", SqlLifecycle::Experimental),
    SqlContractObject::function(
        "l1_distance",
        "\"left\" vector, \"right\" vector",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "l2_distance",
        "\"left\" vector, \"right\" vector",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function("model_versions", "", SqlLifecycle::Stable),
    SqlContractObject::function(
        "negative_inner_product",
        "\"left\" vector, \"right\" vector",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "optimization_status",
        "collection text",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "product_quantize",
        "vector vector, subvector_dimensions integer, codebooks jsonb",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "product_reconstruct",
        "codes bytea, subvector_dimensions integer, codebooks jsonb",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "query",
        "collection text, vector vector, text_query text, text_column text, \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "query",
        "collection text, vector vector, sparse_vector_name text, sparse_query sparsevec, \"limit\" integer",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "query_discover",
        "context_point_ids bigint[], \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "query_formula",
        "branch jsonb, formula text",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function("query_lookup", "point_ids bigint[]", SqlLifecycle::Stable),
    SqlContractObject::function(
        "query_nearest",
        "vector vector, \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function("query_prefetch", "branches jsonb[]", SqlLifecycle::Stable),
    SqlContractObject::function(
        "query_recommend",
        "positive_point_ids bigint[], negative_point_ids bigint[], \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "query_rerank",
        "branch jsonb, \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "query_score_threshold",
        "branch jsonb, min_score double precision, max_score double precision",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "query_weight",
        "branch jsonb, weight double precision",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "recommend",
        "collection text, positive_point_ids bigint[], negative_point_ids bigint[], \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "recommend",
        "collection text, positive_vectors vector[], negative_vectors vector[], \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function("query_cohort_stats", "", SqlLifecycle::Stable),
    SqlContractObject::function(
        "recall_check",
        "exact_point_ids bigint[], candidate_point_ids bigint[], min_recall double precision",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "request_build_cancel",
        "build_job_id bigint",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "retry_build_job",
        "build_job_id bigint",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "run_build_job",
        "build_job_id bigint, units_per_step bigint",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "record_query_stat",
        "collection text, cohort text, query_kind text, result_count bigint, candidate_count bigint, latency_ms double precision",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "record_query_stat",
        "collection text, cohort text, query_kind text, result_count bigint, candidates_considered bigint, rows_rechecked bigint, rows_pruned bigint, recall_threshold double precision, recall_achieved double precision, latency_ms double precision, lifecycle_state pgcontext.querylifecyclestate",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "register_filter_column",
        "collection_name text, filter_key text, column_name text",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "register_jsonb_path",
        "collection_name text, filter_key text, column_name text, path text[]",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "register_model_version",
        "collection text, model_name text, model_version text, dimensions integer, metric text",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "register_vector",
        "collection_name text, vector_name text, vector_column text, dimensions integer, metric text",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "register_sparse_vector",
        "collection_name text, vector_name text, vector_column text, dimensions integer, metric text",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "scroll",
        "collection text, cursor text, \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "scalar_quantize",
        "vector vector, min real, max real, levels integer",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "scalar_reconstruct",
        "codes bytea, min real, max real, levels integer",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("sparsevec", "input text", SqlLifecycle::Experimental),
    SqlContractObject::function("sparsevec_cmp", "\"left\" sparsevec, \"right\" sparsevec", SqlLifecycle::Internal),
    SqlContractObject::function(
        "sparsevec_dims",
        "vector sparsevec",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("sparsevec_enforce_typmod", "vector sparsevec, typmod integer, _explicit boolean", SqlLifecycle::Internal),
    SqlContractObject::function("sparsevec_eq", "\"left\" sparsevec, \"right\" sparsevec", SqlLifecycle::Internal),
    SqlContractObject::function(
        "sparsevec_from_arrays",
        "indices integer[], \"values\" real[], dimensions integer",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "sparsevec_from_real_array",
        "\"values\" real[]",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "sparsevec_from_vector",
        "vector vector",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function("sparsevec_ge", "\"left\" sparsevec, \"right\" sparsevec", SqlLifecycle::Internal),
    SqlContractObject::function("sparsevec_gt", "\"left\" sparsevec, \"right\" sparsevec", SqlLifecycle::Internal),
    SqlContractObject::function(
        "sparsevec_indices",
        "vector sparsevec",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "sparsevec_inner_product",
        "\"left\" sparsevec, \"right\" sparsevec",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "sparsevec_cosine_distance",
        "\"left\" sparsevec, \"right\" sparsevec",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "sparsevec_l1_distance",
        "\"left\" sparsevec, \"right\" sparsevec",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("sparsevec_le", "\"left\" sparsevec, \"right\" sparsevec", SqlLifecycle::Internal),
    SqlContractObject::function("sparsevec_typmod_in", "modifiers cstring[]", SqlLifecycle::Internal),
    SqlContractObject::function("sparsevec_typmod_out", "typmod integer", SqlLifecycle::Internal),
    SqlContractObject::function(
        "sparsevec_l2_distance",
        "\"left\" sparsevec, \"right\" sparsevec",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "sparsevec_to_vector",
        "vector sparsevec",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function("sparsevec_lt", "\"left\" sparsevec, \"right\" sparsevec", SqlLifecycle::Internal),
    SqlContractObject::function("sparsevec_ne", "\"left\" sparsevec, \"right\" sparsevec", SqlLifecycle::Internal),
    SqlContractObject::function(
        "sparsevec_negative_inner_product",
        "\"left\" sparsevec, \"right\" sparsevec",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "sparsevec_avg_final",
        "state real[]",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "sparsevec_sum_final",
        "state real[]",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "sparsevec_sum_transition",
        "state real[], value sparsevec",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "sparsevec_to_real_array",
        "vector sparsevec",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "sparsevec_values",
        "vector sparsevec",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "search",
        "collection text, vector vector, \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "search",
        "collection text, vector_name text, vector vector, \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "search",
        "collection text, vector vector, filter text, \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "search",
        "collection text, vector_name text, vector vector, filter text, \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "search",
        "collection text, vector vector, candidate_point_ids bigint[], \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "search",
        "collection text, vector_name text, vector vector, candidate_point_ids bigint[], \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "search",
        "collection text, vector vector, filter text, candidate_point_ids bigint[], \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "search",
        "collection text, vector_name text, vector vector, filter text, candidate_point_ids bigint[], \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "set_payload",
        "collection_name text, source_keys text[], payload jsonb",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "search",
        "query vector, point_ids bigint[], vectors vector[], metric text, \"limit\" integer",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function(
        "search_sparse",
        "query sparsevec, point_ids bigint[], vectors sparsevec[], metric text, \"limit\" integer",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("artifact_segments", "collection text", SqlLifecycle::Experimental),
    SqlContractObject::function("artifact_segment_memory", "collection text", SqlLifecycle::Experimental),
    SqlContractObject::function("artifact_segment_diagnostics", "collection text", SqlLifecycle::Experimental),
    SqlContractObject::function(
        "artifact_segment_serving_readiness",
        "collection text, max_mapped_bytes bigint",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "artifact_segment_mmap_payload",
        "collection text, artifact_name text, max_mapped_bytes bigint",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "search_mmap_hnsw_artifact",
        "collection text, artifact_name text, vector vector, max_mapped_bytes bigint, candidate_limit integer, \"limit\" integer",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("cleanup_artifact_segments", "collection text, dry_run boolean", SqlLifecycle::Experimental),
    SqlContractObject::function("retire_artifact_segment", "artifact_id bigint", SqlLifecycle::Experimental),
    SqlContractObject::function(
        "search_sparse",
        "collection text, vector_name text, query sparsevec, \"limit\" integer",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "rerank_quantized_candidates",
        "query vector, point_ids bigint[], original_vectors vector[], metric text, \"limit\" integer",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("publish_artifact_segment", "build_job_id bigint, segment bytea", SqlLifecycle::Experimental),
    SqlContractObject::function("publish_artifact_segment_file", "build_job_id bigint, segment bytea", SqlLifecycle::Experimental),
    SqlContractObject::function(
        "rerank_late_interaction",
        "query_vectors vector[], point_ids bigint[], candidate_vectors vector[], candidate_offsets integer[], \"limit\" integer",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "search_late_interaction",
        "collection text, query_vectors vector[], vector_column text, \"limit\" integer",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("search_late_interaction_ann", "collection text, query_vectors vector[], vector_column text, token_table text, token_source_key_column text, token_vector_column text, candidates_per_query integer, \"limit\" integer", SqlLifecycle::Experimental),
    SqlContractObject::function("encode_artifact_segment", "kind text, payload bytea", SqlLifecycle::Experimental),
    SqlContractObject::function(
        "validate_hnsw_graph_artifact",
        "segment bytea",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "explain_late_interaction",
        "collection text, query_vectors vector[], vector_column text",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("explain_late_interaction_ann", "collection text, query_vectors vector[], vector_column text, token_table text, token_source_key_column text, token_vector_column text, candidates_per_query integer", SqlLifecycle::Experimental),
    SqlContractObject::function(
        "start_build_job",
        "collection text, artifact_kind text, artifact_name text, target_name text, total_units bigint",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("telemetry", "", SqlLifecycle::Stable),
    SqlContractObject::function(
        "update_build_job",
        "build_job_id bigint, processed_units bigint, status text, error_message text",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "update_embedding_migration",
        "migration_id bigint, processed_points bigint, status text",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function("validate_artifact_segment", "segment bytea", SqlLifecycle::Experimental),
    SqlContractObject::function(
        "upsert_points",
        "collection_name text, source_keys text[]",
        SqlLifecycle::Stable,
    ),
    SqlContractObject::function("vacuum_advice", "index_name text", SqlLifecycle::Stable),
    SqlContractObject::function("vector_avg_final", "state real[]", SqlLifecycle::Internal),
    SqlContractObject::function(
        "vector_cmp",
        "\"left\" vector, \"right\" vector",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function("vector_dims", "vector vector", SqlLifecycle::Stable),
    SqlContractObject::function(
        "vector_enforce_typmod",
        "vector vector, typmod integer, _explicit boolean",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "vector_eq",
        "\"left\" vector, \"right\" vector",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "vector_from_double_array",
        "\"values\" double precision[]",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "vector_from_integer_array",
        "\"values\" integer[]",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "vector_from_real_array",
        "\"values\" real[]",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "vector_ge",
        "\"left\" vector, \"right\" vector",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "vector_gt",
        "\"left\" vector, \"right\" vector",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "vector_le",
        "\"left\" vector, \"right\" vector",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "vector_lt",
        "\"left\" vector, \"right\" vector",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "vector_ne",
        "\"left\" vector, \"right\" vector",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function("vector_sum_final", "state real[]", SqlLifecycle::Internal),
    SqlContractObject::function(
        "vector_sum_transition",
        "state real[], value vector",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "vector_to_real_array",
        "vector vector",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function("vector_typmod_in", "modifiers cstring[]", SqlLifecycle::Internal),
    SqlContractObject::function("vector_typmod_out", "typmod integer", SqlLifecycle::Internal),
    // Functions added after the registry was first frozen. The classification
    // test that enforces this list sat outside the old gate filter, so these
    // accumulated without a lifecycle decision; classified retroactively.
    SqlContractObject::function("_capture_build_point_delta", "", SqlLifecycle::Internal),
    SqlContractObject::function(
        "_refresh_collection_source_table",
        "p_collection_id bigint",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "_refresh_vector_source_binding",
        "p_collection_id bigint, p_vector_column_name text",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "_refresh_sparse_vector_source_binding",
        "p_collection_id bigint, p_vector_name text",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "_refresh_payload_source_bindings",
        "p_collection_id bigint",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "_cosine_distance_fast",
        "vector, vector",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "_hnsw_masked_candidates",
        "index_relation regclass, query vector, allowed_heap_tids anyarray, \"limit\" integer",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function("_l1_distance_fast", "vector, vector", SqlLifecycle::Internal),
    SqlContractObject::function("_l2_distance_fast", "vector, vector", SqlLifecycle::Internal),
    SqlContractObject::function("_l2_distance_fast8", "vector, vector", SqlLifecycle::Internal),
    SqlContractObject::function(
        "_negative_inner_product_fast",
        "vector, vector",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "adopt_pgvector",
        "target regclass, dry_run boolean, drop_old boolean",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "attach_hnsw_index",
        "collection_name text, vector_name text, index_name text",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "build_mmap_hnsw_artifact",
        "build_job_id bigint",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function("compact", "index regclass", SqlLifecycle::Experimental),
    SqlContractObject::function(
        "compare_indexes",
        "table_name text, column_name text, queries integer",
        SqlLifecycle::Experimental,
    ),
    SqlContractObject::function(
        "current_vector_config_revision",
        "collection bigint",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function("enable_pgvector_binding", "", SqlLifecycle::Experimental),
    SqlContractObject::function("hnsw_build_stats", "", SqlLifecycle::Experimental),
    SqlContractObject::function("hnsw_last_scan_work", "", SqlLifecycle::Experimental),
    SqlContractObject::function("hnsw_serving_stats", "", SqlLifecycle::Experimental),
    SqlContractObject::function("migration_report", "", SqlLifecycle::Experimental),
    // Failpoint setters exist only in pg_test builds; the classification test
    // that reads this registry runs only in those builds, so listing them
    // unconditionally keeps the const array length fixed without ever
    // claiming them against a production catalog.
    SqlContractObject::function(
        "test_set_artifact_publish_failpoint",
        "name text",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "test_set_build_job_failpoint",
        "name text",
        SqlLifecycle::Internal,
    ),
    SqlContractObject::function(
        "test_set_hnsw_physical_failpoint",
        "name text",
        SqlLifecycle::Internal,
    ),
];
