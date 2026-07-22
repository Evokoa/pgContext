//! SQL lifecycle entries for pgvector ownership conversion.

use super::{SqlContractObject, SqlLifecycle};

pub(super) const PGVECTOR_OWNERSHIP_SQL_CONTRACT_OBJECTS_LEN: usize = 13;

#[rustfmt::skip]
pub(super) const PGVECTOR_OWNERSHIP_SQL_CONTRACT_OBJECTS:
    &[SqlContractObject; PGVECTOR_OWNERSHIP_SQL_CONTRACT_OBJECTS_LEN] = &[
        SqlContractObject::function(
            "_sync_pgvector_ownership_columns",
            "",
            SqlLifecycle::Internal,
        ),
        SqlContractObject::function(
            "_begin_pgvector_ownership_conversion",
            "source_table_oid oid, source_column_name text, mode text, metric text, dependency_manifest text[], validation_attestations text[]",
            SqlLifecycle::Internal,
        ),
        SqlContractObject::function(
            "_transition_pgvector_ownership_conversion",
            "conversion_id bigint, expected_status text, new_status text, shadow_attnum smallint, total_rows bigint, processed_rows bigint, mismatch_count bigint, backfill_cursor text, source_checksum text, shadow_checksum text, attestation text, error_message text",
            SqlLifecycle::Internal,
        ),
        SqlContractObject::function(
            "start_pgvector_ownership_conversion",
            "target regclass, column_name text, mode text, metric text, application_uses_column_lists boolean, application_dependencies_reviewed boolean",
            SqlLifecycle::Experimental,
        ),
        SqlContractObject::function(
            "run_pgvector_ownership_conversion",
            "conversion_id bigint, batch_size integer, sessions_drained boolean",
            SqlLifecycle::Experimental,
        ),
        SqlContractObject::function(
            "cutover_pgvector_ownership_conversion",
            "conversion_id bigint, sessions_drained boolean",
            SqlLifecycle::Experimental,
        ),
        SqlContractObject::function(
            "finalize_pgvector_ownership_conversion",
            "conversion_id bigint",
            SqlLifecycle::Experimental,
        ),
        SqlContractObject::function(
            "rollback_pgvector_ownership_conversion",
            "conversion_id bigint",
            SqlLifecycle::Experimental,
        ),
        SqlContractObject::function(
            "pgvector_ownership_conversions",
            "",
            SqlLifecycle::Experimental,
        ),
        SqlContractObject::function(
            "adopt_pgvector",
            "target regclass, dry_run boolean, drop_old boolean",
            SqlLifecycle::Experimental,
        ),
        SqlContractObject::function(
            "compare_indexes",
            "table_name text, column_name text, queries integer",
            SqlLifecycle::Experimental,
        ),
        SqlContractObject::function(
            "enable_pgvector_binding",
            "",
            SqlLifecycle::Experimental,
        ),
        SqlContractObject::function(
            "migration_report",
            "",
            SqlLifecycle::Experimental,
        ),
    ];
