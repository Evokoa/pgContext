//! Thin PostgreSQL adapter crate for pgContext.
//!
//! PostgreSQL integration, SQLSTATE mapping, SPI access, ACL checks, and pgrx
//! bindings belong here. Reusable retrieval behavior belongs in the pure Rust
//! crates this adapter depends on.

use pgrx::prelude::*;

#[allow(
    unsafe_code,
    reason = "artifact mmap serving upholds the immutable-generation file contract at its audited adapter boundary"
)]
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
#[allow(
    unsafe_code,
    reason = "ownership conversion uses certified binary Datum copies and OID-bound relation locks"
)]
mod pgvector_ownership;
mod pgvector_ownership_catalog;
mod points;
mod quantization_sql;
mod query_builders;
mod query_stats;
#[allow(
    unsafe_code,
    reason = "rollback-independent telemetry uses PostgreSQL named DSM, error hooks, and a dynamic background worker behind a fixed safe event API"
)]
mod query_stats_async;
mod retrieval;
mod settings;
mod sparse_search;
mod table_search;
mod telemetry;
mod vector_catalog;
#[allow(
    unsafe_code,
    reason = "the packed vector varlena codec is an audited PostgreSQL datum boundary"
)]
mod vector_datum;
mod vector_metadata_validation;
mod vector_variant_ordering;
mod vector_variant_typmods;

::pgrx::pg_module_magic!(name, version);

/// Rust namespace for SQL entities installed into the fixed `pgcontext` schema.
pub mod pgcontext {
    include!("sql_enums.rs");

    pub(crate) mod vector {
        include!("vector.rs");
    }

    pub(crate) mod vector_variants {
        include!("vector_variants.rs");
    }
}

pub(crate) use pgcontext::{vector, vector_variants};

// The generated schema item runs before this guard. Refuse a pre-existing
// foreign-owned or delegated-CREATE schema: security-definer functions place
// this namespace on trusted paths.
pgrx::extension_sql!(
    r#"
DO $pgcontext_schema_guard$
DECLARE
    target_owner oid;
    delegated_create boolean;
BEGIN
    SELECT namespace.nspowner,
           EXISTS (
               SELECT 1
                 FROM pg_catalog.aclexplode(
                          coalesce(
                              namespace.nspacl,
                              pg_catalog.acldefault('n', namespace.nspowner)
                          )
                      ) AS privilege
                WHERE privilege.privilege_type = 'CREATE'
                  AND privilege.grantee <> namespace.nspowner
           )
      INTO target_owner, delegated_create
      FROM pg_catalog.pg_namespace AS namespace
     WHERE namespace.nspname = 'pgcontext';

    IF target_owner IS DISTINCT FROM CURRENT_USER::pg_catalog.regrole::oid THEN
        RAISE EXCEPTION 'pgcontext schema must be owned by the extension installer'
            USING ERRCODE = '42501';
    END IF;
    IF delegated_create THEN
        RAISE EXCEPTION 'pgcontext schema must not delegate CREATE privilege'
            USING ERRCODE = '42501';
    END IF;
END
$pgcontext_schema_guard$;
"#,
    name = "pgcontext_bootstrap"
);

/// Registers pgContext custom PostgreSQL settings when the extension loads.
#[pg_guard]
pub extern "C-unwind" fn _PG_init() {
    settings::init_gucs();
    hnsw_am::init_mapped_graph_lifecycle_hooks();
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
    include!("pg_tests/membership_view_security.rs");
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
    include!("pg_tests/hnsw_pgvector_compat.rs");
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
