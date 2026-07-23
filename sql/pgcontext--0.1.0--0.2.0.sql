/* pgContext 0.1.0 -> 0.2.0
 *
 * Preserves extension configuration data while adding the post-v1 roadmap
 * catalog and SQL surface. Historical client-written "automatic" cohorts are
 * renamed before that cohort becomes reserved for internal observations.
 */

SET search_path = pgcontext, pg_catalog, public;

-- A pgvector-first 0.1 install bound pgContext's original SQL surface directly
-- to pgvector-owned public types. That legacy shape cannot be converted into
-- the 0.2 fixed-schema ownership model by moving another extension's objects.
-- Refuse before any catalog mutation; user columns and pgvector data remain
-- untouched and can be served after reinstalling pgContext plus its bridge.
DO $pgcontext_upgrade_preflight$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pg_catalog.pg_roles
         WHERE rolname = CURRENT_USER
           AND rolsuper
    ) THEN
        RAISE EXCEPTION 'pgContext 0.1 to 0.2 update requires a PostgreSQL superuser'
            USING ERRCODE = '42501',
                  DETAIL = 'The version-pinned namespace repair updates PostgreSQL extension catalogs; no pgContext catalog row has been changed.',
                  HINT = 'Have a superuser run ALTER EXTENSION pgcontext UPDATE TO ''0.2.0''; application roles do not need superuser privileges to use granted APIs.';
    END IF;
    IF EXISTS (
        SELECT 1
          FROM pg_catalog.pg_type AS type
          JOIN pg_catalog.pg_namespace AS namespace
            ON namespace.oid = type.typnamespace
          JOIN pg_catalog.pg_depend AS dependency
            ON dependency.classid = 'pg_catalog.pg_type'::pg_catalog.regclass
           AND dependency.objid = type.oid
           AND dependency.deptype = 'e'
          JOIN pg_catalog.pg_extension AS extension
            ON extension.oid = dependency.refobjid
         WHERE namespace.nspname = 'public'
           AND type.typname = 'vector'
           AND extension.extname = 'vector'
    ) THEN
        RAISE EXCEPTION 'pgContext 0.1 pgvector-first coexistence requires a bridge reinstall for 0.2'
            USING ERRCODE = '0A000',
                  DETAIL = 'No pgvector-owned type or user column was changed.',
                  HINT = 'Export pgContext registrations and inventory every dependent object first; DROP EXTENSION pgcontext CASCADE can remove more than indexes. Install 0.2 plus pgcontext_pgvector, recreate registrations/dependents, and rebuild pgcontext_hnsw indexes over the unchanged pgvector columns.';
    END IF;
END
$pgcontext_upgrade_preflight$;

UPDATE pgcontext._query_stats
   SET cohort = 'legacy_automatic'
 WHERE cohort = 'automatic';

-- Move the released pgContext-owned physical type OIDs and their I/O support
-- functions in place. Dependent user columns, functions, operators, and
-- indexes follow without a table rewrite.
ALTER TYPE public.vector SET SCHEMA pgcontext;
ALTER TYPE public.halfvec SET SCHEMA pgcontext;
ALTER TYPE public.sparsevec SET SCHEMA pgcontext;
ALTER TYPE public.bitvec SET SCHEMA pgcontext;

ALTER FUNCTION public.vector_in(cstring) SET SCHEMA pgcontext;
ALTER FUNCTION public.vector_out(pgcontext.vector) SET SCHEMA pgcontext;
ALTER FUNCTION public.halfvec_in(cstring) SET SCHEMA pgcontext;
ALTER FUNCTION public.halfvec_out(pgcontext.halfvec) SET SCHEMA pgcontext;
ALTER FUNCTION public.sparsevec_in(cstring) SET SCHEMA pgcontext;
ALTER FUNCTION public.sparsevec_out(pgcontext.sparsevec) SET SCHEMA pgcontext;
ALTER FUNCTION public.bitvec_in(cstring) SET SCHEMA pgcontext;
ALTER FUNCTION public.bitvec_out(pgcontext.bitvec) SET SCHEMA pgcontext;

-- v0.1 registered the extension itself in the caller's target schema even
-- though its SQL objects live in pgcontext. PostgreSQL has no supported
-- ALTER EXTENSION SET SCHEMA path for that non-relocatable mixed-schema
-- release, so repair both catalog fields that represent the namespace in one
-- versioned transaction. This makes pg_dump emit a restorable
-- `WITH SCHEMA pgcontext` declaration matching the fixed-schema 0.2 control.
ALTER EXTENSION pgcontext DROP SCHEMA pgcontext;

DO $pgcontext_namespace_repair$
DECLARE
    dependency_rows integer;
    extension_rows integer;
BEGIN
    UPDATE pg_catalog.pg_depend AS dependency
       SET refobjid = 'pgcontext'::pg_catalog.regnamespace
      FROM pg_catalog.pg_extension AS extension
     WHERE extension.extname = 'pgcontext'
       AND dependency.classid = 'pg_catalog.pg_extension'::pg_catalog.regclass
       AND dependency.objid = extension.oid
       AND dependency.refclassid = 'pg_catalog.pg_namespace'::pg_catalog.regclass
       AND dependency.deptype = 'n';
    GET DIAGNOSTICS dependency_rows = ROW_COUNT;

    UPDATE pg_catalog.pg_extension
       SET extnamespace = 'pgcontext'::pg_catalog.regnamespace
     WHERE extname = 'pgcontext';
    GET DIAGNOSTICS extension_rows = ROW_COUNT;

    IF dependency_rows <> 1 OR extension_rows <> 1 THEN
        RAISE EXCEPTION 'pgContext extension namespace repair changed unexpected row counts: dependency %, extension %',
                        dependency_rows, extension_rows
            USING ERRCODE = 'XX000';
    END IF;
    IF EXISTS (
        SELECT 1
          FROM pg_catalog.pg_namespace AS namespace
          JOIN pg_catalog.pg_depend AS dependency
            ON dependency.classid = 'pg_catalog.pg_namespace'::pg_catalog.regclass
           AND dependency.objid = namespace.oid
           AND dependency.refclassid = 'pg_catalog.pg_extension'::pg_catalog.regclass
           AND dependency.deptype = 'e'
          JOIN pg_catalog.pg_extension AS extension
            ON extension.oid = dependency.refobjid
         WHERE namespace.nspname = 'pgcontext'
           AND extension.extname = 'pgcontext'
    ) THEN
        RAISE EXCEPTION 'pgcontext schema remained an extension member after namespace repair'
            USING ERRCODE = 'XX000';
    END IF;
    IF NOT EXISTS (
        SELECT 1
          FROM pg_catalog.pg_extension AS extension
          JOIN pg_catalog.pg_depend AS dependency
            ON dependency.classid = 'pg_catalog.pg_extension'::pg_catalog.regclass
           AND dependency.objid = extension.oid
           AND dependency.refclassid = 'pg_catalog.pg_namespace'::pg_catalog.regclass
           AND dependency.refobjid = extension.extnamespace
           AND dependency.deptype = 'n'
         WHERE extension.extname = 'pgcontext'
           AND extension.extnamespace = 'pgcontext'::pg_catalog.regnamespace
    ) THEN
        RAISE EXCEPTION 'pgContext extension namespace repair did not preserve catalog invariants'
            USING ERRCODE = 'XX000';
    END IF;
END
$pgcontext_namespace_repair$;

-- These retained wrapper symbols changed SQL descriptors or privilege mode in
-- 0.2. Replace their catalog declarations before the new library can return a
-- tuple shape that disagrees with the v0.1 pg_proc row.
DROP FUNCTION pgcontext.hnsw_serving_stats();
CREATE FUNCTION pgcontext.hnsw_serving_stats() RETURNS TABLE (
    pack_builds bigint,
    pack_reuses bigint,
    last_pack_bytes bigint,
    last_pack_millis bigint,
    total_pack_millis bigint,
    shared_attaches bigint,
    shared_publishes bigint,
    shared_publish_skips bigint,
    mapped_attaches bigint,
    mapped_publishes bigint,
    mapped_publish_skips bigint,
    page_native_fallbacks bigint,
    delta_segment_records bigint,
    delta_segment_scans bigint
)
STRICT
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c
AS 'MODULE_PATHNAME', 'hnsw_serving_stats_wrapper';

DROP FUNCTION pgcontext.migration_report();
CREATE FUNCTION pgcontext.migration_report() RETURNS TABLE (
    schema_name text,
    table_name text,
    column_name text,
    type_name text,
    dimensions integer,
    pgvector_indexes text[],
    pgcontext_indexes text[],
    conversion_supported boolean,
    blockers text[],
    suggested_command text
)
STRICT
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c
AS 'MODULE_PATHNAME', 'migration_report_wrapper';

CREATE OR REPLACE FUNCTION pgcontext.build_mmap_hnsw_artifact(build_job_id bigint)
RETURNS bytea
STRICT VOLATILE SECURITY INVOKER
SET search_path TO pg_catalog, pgcontext
LANGUAGE c
AS 'MODULE_PATHNAME', 'build_mmap_hnsw_artifact_wrapper';

ALTER TABLE pgcontext._collection_vectors
    DROP CONSTRAINT _collection_vectors_metric_check;
ALTER TABLE pgcontext._collection_vectors
    ADD CONSTRAINT _collection_vectors_metric_check
    CHECK (metric IN ('l2', 'inner_product', 'cosine', 'l1', 'hamming', 'jaccard'));

ALTER TABLE pgcontext._collection_sparse_vectors
    DROP CONSTRAINT _collection_sparse_vectors_metric_check;
ALTER TABLE pgcontext._collection_sparse_vectors
    ADD CONSTRAINT _collection_sparse_vectors_metric_check
    CHECK (metric IN ('l2', 'inner_product', 'cosine', 'l1', 'hamming', 'jaccard'));

ALTER TABLE pgcontext._model_versions
    DROP CONSTRAINT _model_versions_metric_check;
ALTER TABLE pgcontext._model_versions
    ADD CONSTRAINT _model_versions_metric_check
    CHECK (metric IN ('l2', 'inner_product', 'cosine', 'l1', 'hamming', 'jaccard'));

ALTER TABLE pgcontext._query_stats
    ADD COLUMN strategy text NOT NULL DEFAULT 'unspecified'
        CHECK (pg_catalog.octet_length(strategy) BETWEEN 1 AND 64 AND strategy ~ '^[a-z0-9_]+$'),
    ADD COLUMN visits bigint NOT NULL DEFAULT 0 CHECK (visits >= 0),
    ADD COLUMN filter_candidates bigint NOT NULL DEFAULT 0 CHECK (filter_candidates >= 0),
    ADD COLUMN candidates bigint NOT NULL DEFAULT 0 CHECK (candidates >= 0),
    ADD COLUMN rechecks bigint NOT NULL DEFAULT 0 CHECK (rechecks >= 0),
    ADD COLUMN stages bigint NOT NULL DEFAULT 0 CHECK (stages >= 0),
    ADD COLUMN expansions bigint NOT NULL DEFAULT 0 CHECK (expansions >= 0),
    ADD COLUMN completion text NOT NULL DEFAULT 'unspecified'
        CHECK (completion IN ('unspecified','complete','cancelled','budget_exhausted','error'));

CREATE VIEW pgcontext._visible_query_stats
WITH (security_barrier = true) AS
SELECT stats.*
  FROM pgcontext._query_stats AS stats
  JOIN pgcontext._collections AS collections USING (collection_id)
 WHERE pg_catalog.pg_has_role(SESSION_USER, collections.owner_role, 'MEMBER');

GRANT SELECT ON pgcontext._visible_query_stats TO PUBLIC;

CREATE VIEW pgcontext._visible_collections
WITH (security_barrier = true) AS
SELECT collection_id,
       collection_name,
       owner_role,
       source_table_oid,
       source_schema_name,
       source_table_name
  FROM pgcontext._collections
 WHERE pg_catalog.pg_has_role(SESSION_USER, owner_role, 'MEMBER');

GRANT SELECT ON pgcontext._visible_collections TO PUBLIC;

-- These visibility views predate 0.2.0. Preserve their definitions and harden
-- their membership predicates against non-leakproof caller quals during the
-- in-place upgrade.
ALTER VIEW pgcontext._visible_collection_vectors
    SET (security_barrier = true);
ALTER VIEW pgcontext._visible_collection_sparse_vectors
    SET (security_barrier = true);
ALTER VIEW pgcontext._visible_collection_points
    SET (security_barrier = true);
ALTER VIEW pgcontext._visible_collection_payload_columns
    SET (security_barrier = true);
ALTER VIEW pgcontext._visible_build_jobs
    SET (security_barrier = true);
ALTER VIEW pgcontext._visible_artifact_segments
    SET (security_barrier = true);
ALTER VIEW pgcontext._visible_collection_limits
    SET (security_barrier = true);

CREATE FUNCTION pgcontext._refresh_collection_source_table(
    p_collection_id bigint
)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pgcontext._collections
         WHERE collection_id = p_collection_id
           AND pg_catalog.pg_has_role(SESSION_USER, owner_role, 'MEMBER')
    ) THEN
        RAISE EXCEPTION 'permission denied to refresh collection %', p_collection_id
            USING ERRCODE = '42501';
    END IF;

    UPDATE pgcontext._collections AS collections
       SET source_table_oid = source_class.oid,
           updated_at = pg_catalog.now()
      FROM pg_catalog.pg_class AS source_class
      JOIN pg_catalog.pg_namespace AS source_namespace
        ON source_namespace.oid = source_class.relnamespace
     WHERE collections.collection_id = p_collection_id
       AND source_namespace.nspname = collections.source_schema_name
       AND source_class.relname = collections.source_table_name
       AND source_class.relkind IN ('r', 'p');
END;
$$;

CREATE FUNCTION pgcontext._refresh_vector_source_binding(
    p_collection_id bigint,
    p_vector_column_name text
)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pgcontext._collections
         WHERE collection_id = p_collection_id
           AND pg_catalog.pg_has_role(SESSION_USER, owner_role, 'MEMBER')
    ) THEN
        RAISE EXCEPTION 'permission denied to refresh collection %', p_collection_id
            USING ERRCODE = '42501';
    END IF;

    UPDATE pgcontext._collection_vectors AS vectors
       SET source_table_oid = source_class.oid,
           vector_attnum = source_attribute.attnum,
           updated_at = pg_catalog.now()
      FROM pg_catalog.pg_class AS source_class
      JOIN pg_catalog.pg_namespace AS source_namespace
        ON source_namespace.oid = source_class.relnamespace
      JOIN pg_catalog.pg_attribute AS source_attribute
        ON source_attribute.attrelid = source_class.oid
     WHERE vectors.collection_id = p_collection_id
       AND vectors.vector_column_name = p_vector_column_name
       AND source_namespace.nspname = vectors.source_schema_name
       AND source_class.relname = vectors.source_table_name
       AND source_class.relkind IN ('r', 'p')
       AND source_attribute.attname = vectors.vector_column_name
       AND source_attribute.attnum > 0
       AND NOT source_attribute.attisdropped;
END;
$$;

CREATE FUNCTION pgcontext._refresh_sparse_vector_source_binding(
    p_collection_id bigint,
    p_vector_name text
)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pgcontext._collections
         WHERE collection_id = p_collection_id
           AND pg_catalog.pg_has_role(SESSION_USER, owner_role, 'MEMBER')
    ) THEN
        RAISE EXCEPTION 'permission denied to refresh collection %', p_collection_id
            USING ERRCODE = '42501';
    END IF;

    UPDATE pgcontext._collection_sparse_vectors AS sparse_vectors
       SET source_table_oid = source_class.oid,
           vector_attnum = source_attribute.attnum,
           updated_at = pg_catalog.now()
      FROM pg_catalog.pg_class AS source_class
      JOIN pg_catalog.pg_namespace AS source_namespace
        ON source_namespace.oid = source_class.relnamespace
      JOIN pg_catalog.pg_attribute AS source_attribute
        ON source_attribute.attrelid = source_class.oid
     WHERE sparse_vectors.collection_id = p_collection_id
       AND sparse_vectors.vector_name = p_vector_name
       AND source_namespace.nspname = sparse_vectors.source_schema_name
       AND source_class.relname = sparse_vectors.source_table_name
       AND source_class.relkind IN ('r', 'p')
       AND source_attribute.attname = sparse_vectors.vector_column_name
       AND source_attribute.attnum > 0
       AND NOT source_attribute.attisdropped;
END;
$$;

CREATE FUNCTION pgcontext._refresh_payload_source_bindings(
    p_collection_id bigint
)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pgcontext._collections
         WHERE collection_id = p_collection_id
           AND pg_catalog.pg_has_role(SESSION_USER, owner_role, 'MEMBER')
    ) THEN
        RAISE EXCEPTION 'permission denied to refresh collection %', p_collection_id
            USING ERRCODE = '42501';
    END IF;

    UPDATE pgcontext._collection_payload_columns AS payload_columns
       SET source_table_oid = source_class.oid,
           column_attnum = source_attribute.attnum,
           updated_at = pg_catalog.now()
      FROM pg_catalog.pg_class AS source_class
      JOIN pg_catalog.pg_namespace AS source_namespace
        ON source_namespace.oid = source_class.relnamespace
      JOIN pg_catalog.pg_attribute AS source_attribute
        ON source_attribute.attrelid = source_class.oid
     WHERE payload_columns.collection_id = p_collection_id
       AND source_namespace.nspname = payload_columns.source_schema_name
       AND source_class.relname = payload_columns.source_table_name
       AND source_class.relkind IN ('r', 'p')
       AND source_attribute.attname = payload_columns.column_name
       AND source_attribute.attnum > 0
       AND NOT source_attribute.attisdropped;
END;
$$;

-- crates/context-pg/src/pgvector_ownership/persistence.rs:140
-- pgcontext::pgvector_ownership::persistence::_begin_pgvector_ownership_conversion
CREATE  FUNCTION "_begin_pgvector_ownership_conversion"(
	"source_table_oid" oid, /* pg_sys :: Oid */
	"source_column_name" TEXT, /* String */
	"mode" TEXT, /* String */
	"metric" TEXT, /* String */
	"dependency_manifest" TEXT[], /* Vec < String > */
	"validation_attestations" TEXT[] /* Vec < String > */
) RETURNS bigint /* i64 */
STRICT SECURITY DEFINER 
SET search_path TO pg_catalog, pgcontext
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'begin_conversion_catalog_wrapper';

-- crates/context-pg/src/pgvector_ownership/persistence.rs:300
-- pgcontext::pgvector_ownership::persistence::_transition_pgvector_ownership_conversion
CREATE  FUNCTION "_transition_pgvector_ownership_conversion"(
	"conversion_id" bigint, /* i64 */
	"expected_status" TEXT, /* String */
	"new_status" TEXT, /* String */
	"shadow_attnum" smallint, /* Option < i16 > */
	"total_rows" bigint, /* i64 */
	"processed_rows" bigint, /* i64 */
	"mismatch_count" bigint, /* i64 */
	"backfill_cursor" TEXT, /* Option < String > */
	"source_checksum" TEXT, /* Option < String > */
	"shadow_checksum" TEXT, /* Option < String > */
	"attestation" TEXT, /* Option < String > */
	"error_message" TEXT /* Option < String > */
) RETURNS void
SECURITY DEFINER 
SET search_path TO pg_catalog, pgcontext
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'transition_conversion_catalog_wrapper';

-- crates/context-pg/src/vector_catalog.rs:202
-- pgcontext::vector_catalog::attach_sparse_hnsw_index
CREATE  FUNCTION "attach_sparse_hnsw_index"(
	"collection_name" TEXT, /* String */
	"vector_name" TEXT, /* String */
	"index_name" TEXT /* String */
) RETURNS void
STRICT SECURITY DEFINER
SET search_path TO pg_catalog, pgcontext
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'attach_sparse_hnsw_index_wrapper';

-- crates/context-pg/src/pgvector_ownership.rs:225
-- pgcontext::pgvector_ownership::cutover_pgvector_ownership_conversion
CREATE  FUNCTION "cutover_pgvector_ownership_conversion"(
	"conversion_id" bigint, /* i64 */
	"sessions_drained" bool DEFAULT false /* bool */
) RETURNS TABLE (
	"conversion_id" bigint,  /* i64 */
	"mode" TEXT,  /* String */
	"status" TEXT,  /* String */
	"schema_name" TEXT,  /* String */
	"table_name" TEXT,  /* String */
	"column_name" TEXT,  /* String */
	"target_type" TEXT,  /* String */
	"total_rows" bigint,  /* i64 */
	"processed_rows" bigint,  /* i64 */
	"mismatch_count" bigint,  /* i64 */
	"validation_attestations" TEXT[],  /* Vec < String > */
	"next_command" TEXT,  /* Option < String > */
	"error_message" TEXT  /* Option < String > */
)
STRICT
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'cutover_pgvector_ownership_conversion_wrapper';

-- crates/context-pg/src/query_builders.rs:244
-- pgcontext::query_builders::execute_query
CREATE  FUNCTION "execute_query"(
	"collection" TEXT, /* String */
	"plan" jsonb /* JsonB */
) RETURNS TABLE (
	"point_id" bigint,  /* i64 */
	"source_key" TEXT,  /* String */
	"score" real  /* f32 */
)
STRICT
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'execute_query_wrapper';

-- crates/context-pg/src/pgvector_ownership.rs:270
-- pgcontext::pgvector_ownership::finalize_pgvector_ownership_conversion
CREATE  FUNCTION "finalize_pgvector_ownership_conversion"(
	"conversion_id" bigint /* i64 */
) RETURNS TABLE (
	"conversion_id" bigint,  /* i64 */
	"mode" TEXT,  /* String */
	"status" TEXT,  /* String */
	"schema_name" TEXT,  /* String */
	"table_name" TEXT,  /* String */
	"column_name" TEXT,  /* String */
	"target_type" TEXT,  /* String */
	"total_rows" bigint,  /* i64 */
	"processed_rows" bigint,  /* i64 */
	"mismatch_count" bigint,  /* i64 */
	"validation_attestations" TEXT[],  /* Vec < String > */
	"next_command" TEXT,  /* Option < String > */
	"error_message" TEXT  /* Option < String > */
)
STRICT
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'finalize_pgvector_ownership_conversion_wrapper';

-- crates/context-pg/src/pgvector_ownership.rs:345
-- pgcontext::pgvector_ownership::pgvector_ownership_conversions
CREATE  FUNCTION "pgvector_ownership_conversions"() RETURNS TABLE (
	"conversion_id" bigint,  /* i64 */
	"mode" TEXT,  /* String */
	"status" TEXT,  /* String */
	"schema_name" TEXT,  /* String */
	"table_name" TEXT,  /* String */
	"column_name" TEXT,  /* String */
	"target_type" TEXT,  /* String */
	"total_rows" bigint,  /* i64 */
	"processed_rows" bigint,  /* i64 */
	"mismatch_count" bigint,  /* i64 */
	"validation_attestations" TEXT[],  /* Vec < String > */
	"next_command" TEXT,  /* Option < String > */
	"error_message" TEXT  /* Option < String > */
)
STRICT
SET search_path TO pg_catalog, pgcontext
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'pgvector_ownership_conversions_wrapper';

-- crates/context-pg/src/query_stats.rs:379
-- pgcontext::query_stats::query_execution_stats
CREATE  FUNCTION "query_execution_stats"() RETURNS TABLE (
	"collection_name" TEXT,  /* String */
	"query_kind" TEXT,  /* String */
	"strategy" TEXT,  /* String */
	"query_count" bigint,  /* i64 */
	"total_visits" bigint,  /* i64 */
	"total_filter_candidates" bigint,  /* i64 */
	"total_candidates" bigint,  /* i64 */
	"total_rechecks" bigint,  /* i64 */
	"total_stages" bigint,  /* i64 */
	"total_expansions" bigint,  /* i64 */
	"completion" TEXT,  /* String */
	"latency_bucket" QueryLatencyBucket,  /* QueryLatencyBucket */
	"lifecycle_state" QueryLifecycleState,  /* QueryLifecycleState */
	"avg_latency_ms" double precision  /* f64 */
)
STRICT 
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'query_execution_stats_wrapper';

-- crates/context-pg/src/query_builders.rs:94
-- pgcontext::query_builders::query_full_text
CREATE  FUNCTION "query_full_text"(
	"text_query" TEXT, /* String */
	"text_column" TEXT, /* String */
	"limit" INT /* i32 */
) RETURNS jsonb /* JsonB */
STRICT
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'query_full_text_wrapper';

-- crates/context-pg/src/query_stats.rs:408
-- pgcontext::query_stats::query_telemetry_queue_stats
CREATE  FUNCTION "query_telemetry_queue_stats"() RETURNS TABLE (
	"transport" TEXT,  /* String */
	"delivery" TEXT,  /* String */
	"enqueued" bigint,  /* i64 */
	"persisted" bigint,  /* i64 */
	"dropped_contention" bigint,  /* i64 */
	"dropped_full" bigint,  /* i64 */
	"dropped_orphaned" bigint,  /* i64 */
	"database_slot_exhausted" bigint,  /* i64 */
	"worker_launch_failures" bigint,  /* i64 */
	"pending" bigint,  /* i64 */
	"worker_pid" INT  /* Option < i32 > */
)
STRICT 
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'query_telemetry_queue_stats_wrapper';

-- crates/context-pg/src/late_interaction_catalog.rs:57
-- pgcontext::late_interaction_catalog::register_late_interaction
CREATE  FUNCTION "register_late_interaction"(
	"collection" TEXT, /* String */
	"source_table" TEXT, /* String */
	"token_source" TEXT /* String */
) RETURNS TABLE (
	"collection" TEXT,  /* String */
	"source_table" TEXT,  /* String */
	"token_source" TEXT,  /* String */
	"dimensions" INT,  /* Option < i32 > */
	"point_count" bigint,  /* i64 */
	"token_count" bigint,  /* i64 */
	"status" TEXT  /* String */
)
STRICT
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'register_late_interaction_wrapper';

-- crates/context-pg/src/late_interaction_catalog.rs:113
-- pgcontext::late_interaction_catalog::repair_late_interaction
CREATE  FUNCTION "repair_late_interaction"(
	"collection" TEXT, /* String */
	"batch_size" INT /* i32 */
) RETURNS TABLE (
	"collection" TEXT,  /* String */
	"batch_count" bigint,  /* i64 */
	"point_count" bigint,  /* i64 */
	"token_count" bigint,  /* i64 */
	"dimensions" INT,  /* Option < i32 > */
	"status" TEXT  /* String */
)
STRICT
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'repair_late_interaction_wrapper';

-- crates/context-pg/src/pgvector_ownership.rs:306
-- pgcontext::pgvector_ownership::rollback_pgvector_ownership_conversion
CREATE  FUNCTION "rollback_pgvector_ownership_conversion"(
	"conversion_id" bigint /* i64 */
) RETURNS TABLE (
	"conversion_id" bigint,  /* i64 */
	"mode" TEXT,  /* String */
	"status" TEXT,  /* String */
	"schema_name" TEXT,  /* String */
	"table_name" TEXT,  /* String */
	"column_name" TEXT,  /* String */
	"target_type" TEXT,  /* String */
	"total_rows" bigint,  /* i64 */
	"processed_rows" bigint,  /* i64 */
	"mismatch_count" bigint,  /* i64 */
	"validation_attestations" TEXT[],  /* Vec < String > */
	"next_command" TEXT,  /* Option < String > */
	"error_message" TEXT  /* Option < String > */
)
STRICT
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'rollback_pgvector_ownership_conversion_wrapper';

-- crates/context-pg/src/pgvector_ownership.rs:156
-- pgcontext::pgvector_ownership::run_pgvector_ownership_conversion
CREATE  FUNCTION "run_pgvector_ownership_conversion"(
	"conversion_id" bigint, /* i64 */
	"batch_size" INT DEFAULT 1000, /* i32 */
	"sessions_drained" bool DEFAULT false /* bool */
) RETURNS TABLE (
	"conversion_id" bigint,  /* i64 */
	"mode" TEXT,  /* String */
	"status" TEXT,  /* String */
	"schema_name" TEXT,  /* String */
	"table_name" TEXT,  /* String */
	"column_name" TEXT,  /* String */
	"target_type" TEXT,  /* String */
	"total_rows" bigint,  /* i64 */
	"processed_rows" bigint,  /* i64 */
	"mismatch_count" bigint,  /* i64 */
	"validation_attestations" TEXT[],  /* Vec < String > */
	"next_command" TEXT,  /* Option < String > */
	"error_message" TEXT  /* Option < String > */
)
STRICT
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'run_pgvector_ownership_conversion_wrapper';

-- crates/context-pg/src/query_builders.rs:65
-- pgcontext::query_builders::query_sparse_nearest
CREATE  FUNCTION "query_sparse_nearest"(
	"vector_name" TEXT, /* String */
	"vector" SparseVec, /* SparseVec */
	"filter" jsonb, /* Option < JsonB > */
	"limit" INT /* i32 */
) RETURNS jsonb /* JsonB */

SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'query_sparse_nearest_filtered_wrapper';

-- crates/context-pg/src/hnsw_am.rs:539
-- pgcontext::hnsw_am::_hnsw_sparse_candidates
CREATE  FUNCTION "_hnsw_sparse_candidates"(
	"index_relation" regclass, /* PgRelation */
	"query" SparseVec, /* SparseVec */
	"limit" INT /* i32 */
) RETURNS TABLE (
	"heap_tid" TEXT,  /* String */
	"score" real  /* f32 */
)
STRICT 
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'hnsw_sparse_candidates_wrapper';

-- crates/context-pg/src/query_builders.rs:50
-- pgcontext::query_builders::query_sparse_nearest
CREATE  FUNCTION "query_sparse_nearest"(
	"vector_name" TEXT, /* String */
	"vector" SparseVec, /* SparseVec */
	"limit" INT /* i32 */
) RETURNS jsonb /* JsonB */
STRICT
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'query_sparse_nearest_wrapper';

-- crates/context-pg/src/sparse_search.rs:54
-- pgcontext::sparse_search::explain_sparse
CREATE  FUNCTION "explain_sparse"(
	"collection" TEXT, /* String */
	"vector_name" TEXT, /* String */
	"query" SparseVec, /* SparseVec */
	"limit" INT /* i32 */
) RETURNS TABLE (
	"strategy" TEXT,  /* String */
	"active_points" bigint,  /* i64 */
	"scored_count" bigint,  /* i64 */
	"candidate_count" bigint,  /* i64 */
	"recheck_count" bigint  /* i64 */
)
STRICT
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'explain_sparse_wrapper';

-- crates/context-pg/src/hnsw_am.rs:609
-- pgcontext::hnsw_am::_hnsw_sparse_masked_candidates
CREATE  FUNCTION "_hnsw_sparse_masked_candidates"(
	"index_relation" regclass, /* PgRelation */
	"query" SparseVec, /* SparseVec */
	"allowed_heap_tids" anyarray, /* AnyArray */
	"limit" INT /* i32 */
) RETURNS TABLE (
	"heap_tid" TEXT,  /* String */
	"score" real  /* f32 */
)
STRICT 
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'hnsw_sparse_masked_candidates_wrapper';

-- crates/context-pg/src/sparse_search.rs:159
-- pgcontext::sparse_search::search_sparse
CREATE  FUNCTION "search_sparse"(
	"collection" TEXT, /* String */
	"vector_name" TEXT, /* String */
	"query" SparseVec, /* SparseVec */
	"filter" TEXT, /* Option < String > */
	"limit" INT /* i32 */
) RETURNS TABLE (
	"point_id" bigint,  /* i64 */
	"source_key" TEXT,  /* String */
	"score" real  /* f32 */
)

SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'search_sparse_collection_filtered_wrapper';

-- crates/context-pg/src/pgvector_ownership.rs:54
-- pgcontext::pgvector_ownership::start_pgvector_ownership_conversion
CREATE  FUNCTION "start_pgvector_ownership_conversion"(
	"target" regclass, /* PgRelation */
	"column_name" TEXT, /* String */
	"mode" TEXT DEFAULT 'fast', /* String */
	"metric" TEXT DEFAULT 'cosine', /* String */
	"application_uses_column_lists" bool DEFAULT false, /* bool */
	"application_dependencies_reviewed" bool DEFAULT false /* bool */
) RETURNS TABLE (
	"conversion_id" bigint,  /* i64 */
	"mode" TEXT,  /* String */
	"status" TEXT,  /* String */
	"schema_name" TEXT,  /* String */
	"table_name" TEXT,  /* String */
	"column_name" TEXT,  /* String */
	"target_type" TEXT,  /* String */
	"total_rows" bigint,  /* i64 */
	"processed_rows" bigint,  /* i64 */
	"mismatch_count" bigint,  /* i64 */
	"validation_attestations" TEXT[],  /* Vec < String > */
	"next_command" TEXT,  /* Option < String > */
	"error_message" TEXT  /* Option < String > */
)
STRICT
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'start_pgvector_ownership_conversion_wrapper';

-- crates/context-pg/src/query_builders.rs:110
-- pgcontext::query_builders::query_late_interaction
CREATE  FUNCTION "query_late_interaction"(
	"query_vectors" Vector[], /* Vec < Vector > */
	"candidates_per_query" INT, /* i32 */
	"limit" INT /* i32 */
) RETURNS jsonb /* JsonB */
STRICT
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'query_late_interaction_wrapper';

-- crates/context-pg/src/hybrid_query/late_interaction_ann.rs:193
-- pgcontext::hybrid_query::late_interaction_ann::search_late_interaction_ann
CREATE  FUNCTION "search_late_interaction_ann"(
	"collection" TEXT, /* String */
	"query_vectors" Vector[], /* Vec < Vector > */
	"candidates_per_query" INT, /* i32 */
	"limit" INT /* i32 */
) RETURNS TABLE (
	"point_id" bigint,  /* i64 */
	"source_key" TEXT,  /* String */
	"score" double precision  /* f64 */
)
STRICT 
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'search_owned_late_interaction_ann_wrapper';

-- crates/context-pg/src/hnsw_am.rs:509
-- pgcontext::hnsw_am::_hnsw_candidates
CREATE  FUNCTION "_hnsw_candidates"(
	"index_relation" regclass, /* PgRelation */
	"query" Vector, /* Vector */
	"limit" INT /* i32 */
) RETURNS TABLE (
	"heap_tid" TEXT,  /* String */
	"score" real  /* f32 */
)
STRICT 
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'hnsw_candidates_wrapper';

-- crates/context-pg/src/table_search/candidate_recheck.rs:420
-- pgcontext::table_search::candidate_recheck::_mmap_hnsw_artifact_candidates
CREATE  FUNCTION "_mmap_hnsw_artifact_candidates"(
	"collection" TEXT, /* String */
	"artifact_name" TEXT, /* String */
	"vector" Vector, /* Vector */
	"max_mapped_bytes" bigint, /* i64 */
	"candidate_limit" INT, /* i32 */
	"limit" INT /* i32 */
) RETURNS TABLE (
	"point_id" bigint,  /* i64 */
	"score" real,  /* f32 */
	"generation_high_water" bigint  /* i64 */
)
STRICT SECURITY DEFINER 
SET search_path TO pg_catalog, pgcontext
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'mmap_hnsw_artifact_candidates_internal_wrapper';

-- crates/context-pg/src/hybrid_query/late_interaction_ann.rs:273
-- pgcontext::hybrid_query::late_interaction_ann::explain_late_interaction_ann
CREATE  FUNCTION "explain_late_interaction_ann"(
	"collection" TEXT, /* String */
	"query_vectors" Vector[], /* Vec < Vector > */
	"candidates_per_query" INT /* i32 */
) RETURNS TABLE (
	"stage" TEXT,  /* String */
	"detail" TEXT,  /* String */
	"branch" TEXT,  /* Option < String > */
	"strategy" TEXT,  /* String */
	"status" QueryExplainStatus,  /* QueryExplainStatus */
	"estimated_candidates" bigint,  /* Option < i64 > */
	"candidate_budget" bigint  /* Option < i64 > */
)
STRICT 
SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'explain_owned_late_interaction_ann_wrapper';

-- crates/context-pg/src/query_builders.rs:23
-- pgcontext::query_builders::query_nearest
CREATE  FUNCTION "query_nearest"(
	"vector_name" TEXT, /* Option < String > */
	"vector" Vector, /* Vector */
	"filter" jsonb, /* Option < JsonB > */
	"limit" INT /* i32 */
) RETURNS jsonb /* JsonB */

SET search_path TO pg_catalog, pgcontext, public
LANGUAGE c /* Rust */
AS 'MODULE_PATHNAME', 'query_nearest_configured_wrapper';

-- crates/context-pg/src/pgvector_ownership/trigger.rs:14
-- pgcontext::pgvector_ownership::trigger::_sync_pgvector_ownership_columns
CREATE FUNCTION "_sync_pgvector_ownership_columns"()
	RETURNS TRIGGER
	LANGUAGE c
	AS 'MODULE_PATHNAME', '_sync_pgvector_ownership_columns_wrapper';

-- crates/context-pg/src/hnsw_am/mapped_lifecycle.rs:38
-- requires:
--   pgcontext_bootstrap


CREATE FUNCTION pgcontext._mapped_hnsw_sql_drop()
RETURNS event_trigger
AS 'MODULE_PATHNAME', 'pgcontext_hnsw_mapped_sql_drop'
LANGUAGE C;

CREATE EVENT TRIGGER pgcontext_mapped_hnsw_sql_drop
    ON sql_drop
    EXECUTE FUNCTION pgcontext._mapped_hnsw_sql_drop();

-- crates/context-pg/src/pgvector_ownership_catalog.rs:7
-- requires:
--   pgcontext_bootstrap


CREATE TABLE pgcontext._pgvector_ownership_conversions (
    conversion_id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    owner_role oid NOT NULL,
    source_table_oid oid NOT NULL,
    source_schema_name name NOT NULL,
    source_table_name name NOT NULL,
    source_column_name name NOT NULL,
    source_attnum int2 NOT NULL CHECK (source_attnum > 0),
    source_type_oid oid NOT NULL,
    source_type_name text NOT NULL CHECK (source_type_name <> ''),
    source_typmod int4 NOT NULL CHECK (source_typmod >= -1),
    shadow_column_name name NOT NULL,
    shadow_attnum int2 CHECK (shadow_attnum IS NULL OR shadow_attnum > 0),
    backup_column_name name NOT NULL,
    trigger_name name NOT NULL,
    index_name name NOT NULL,
    mode text NOT NULL CHECK (mode IN ('fast', 'restricted_online')),
    metric text NOT NULL CHECK (metric IN ('l2', 'inner_product', 'cosine', 'l1')),
    status text NOT NULL DEFAULT 'planned' CHECK (
        status IN (
            'planned',
            'backfilling',
            'index_pending',
            'ready',
            'cutover',
            'completed',
            'rolled_back',
            'failed'
        )
    ),
    dependency_manifest text[] NOT NULL DEFAULT ARRAY[]::text[],
    validation_attestations text[] NOT NULL DEFAULT ARRAY[]::text[],
    total_rows bigint NOT NULL DEFAULT 0 CHECK (total_rows >= 0),
    processed_rows bigint NOT NULL DEFAULT 0 CHECK (processed_rows >= 0),
    mismatch_count bigint NOT NULL DEFAULT 0 CHECK (mismatch_count >= 0),
    backfill_cursor text NOT NULL DEFAULT '(0,0)',
    source_checksum text,
    shadow_checksum text,
    error_message text,
    created_at timestamptz NOT NULL DEFAULT pg_catalog.now(),
    started_at timestamptz,
    updated_at timestamptz NOT NULL DEFAULT pg_catalog.now(),
    completed_at timestamptz,
    CHECK (source_schema_name <> ''),
    CHECK (source_table_name <> ''),
    CHECK (source_column_name <> ''),
    CHECK (shadow_column_name <> backup_column_name),
    CHECK (trigger_name <> index_name),
    CHECK (processed_rows <= total_rows),
    CHECK (completed_at IS NULL OR status IN ('completed', 'rolled_back'))
);

CREATE UNIQUE INDEX pgcontext_pgvector_ownership_conversions_active_source
    ON pgcontext._pgvector_ownership_conversions (source_table_oid, source_attnum)
    WHERE status IN (
        'planned',
        'backfilling',
        'index_pending',
        'ready',
        'cutover',
        'failed'
    );

REVOKE ALL ON TABLE pgcontext._pgvector_ownership_conversions FROM PUBLIC;

CREATE VIEW pgcontext._visible_pgvector_ownership_conversions
WITH (security_barrier = true) AS
SELECT conversions.*
  FROM pgcontext._pgvector_ownership_conversions AS conversions
 WHERE pg_catalog.pg_has_role(SESSION_USER, conversions.owner_role, 'MEMBER');

GRANT SELECT ON pgcontext._visible_pgvector_ownership_conversions TO PUBLIC;

-- crates/context-pg/src/late_interaction_catalog_schema.rs:9
-- requires:
--   Vector
--   create_catalog_tables


CREATE TABLE pgcontext._collection_late_interaction (
    collection_id bigint PRIMARY KEY
        REFERENCES pgcontext._collections(collection_id) ON DELETE CASCADE,
    source_table_oid oid NOT NULL,
    source_schema_name text NOT NULL,
    source_table_name text NOT NULL,
    token_column_name text NOT NULL,
    token_attnum int2 NOT NULL,
    dimensions int4,
    hnsw_index_oid oid,
    point_count bigint NOT NULL DEFAULT 0 CHECK (point_count >= 0),
    token_count bigint NOT NULL DEFAULT 0 CHECK (token_count >= 0),
    status text NOT NULL DEFAULT 'building'
        CHECK (status IN ('building', 'ready', 'stale', 'failed')),
    created_at timestamptz NOT NULL DEFAULT pg_catalog.now(),
    updated_at timestamptz NOT NULL DEFAULT pg_catalog.now(),
    CHECK (dimensions IS NULL OR dimensions > 0),
    CHECK ((status = 'ready') = (dimensions IS NOT NULL AND hnsw_index_oid IS NOT NULL))
);

ALTER TABLE pgcontext._collection_points
    ADD CONSTRAINT pgcontext_collection_points_collection_point_unique
    UNIQUE (collection_id, point_id);

CREATE TABLE pgcontext._collection_late_interaction_tokens (
    token_id bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY,
    collection_id bigint NOT NULL,
    point_id bigint NOT NULL,
    token_ordinal int4 NOT NULL CHECK (token_ordinal > 0),
    token_vector vector NOT NULL,
    created_at timestamptz NOT NULL DEFAULT pg_catalog.now(),
    updated_at timestamptz NOT NULL DEFAULT pg_catalog.now(),
    FOREIGN KEY (collection_id, point_id)
        REFERENCES pgcontext._collection_points(collection_id, point_id)
        ON DELETE CASCADE,
    UNIQUE (collection_id, point_id, token_ordinal)
);

CREATE VIEW pgcontext._visible_collection_late_interaction
WITH (security_barrier = true) AS
SELECT registrations.*
  FROM pgcontext._collection_late_interaction AS registrations
  JOIN pgcontext._collections AS collections USING (collection_id)
 WHERE pg_catalog.pg_has_role(SESSION_USER, collections.owner_role, 'MEMBER');

GRANT SELECT ON pgcontext._visible_collection_late_interaction TO PUBLIC;

CREATE FUNCTION pgcontext._capture_late_interaction_tokens()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
DECLARE
    registration pgcontext._collection_late_interaction%ROWTYPE;
    selected_record record;
    current_source_key text;
    previous_source_key text;
    token_vectors pgcontext.vector[];
    point bigint;
    minimum_dimensions int4;
    maximum_dimensions int4;
    deleted_token_count bigint;
    replacement_token_count bigint;
BEGIN
    SELECT *
      INTO registration
      FROM pgcontext._collection_late_interaction
     WHERE collection_id = TG_ARGV[0]::bigint;

    IF NOT FOUND OR registration.source_table_oid <> TG_RELID THEN
        RAISE EXCEPTION 'late-interaction source trigger binding is stale for relation %', TG_RELID
            USING ERRCODE = '55000';
    END IF;

    IF TG_OP IN ('UPDATE', 'DELETE') THEN
        previous_source_key := pg_catalog.to_jsonb(OLD)->>'id';
        IF previous_source_key IS NULL OR previous_source_key = '' THEN
            RAISE EXCEPTION 'late-interaction source key id must not be null or empty'
                USING ERRCODE = '22023';
        END IF;
    END IF;

    IF TG_OP = 'DELETE' THEN
        DELETE FROM pgcontext._collection_late_interaction_tokens AS tokens
         USING pgcontext._collection_points AS points
         WHERE tokens.collection_id = registration.collection_id
           AND tokens.point_id = points.point_id
           AND points.collection_id = registration.collection_id
           AND points.source_key = previous_source_key;
        GET DIAGNOSTICS deleted_token_count = ROW_COUNT;

        IF deleted_token_count > 0 THEN
            UPDATE pgcontext._collection_late_interaction
               SET point_count = point_count - 1,
                   token_count = token_count - deleted_token_count,
                   updated_at = pg_catalog.now()
             WHERE collection_id = registration.collection_id;
        END IF;

        UPDATE pgcontext._collection_points
           SET deleted_at = coalesce(deleted_at, pg_catalog.now()),
               updated_at = pg_catalog.now()
         WHERE collection_id = registration.collection_id
           AND source_key = previous_source_key;
        RETURN OLD;
    END IF;

    current_source_key := pg_catalog.to_jsonb(NEW)->>'id';
    IF current_source_key IS NULL OR current_source_key = '' THEN
        RAISE EXCEPTION 'late-interaction source key id must not be null or empty'
            USING ERRCODE = '22023';
    END IF;

    EXECUTE pg_catalog.format(
        'SELECT ($1).%I::pgcontext.vector[] AS token_vectors',
        registration.token_column_name
    )
    INTO selected_record
    USING NEW;
    token_vectors := selected_record.token_vectors;

    IF token_vectors IS NULL
       OR pg_catalog.cardinality(token_vectors) = 0
       OR pg_catalog.array_position(token_vectors, NULL) IS NOT NULL THEN
        RAISE EXCEPTION 'late-interaction token source must contain at least one non-null vector for source key %', current_source_key
            USING ERRCODE = '22023';
    END IF;
    IF pg_catalog.cardinality(token_vectors) > 16384 THEN
        RAISE EXCEPTION 'late-interaction token count exceeds per-point limit 16384 for source key %', current_source_key
            USING ERRCODE = '54000';
    END IF;

    SELECT pg_catalog.min(pgcontext.vector_dims(token)),
           pg_catalog.max(pgcontext.vector_dims(token))
      INTO minimum_dimensions, maximum_dimensions
      FROM pg_catalog.unnest(token_vectors) AS token;
    IF minimum_dimensions IS NULL OR minimum_dimensions <> maximum_dimensions THEN
        RAISE EXCEPTION 'late-interaction token dimensions must be uniform for source key %', current_source_key
            USING ERRCODE = '22023';
    END IF;
    IF registration.dimensions IS NOT NULL
       AND registration.dimensions <> minimum_dimensions THEN
        RAISE EXCEPTION 'late-interaction token dimension mismatch: expected %, found % for source key %',
            registration.dimensions,
            minimum_dimensions,
            current_source_key
            USING ERRCODE = '22023';
    END IF;

    IF TG_OP = 'UPDATE' AND previous_source_key <> current_source_key THEN
        DELETE FROM pgcontext._collection_late_interaction_tokens AS tokens
         USING pgcontext._collection_points AS points
         WHERE tokens.collection_id = registration.collection_id
           AND tokens.point_id = points.point_id
           AND points.collection_id = registration.collection_id
           AND points.source_key = previous_source_key;
        GET DIAGNOSTICS deleted_token_count = ROW_COUNT;
        IF deleted_token_count > 0 THEN
            UPDATE pgcontext._collection_late_interaction
               SET point_count = point_count - 1,
                   token_count = token_count - deleted_token_count,
                   updated_at = pg_catalog.now()
             WHERE collection_id = registration.collection_id;
        END IF;
        UPDATE pgcontext._collection_points
           SET deleted_at = coalesce(deleted_at, pg_catalog.now()),
               updated_at = pg_catalog.now()
         WHERE collection_id = registration.collection_id
           AND source_key = previous_source_key;
    END IF;

    INSERT INTO pgcontext._collection_points (collection_id, source_key)
    VALUES (registration.collection_id, current_source_key)
    ON CONFLICT (collection_id, source_key) DO UPDATE
        SET deleted_at = NULL,
            updated_at = pg_catalog.now()
    RETURNING point_id INTO point;

    DELETE FROM pgcontext._collection_late_interaction_tokens
     WHERE collection_id = registration.collection_id
       AND point_id = point;
    GET DIAGNOSTICS replacement_token_count = ROW_COUNT;
    INSERT INTO pgcontext._collection_late_interaction_tokens (
        collection_id,
        point_id,
        token_ordinal,
        token_vector
    )
    SELECT registration.collection_id,
           point,
           ordinal::int4,
           token
      FROM pg_catalog.unnest(token_vectors) WITH ORDINALITY AS expanded(token, ordinal);

    UPDATE pgcontext._collection_late_interaction
       SET dimensions = coalesce(dimensions, minimum_dimensions),
           point_count = point_count + CASE WHEN replacement_token_count = 0 THEN 1 ELSE 0 END,
           token_count = token_count
               - replacement_token_count
               + pg_catalog.cardinality(token_vectors),
           updated_at = pg_catalog.now()
     WHERE collection_id = registration.collection_id;
    RETURN NEW;
END;
$$;

CREATE FUNCTION pgcontext._begin_late_interaction_registration(
    p_collection_id bigint,
    p_source_table_oid oid,
    p_source_schema_name text,
    p_source_table_name text,
    p_token_column_name text,
    p_token_attnum int2
)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
DECLARE
    trigger_name text;
BEGIN
    IF NOT EXISTS (
        SELECT 1
          FROM pgcontext._collections AS collections
          JOIN pg_catalog.pg_class AS source_class
            ON source_class.oid = collections.source_table_oid
          JOIN pg_catalog.pg_namespace AS source_namespace
            ON source_namespace.oid = source_class.relnamespace
          JOIN pg_catalog.pg_attribute AS token_attribute
            ON token_attribute.attrelid = source_class.oid
           AND token_attribute.attname = p_token_column_name
           AND token_attribute.attnum = p_token_attnum
           AND token_attribute.attnum > 0
           AND NOT token_attribute.attisdropped
         WHERE collections.collection_id = p_collection_id
           AND pg_catalog.pg_has_role(SESSION_USER, collections.owner_role, 'MEMBER')
           AND source_class.oid = p_source_table_oid
           AND source_namespace.nspname = p_source_schema_name
           AND source_class.relname = p_source_table_name
           AND source_class.relkind = 'r'
           AND token_attribute.atttypid = 'pgcontext.vector[]'::pg_catalog.regtype
    ) THEN
        RAISE EXCEPTION 'invalid or unauthorized late-interaction registration for collection %', p_collection_id
            USING ERRCODE = '42501';
    END IF;
    IF NOT pg_catalog.has_table_privilege(SESSION_USER, p_source_table_oid, 'SELECT') THEN
        RAISE EXCEPTION 'permission denied for late-interaction source table %.%',
            p_source_schema_name,
            p_source_table_name
            USING ERRCODE = '42501';
    END IF;

    INSERT INTO pgcontext._collection_late_interaction (
        collection_id,
        source_table_oid,
        source_schema_name,
        source_table_name,
        token_column_name,
        token_attnum
    )
    VALUES (
        p_collection_id,
        p_source_table_oid,
        p_source_schema_name,
        p_source_table_name,
        p_token_column_name,
        p_token_attnum
    );

    trigger_name := pg_catalog.format('pgcontext_late_interaction_%s', p_collection_id);
    EXECUTE pg_catalog.format(
        'CREATE TRIGGER %I AFTER INSERT OR UPDATE OF id, %I OR DELETE ON %I.%I '
        'FOR EACH ROW EXECUTE FUNCTION pgcontext._capture_late_interaction_tokens(%L)',
        trigger_name,
        p_token_column_name,
        p_source_schema_name,
        p_source_table_name,
        p_collection_id::text
    );
END;
$$;

CREATE FUNCTION pgcontext._store_late_interaction_tokens(
    p_collection_id bigint,
    p_source_key text
)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
DECLARE
    registration pgcontext._collection_late_interaction%ROWTYPE;
    point bigint;
    minimum_dimensions int4;
    maximum_dimensions int4;
    replacement_token_count bigint;
    token_vectors pgcontext.vector[];
    loaded_row_count bigint;
BEGIN
    SELECT registrations.*
      INTO registration
      FROM pgcontext._collection_late_interaction AS registrations
      JOIN pgcontext._collections AS collections USING (collection_id)
     WHERE registrations.collection_id = p_collection_id
       AND pg_catalog.pg_has_role(SESSION_USER, collections.owner_role, 'MEMBER');
    IF NOT FOUND
       OR NOT pg_catalog.has_table_privilege(
           SESSION_USER,
           registration.source_table_oid,
           'SELECT'
       ) THEN
        RAISE EXCEPTION 'permission denied for late-interaction collection %', p_collection_id
            USING ERRCODE = '42501';
    END IF;
    IF p_source_key IS NULL OR p_source_key = '' THEN
        RAISE EXCEPTION 'late-interaction source key must not be null or empty'
            USING ERRCODE = '22023';
    END IF;

    EXECUTE pg_catalog.format(
        'SELECT source.%I::pgcontext.vector[]
           FROM %I.%I AS source
          WHERE source.id::text = $1
          LIMIT 1',
        registration.token_column_name,
        registration.source_schema_name,
        registration.source_table_name
    ) INTO token_vectors USING p_source_key;
    GET DIAGNOSTICS loaded_row_count = ROW_COUNT;
    IF loaded_row_count = 0 THEN
        RAISE EXCEPTION 'late-interaction source row does not exist for source key %', p_source_key
            USING ERRCODE = '42704';
    END IF;
    IF token_vectors IS NULL
       OR pg_catalog.cardinality(token_vectors) = 0
       OR pg_catalog.array_position(token_vectors, NULL) IS NOT NULL THEN
        RAISE EXCEPTION 'late-interaction token source must contain at least one non-null vector for source key %', p_source_key
            USING ERRCODE = '22023';
    END IF;
    IF pg_catalog.cardinality(token_vectors) > 16384 THEN
        RAISE EXCEPTION 'late-interaction token count exceeds per-point limit 16384 for source key %', p_source_key
            USING ERRCODE = '54000';
    END IF;

    SELECT pg_catalog.min(pgcontext.vector_dims(token)),
           pg_catalog.max(pgcontext.vector_dims(token))
      INTO minimum_dimensions, maximum_dimensions
      FROM pg_catalog.unnest(token_vectors) AS token;
    IF minimum_dimensions IS NULL OR minimum_dimensions <> maximum_dimensions THEN
        RAISE EXCEPTION 'late-interaction token dimensions must be uniform for source key %', p_source_key
            USING ERRCODE = '22023';
    END IF;
    IF registration.dimensions IS NOT NULL
       AND registration.dimensions <> minimum_dimensions THEN
        RAISE EXCEPTION 'late-interaction token dimension mismatch: expected %, found % for source key %',
            registration.dimensions,
            minimum_dimensions,
            p_source_key
            USING ERRCODE = '22023';
    END IF;

    INSERT INTO pgcontext._collection_points (collection_id, source_key)
    VALUES (p_collection_id, p_source_key)
    ON CONFLICT (collection_id, source_key) DO UPDATE
        SET updated_at = pg_catalog.now()
    RETURNING point_id INTO point;

    DELETE FROM pgcontext._collection_late_interaction_tokens
     WHERE collection_id = p_collection_id
       AND point_id = point;
    GET DIAGNOSTICS replacement_token_count = ROW_COUNT;
    INSERT INTO pgcontext._collection_late_interaction_tokens (
        collection_id,
        point_id,
        token_ordinal,
        token_vector
    )
    SELECT p_collection_id,
           point,
           ordinal::int4,
           token
      FROM pg_catalog.unnest(token_vectors) WITH ORDINALITY AS expanded(token, ordinal);

    UPDATE pgcontext._collection_late_interaction
       SET dimensions = coalesce(dimensions, minimum_dimensions),
           point_count = point_count + CASE WHEN replacement_token_count = 0 THEN 1 ELSE 0 END,
           token_count = token_count
               - replacement_token_count
               + pg_catalog.cardinality(token_vectors),
           updated_at = pg_catalog.now()
     WHERE collection_id = p_collection_id;
    RETURN point;
END;
$$;

CREATE FUNCTION pgcontext._finish_late_interaction_registration(
    p_collection_id bigint,
    p_dimensions int4
)
RETURNS oid
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
DECLARE
    index_name text;
    index_oid oid;
BEGIN
    IF p_dimensions <= 0 OR NOT EXISTS (
        SELECT 1
          FROM pgcontext._collection_late_interaction AS registrations
          JOIN pgcontext._collections AS collections USING (collection_id)
         WHERE registrations.collection_id = p_collection_id
           AND registrations.dimensions = p_dimensions
           AND pg_catalog.pg_has_role(SESSION_USER, collections.owner_role, 'MEMBER')
    ) THEN
        RAISE EXCEPTION 'invalid or unauthorized late-interaction finalization for collection %', p_collection_id
            USING ERRCODE = '42501';
    END IF;

    index_name := pg_catalog.format('pgcontext_late_interaction_%s_hnsw', p_collection_id);
    EXECUTE pg_catalog.format(
        'CREATE INDEX %I ON pgcontext._collection_late_interaction_tokens '
        'USING pgcontext_hnsw ((token_vector::pgcontext.vector(%s)) pgcontext.vector_hnsw_ip_ops) '
        'WHERE collection_id = %s',
        index_name,
        p_dimensions,
        p_collection_id
    );
    index_oid := pg_catalog.to_regclass(
        pg_catalog.format('pgcontext.%I', index_name)
    )::oid;
    UPDATE pgcontext._collection_late_interaction
       SET hnsw_index_oid = index_oid,
           status = 'ready',
           updated_at = pg_catalog.now()
     WHERE collection_id = p_collection_id;
    RETURN index_oid;
END;
$$;

CREATE FUNCTION pgcontext._late_interaction_ann_candidate_points(
    p_collection_id bigint,
    p_query vector,
    p_limit int4
)
RETURNS TABLE (point_id bigint)
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
DECLARE
    registration pgcontext._collection_late_interaction%ROWTYPE;
    previous_enable_seqscan text;
BEGIN
    IF p_limit IS NULL OR p_limit <= 0 THEN
        RAISE EXCEPTION 'late-interaction ANN candidate limit must be positive'
            USING ERRCODE = '22023';
    END IF;

    SELECT registrations.*
      INTO registration
      FROM pgcontext._collection_late_interaction AS registrations
      JOIN pgcontext._collections AS collections USING (collection_id)
      JOIN pg_catalog.pg_index AS index_metadata
        ON index_metadata.indexrelid = registrations.hnsw_index_oid
      JOIN pg_catalog.pg_class AS index_class
        ON index_class.oid = index_metadata.indexrelid
      JOIN pg_catalog.pg_am AS access_method
        ON access_method.oid = index_class.relam
      JOIN pg_catalog.pg_opclass AS operator_class
        ON operator_class.oid = index_metadata.indclass[0]
      JOIN pg_catalog.pg_namespace AS operator_namespace
        ON operator_namespace.oid = operator_class.opcnamespace
     WHERE registrations.collection_id = p_collection_id
       AND pg_catalog.pg_has_role(SESSION_USER, collections.owner_role, 'MEMBER')
       AND registrations.status = 'ready'
       AND registrations.dimensions IS NOT NULL
       AND index_metadata.indrelid = 'pgcontext._collection_late_interaction_tokens'::regclass
       AND index_metadata.indisvalid
       AND index_metadata.indisready
       AND access_method.amname = 'pgcontext_hnsw'
       AND index_class.relname = pg_catalog.format(
           'pgcontext_late_interaction_%s_hnsw',
           registrations.collection_id
       )
       AND index_metadata.indnkeyatts = 1
       AND index_metadata.indnatts = 1
       AND operator_namespace.nspname = 'pgcontext'
       AND operator_class.opcname = 'vector_hnsw_ip_ops'
       AND pg_catalog.regexp_replace(
           pg_catalog.pg_get_expr(
               index_metadata.indpred,
               index_metadata.indrelid,
               true
           ),
           '[()[:space:]]',
           '',
           'g'
       ) = pg_catalog.format('collection_id=%s', registrations.collection_id)
       AND pg_catalog.regexp_replace(
           pg_catalog.pg_get_indexdef(index_metadata.indexrelid, 1, true),
           '[()[:space:]]',
           '',
           'g'
       ) IN (
           pg_catalog.format('token_vector::vector%s', registrations.dimensions),
           pg_catalog.format('token_vector::pgcontext.vector%s', registrations.dimensions)
       );
    IF NOT FOUND THEN
        RAISE EXCEPTION 'late-interaction ANN generation is not ready or is unauthorized for collection %', p_collection_id
            USING ERRCODE = '55000';
    END IF;
    IF pgcontext.vector_dims(p_query) <> registration.dimensions THEN
        RAISE EXCEPTION 'late-interaction query dimension mismatch: expected %, found %',
            registration.dimensions,
            pgcontext.vector_dims(p_query)
            USING ERRCODE = '22023';
    END IF;

    previous_enable_seqscan := pg_catalog.current_setting('enable_seqscan');
    PERFORM pg_catalog.set_config('enable_seqscan', 'off', true);
    BEGIN
        RETURN QUERY EXECUTE pg_catalog.format(
            'SELECT tokens.point_id
               FROM pgcontext._collection_late_interaction_tokens AS tokens
              WHERE tokens.collection_id = $1
              ORDER BY (tokens.token_vector::pgcontext.vector(%s)) OPERATOR(pgcontext.<#>) $2
              LIMIT $3',
            registration.dimensions
        ) USING p_collection_id, p_query, p_limit;
    EXCEPTION WHEN others THEN
        PERFORM pg_catalog.set_config('enable_seqscan', previous_enable_seqscan, true);
        RAISE;
    END;
    PERFORM pg_catalog.set_config('enable_seqscan', previous_enable_seqscan, true);
END;
$$;

CREATE FUNCTION pgcontext._prepare_late_interaction_repair(
    p_collection_id bigint,
    p_source_table_oid oid,
    p_token_attnum int2
)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
DECLARE
    registration pgcontext._collection_late_interaction%ROWTYPE;
    trigger_name text;
    index_name text;
BEGIN
    SELECT registrations.*
      INTO registration
      FROM pgcontext._collection_late_interaction AS registrations
      JOIN pgcontext._collections AS collections USING (collection_id)
      JOIN pg_catalog.pg_class AS source_class
        ON source_class.oid = p_source_table_oid
      JOIN pg_catalog.pg_namespace AS source_namespace
        ON source_namespace.oid = source_class.relnamespace
      JOIN pg_catalog.pg_attribute AS token_attribute
        ON token_attribute.attrelid = source_class.oid
       AND token_attribute.attname = registrations.token_column_name
       AND token_attribute.attnum = p_token_attnum
       AND token_attribute.attnum > 0
       AND NOT token_attribute.attisdropped
     WHERE registrations.collection_id = p_collection_id
       AND collections.source_table_oid = p_source_table_oid
       AND pg_catalog.pg_has_role(SESSION_USER, collections.owner_role, 'MEMBER')
       AND source_namespace.nspname = registrations.source_schema_name
       AND source_class.relname = registrations.source_table_name
       AND source_class.relkind = 'r'
       AND token_attribute.atttypid = 'pgcontext.vector[]'::pg_catalog.regtype;
    IF NOT FOUND OR NOT pg_catalog.has_table_privilege(
        SESSION_USER,
        p_source_table_oid,
        'SELECT'
    ) THEN
        RAISE EXCEPTION 'invalid or unauthorized late-interaction repair for collection %', p_collection_id
            USING ERRCODE = '42501';
    END IF;

    trigger_name := pg_catalog.format('pgcontext_late_interaction_%s', p_collection_id);
    EXECUTE pg_catalog.format(
        'DROP TRIGGER IF EXISTS %I ON %I.%I',
        trigger_name,
        registration.source_schema_name,
        registration.source_table_name
    );
    index_name := pg_catalog.format('pgcontext_late_interaction_%s_hnsw', p_collection_id);
    EXECUTE pg_catalog.format('DROP INDEX IF EXISTS pgcontext.%I', index_name);

    DELETE FROM pgcontext._collection_late_interaction_tokens
     WHERE collection_id = p_collection_id;
    UPDATE pgcontext._collection_late_interaction
       SET source_table_oid = p_source_table_oid,
           token_attnum = p_token_attnum,
           dimensions = NULL,
           hnsw_index_oid = NULL,
           point_count = 0,
           token_count = 0,
           status = 'building',
           updated_at = pg_catalog.now()
     WHERE collection_id = p_collection_id;

    EXECUTE pg_catalog.format(
        'CREATE TRIGGER %I AFTER INSERT OR UPDATE OF id, %I OR DELETE ON %I.%I '
        'FOR EACH ROW EXECUTE FUNCTION pgcontext._capture_late_interaction_tokens(%L)',
        trigger_name,
        registration.token_column_name,
        registration.source_schema_name,
        registration.source_table_name,
        p_collection_id::text
    );
END;
$$;

CREATE FUNCTION pgcontext._cleanup_late_interaction_registration()
RETURNS trigger
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog, pgcontext
AS $$
DECLARE
    registration pgcontext._collection_late_interaction%ROWTYPE;
    trigger_name text;
    index_name text;
    current_schema_name text;
    current_table_name text;
BEGIN
    SELECT *
      INTO registration
      FROM pgcontext._collection_late_interaction
     WHERE collection_id = OLD.collection_id;
    IF NOT FOUND THEN
        RETURN OLD;
    END IF;

    trigger_name := pg_catalog.format(
        'pgcontext_late_interaction_%s',
        OLD.collection_id
    );
    SELECT namespace.nspname, source_class.relname
      INTO current_schema_name, current_table_name
      FROM pg_catalog.pg_class AS source_class
      JOIN pg_catalog.pg_namespace AS namespace
        ON namespace.oid = source_class.relnamespace
     WHERE source_class.oid = registration.source_table_oid;
    IF FOUND THEN
        EXECUTE pg_catalog.format(
            'DROP TRIGGER IF EXISTS %I ON %I.%I',
            trigger_name,
            current_schema_name,
            current_table_name
        );
    END IF;

    index_name := pg_catalog.format(
        'pgcontext_late_interaction_%s_hnsw',
        OLD.collection_id
    );
    EXECUTE pg_catalog.format('DROP INDEX IF EXISTS pgcontext.%I', index_name);
    RETURN OLD;
END;
$$;

CREATE TRIGGER pgcontext_cleanup_late_interaction_registration
BEFORE DELETE ON pgcontext._collections
FOR EACH ROW
EXECUTE FUNCTION pgcontext._cleanup_late_interaction_registration();

SELECT pg_catalog.pg_extension_config_dump(
    'pgcontext._collection_late_interaction',
    ''
);
SELECT pg_catalog.pg_extension_config_dump(
    'pgcontext._collection_late_interaction_tokens',
    ''
);

CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_ip_ops
    FOR TYPE pgcontext.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<#> (pgcontext.halfvec, pgcontext.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_negative_inner_product(pgcontext.halfvec, pgcontext.halfvec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_cosine_ops
    FOR TYPE pgcontext.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<=> (pgcontext.halfvec, pgcontext.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_cosine_distance(pgcontext.halfvec, pgcontext.halfvec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_l1_ops
    FOR TYPE pgcontext.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<+> (pgcontext.halfvec, pgcontext.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_l1_distance(pgcontext.halfvec, pgcontext.halfvec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_ip_ops
    FOR TYPE pgcontext.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<#> (pgcontext.sparsevec, pgcontext.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_negative_inner_product(pgcontext.sparsevec, pgcontext.sparsevec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_cosine_ops
    FOR TYPE pgcontext.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<=> (pgcontext.sparsevec, pgcontext.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_cosine_distance(pgcontext.sparsevec, pgcontext.sparsevec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_l1_ops
    FOR TYPE pgcontext.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<+> (pgcontext.sparsevec, pgcontext.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_l1_distance(pgcontext.sparsevec, pgcontext.sparsevec),
    STORAGE pgcontext.vector;

CREATE OPERATOR CLASS pgcontext.bitvec_hnsw_jaccard_ops
    FOR TYPE pgcontext.bitvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<%> (pgcontext.bitvec, pgcontext.bitvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.bitvec_jaccard_distance(pgcontext.bitvec, pgcontext.bitvec),
    STORAGE pgcontext.vector;
