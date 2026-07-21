#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_backup_restore}"
RESTORE_DBNAME="${RESTORE_DBNAME:-${DBNAME}_restored}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

require_simple_identifier "${RESTORE_DBNAME}" "RESTORE_DBNAME"

DUMP_FILE="${HEAVY_TMPDIR}/${DBNAME}.dump"

cleanup() {
    rm -f "${DUMP_FILE}"
}
trap cleanup EXIT

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    body text NOT NULL,
    tenant text NOT NULL,
    metadata jsonb NOT NULL
);

INSERT INTO public.docs (id, embedding, body, tenant, metadata)
VALUES
    (1, '[0,0]'::vector, 'database internals', 'acme', '{"priority":"high","lang":"en"}'),
    (2, '[1,0]'::vector, 'query planning', 'acme', '{"priority":"low","lang":"en"}'),
    (3, '[5,5]'::vector, 'gardening notes', 'other', '{"priority":"low","lang":"fr"}');

SELECT * FROM pgcontext.create_collection('backup_docs', 'public.docs');
SELECT * FROM pgcontext.register_vector('backup_docs', 'embedding', 'embedding', 2, 'l2');
SELECT * FROM pgcontext.register_filter_column('backup_docs', 'tenant', 'tenant');
SELECT * FROM pgcontext.register_jsonb_path('backup_docs', 'priority', 'metadata', ARRAY['priority']);
SELECT * FROM pgcontext.upsert_points('backup_docs', ARRAY['1', '2', '3']);
SELECT * FROM pgcontext.record_query_stat('backup_docs', 'tenant:acme', 'search_filtered', 2, 3, 1.25);
SELECT * FROM pgcontext.register_model_version('backup_docs', 'embed-small', 'v1', 2, 'l2');
SELECT * FROM pgcontext.register_model_version('backup_docs', 'embed-small', 'v2', 2, 'l2');
SELECT * FROM pgcontext.create_embedding_migration('backup_docs', 'embed-small', 'v1', 'embed-small', 'v2', 3);
CREATE INDEX docs_embedding_hnsw_idx ON public.docs USING pgcontext_hnsw (embedding);
SQL

pg_dump -h "${PGHOST}" -p "${PGPORT}" -Fc -d "${DBNAME}" -f "${DUMP_FILE}"
printf 'backup_restore_dump_created\n'

drop_database "${RESTORE_DBNAME}"
create_database "${RESTORE_DBNAME}"

pg_restore -h "${PGHOST}" -p "${PGPORT}" -d "${RESTORE_DBNAME}" --exit-on-error "${DUMP_FILE}"
printf 'backup_restore_restore_completed\n'

DBNAME="${RESTORE_DBNAME}" psql_db <<'SQL'
DO $$
DECLARE
    nearest_source_key text;
    filtered_count bigint;
    priority_count bigint;
    point_count bigint;
    model_count bigint;
    migration_count bigint;
    telemetry_status text;
    restored_query_count bigint;
    restored_hnsw_indexes bigint;
BEGIN
    SELECT source_key
      INTO nearest_source_key
      FROM pgcontext.search('backup_docs', '[0,0]'::vector, 1);
    IF nearest_source_key IS DISTINCT FROM '1' THEN
        RAISE EXCEPTION 'unexpected restored nearest source key: %', nearest_source_key;
    END IF;
    RAISE NOTICE 'backup_restore_nearest_verified';

    SELECT count(*)
      INTO filtered_count
      FROM pgcontext.search(
          'backup_docs',
          '[0,0]'::vector,
          '{"must":[{"key":"tenant","match":"acme"}]}',
          10
      );
    IF filtered_count <> 2 THEN
        RAISE EXCEPTION 'unexpected restored tenant filter count: %', filtered_count;
    END IF;
    RAISE NOTICE 'backup_restore_filter_verified';

    SELECT count(*)
      INTO priority_count
      FROM pgcontext.facet('backup_docs', 'priority', NULL, 10)
     WHERE value = 'low' AND count = 2;
    IF priority_count <> 1 THEN
        RAISE EXCEPTION 'restored JSONB priority facet did not match expected count';
    END IF;
    RAISE NOTICE 'backup_restore_jsonb_facet_verified';

    SELECT count(*) INTO point_count FROM pgcontext.scroll('backup_docs', NULL, 10);
    IF point_count <> 3 THEN
        RAISE EXCEPTION 'unexpected restored point count: %', point_count;
    END IF;
    RAISE NOTICE 'backup_restore_scroll_verified';

    SELECT count(*) INTO model_count FROM pgcontext.model_versions()
     WHERE collection_name = 'backup_docs';
    IF model_count <> 2 THEN
        RAISE EXCEPTION 'unexpected restored model version count: %', model_count;
    END IF;
    RAISE NOTICE 'backup_restore_model_versions_verified';

    SELECT count(*) INTO migration_count FROM pgcontext.embedding_migrations()
     WHERE collection_name = 'backup_docs'
       AND status::text = 'Planned';
    IF migration_count <> 1 THEN
        RAISE EXCEPTION 'unexpected restored migration count: %', migration_count;
    END IF;
    RAISE NOTICE 'backup_restore_migration_verified';

    SELECT status::text, hnsw_indexes
      INTO telemetry_status, restored_hnsw_indexes
      FROM pgcontext.telemetry()
     WHERE collection_name = 'backup_docs';
    IF NOT FOUND OR telemetry_status IS DISTINCT FROM 'Active' OR restored_hnsw_indexes IS DISTINCT FROM 1 THEN
        RAISE EXCEPTION 'unexpected restored telemetry status %, hnsw indexes %',
            telemetry_status, restored_hnsw_indexes;
    END IF;
    RAISE NOTICE 'backup_restore_telemetry_verified';

    SELECT query_count
      INTO restored_query_count
      FROM pgcontext.query_cohort_stats()
     WHERE collection_name = 'backup_docs'
       AND cohort = 'tenant:acme'
       AND query_kind = 'search_filtered';
    IF restored_query_count IS DISTINCT FROM 1 THEN
        RAISE EXCEPTION 'unexpected restored query stat count: %', restored_query_count;
    END IF;
    RAISE NOTICE 'backup_restore_query_stats_verified';

    PERFORM 1 FROM pgcontext.index_status('public.docs_embedding_hnsw_idx')
     WHERE access_method = 'pgcontext_hnsw'
       AND status::text = 'Ready';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'restored HNSW index metadata was not ready';
    END IF;
    RAISE NOTICE 'backup_restore_hnsw_ready';
END
$$;
SQL
