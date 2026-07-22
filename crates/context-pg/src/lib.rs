//! Thin PostgreSQL adapter crate for pgContext.
//!
//! PostgreSQL integration, SQLSTATE mapping, SPI access, ACL checks, and pgrx
//! bindings belong here. Reusable retrieval behavior belongs in the pure Rust
//! crates this adapter depends on.

use pgrx::prelude::*;

mod artifact_segments;
mod build_jobs;
mod catalog;
mod catalog_schema;
mod collection_aliases;
mod collection_limits;
#[cfg(any(test, feature = "pg_test"))]
pub(crate) mod contract;
mod domain_types;
mod embedding_migrations;
mod error;
#[allow(
    unsafe_code,
    reason = "the custom PostgreSQL access method is the crate's audited FFI boundary"
)]
mod hnsw_am;
mod hybrid_query;
mod late_interaction;
mod late_interaction_catalog;
mod late_interaction_catalog_schema;
mod model_versions;
mod operations;
mod payload_catalog;
mod payload_mutations;
mod pgvector_compat;
mod points;
mod quantization_sql;
mod query_builders;
mod query_stats;
mod retrieval;
mod settings;
mod sparse_search;
mod table_search;
mod telemetry;
mod vector;
mod vector_catalog;
#[allow(
    unsafe_code,
    reason = "the packed vector varlena codec is an audited PostgreSQL datum boundary"
)]
mod vector_datum;
mod vector_metadata_validation;
mod vector_variant_ordering;
mod vector_variant_typmods;
mod vector_variants;

::pgrx::pg_module_magic!(name, version);

/// The public SQL schema for pgContext types and functions.
#[pg_schema]
pub mod pgcontext {
    include!("sql_enums.rs");
}

/// Registers pgContext custom PostgreSQL settings when the extension loads.
#[pg_guard]
pub extern "C-unwind" fn _PG_init() {
    settings::init_gucs();
}

#[cfg(test)]
pub mod pg_test;

#[cfg(feature = "pg_test")]
#[pg_schema]
#[allow(
    unsafe_code,
    reason = "HNSW PostgreSQL tests exercise raw access-method callback contracts"
)]
mod tests {
    use pgrx::prelude::*;

    include!("pg_tests/sql_helpers.rs");
    include!("pg_tests/acl_denial.rs");
    include!("pg_tests/artifact_build_rls.rs");
    include!("pg_tests/artifact_quantization_policy.rs");
    include!("pg_tests/artifact_segments.rs");
    include!("pg_tests/artifact_segments_helpers.rs");
    include!("pg_tests/artifact_segment_hnsw_payload.rs");
    include!("pg_tests/artifact_segment_diagnostics.rs");
    include!("pg_tests/artifact_segment_retire_cleanup.rs");
    include!("pg_tests/artifact_segment_serving_readiness.rs");
    include!("pg_tests/build_job_helpers.rs");
    include!("pg_tests/build_jobs.rs");
    include!("pg_tests/build_jobs_acl.rs");
    include!("pg_tests/collection_catalog.rs");
    include!("pg_tests/collection_aliases.rs");
    include!("pg_tests/collection_limits.rs");
    include!("pg_tests/client_examples.rs");
    include!("pg_tests/contract.rs");
    include!("pg_tests/count.rs");
    include!("pg_tests/default_privileges.rs");
    include!("pg_tests/discovery.rs");
    include!("pg_tests/embedding_migrations.rs");
    include!("pg_tests/exact_oracle.rs");
    include!("pg_tests/facet.rs");
    include!("pg_tests/grouped_search.rs");
    include!("pg_tests/hnsw_am.rs");
    include!("pg_tests/hnsw_compaction.rs");
    include!("pg_tests/hnsw_delta_segment.rs");
    include!("pg_tests/hnsw_serving.rs");
    include!("pg_tests/hnsw_scan_policy.rs");
    include!("pg_tests/hybrid_query.rs");
    include!("pg_tests/hybrid_sparse_cosine.rs");
    include!("pg_tests/late_interaction_ann.rs");
    include!("pg_tests/late_interaction_owned.rs");
    include!("pg_tests/late_interaction_planner.rs");
    include!("pg_tests/index_advisor.rs");
    include!("pg_tests/model_versions.rs");
    include!("pg_tests/multitenancy.rs");
    include!("pg_tests/operations.rs");
    include!("pg_tests/payload_mutations.rs");
    include!("pg_tests/point_mapping.rs");
    include!("pg_tests/quantization.rs");
    include!("pg_tests/query_stats.rs");
    include!("pg_tests/query_builders.rs");
    include!("pg_tests/retrieval.rs");
    include!("pg_tests/recommendation.rs");
    include!("pg_tests/scroll.rs");
    include!("pg_tests/security.rs");
    include!("pg_tests/sqlstate_contract.rs");
    include!("pg_tests/table_search.rs");
    include!("pg_tests/table_search_mmap_hnsw.rs");
    include!("pg_tests/transaction_rollback.rs");
    include!("pg_tests/vector_compatibility.rs");
    include!("pg_tests/vector_variant_compatibility.rs");
    include!("pg_tests/vector_variant_typmods.rs");
    include!("pg_tests/bitvec_compatibility.rs");
    include!("pg_tests/vector_registration.rs");
    include!("pg_tests/vector_search.rs");
}

pub use vector::Vector;

/// Returns the extension SQL schema name.
#[must_use]
pub const fn extension_schema() -> &'static str {
    "pgcontext"
}
