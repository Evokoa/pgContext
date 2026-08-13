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
    embedding vector(2) NOT NULL,
    body text NOT NULL,
    tenant text NOT NULL,
    metadata jsonb NOT NULL,
    source_version bigint NOT NULL DEFAULT 1
);

INSERT INTO public.docs (id, embedding, body, tenant, metadata)
VALUES
    (1, '[0,0]'::vector, 'database internals', 'acme', '{"priority":"high","lang":"en"}'),
    (2, '[1,0]'::vector, 'query planning', 'acme', '{"priority":"low","lang":"en"}'),
    (3, '[5,5]'::vector, 'gardening notes', 'other', '{"priority":"low","lang":"fr"}');

SELECT * FROM pgcontext.create_collection('backup_docs', 'public.docs');
SELECT * FROM pgcontext.register_exact_first(
    'backup_docs', 'public.docs',
    jsonb_build_object(
        'version', 'exact_first_registration_v1',
        'key_column', 'id',
        'bindings', jsonb_build_array(
            jsonb_build_object(
                'name', 'embedding', 'column', 'embedding', 'kind', 'dense',
                'dimensions', 2, 'metric', 'l2'
            ),
            jsonb_build_object(
                'name', 'tenant', 'column', 'tenant', 'kind', 'filter'
            ),
            jsonb_build_object(
                'name', 'metadata', 'column', 'metadata', 'kind', 'payload'
            )
        )
    )
);
SELECT * FROM pgcontext.exact_first_advisor(
    'backup_docs',
    jsonb_build_object(
        'version', 'exact_first_advisor_v1',
        'memory_budget_bytes', 1048576,
        'build_window_seconds', 60,
        'update_millihertz', 0,
        'filter_selectivity_bps', 10000
    )
);
SELECT * FROM pgcontext.register_vector('backup_docs', 'embedding', 'embedding', 2, 'l2');
SELECT * FROM pgcontext.register_filter_column('backup_docs', 'tenant', 'tenant');
SELECT * FROM pgcontext.register_jsonb_path('backup_docs', 'priority', 'metadata', ARRAY['priority']);
SELECT * FROM pgcontext.upsert_points('backup_docs', ARRAY['1', '2', '3']);
SELECT * FROM pgcontext.record_query_stat('backup_docs', 'tenant:acme', 'search_filtered', 2, 3, 1.25);
CREATE INDEX docs_embedding_hnsw_idx ON public.docs USING pgcontext_hnsw (embedding);
SELECT pgcontext.register_embedding_profile(
    'backup_docs', 'embed_v1', 'embedding', 'public.docs_embedding_hnsw_idx',
    jsonb_build_object(
        'representation', 'dense', 'dimensions', 2, 'normalization', 'none',
        'metric', 'l2', 'provider', 'fixture', 'model', 'embed-small',
        'revision', 'v1', 'input_template', '{t}', 'output_template', '{v}',
        'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
        'configuration_hash', '0123456789abcdef'
    )
);
SELECT pgcontext.register_embedding_profile(
    'backup_docs', 'embed_v2', 'embedding', 'public.docs_embedding_hnsw_idx',
    jsonb_build_object(
        'representation', 'dense', 'dimensions', 2, 'normalization', 'none',
        'metric', 'l2', 'provider', 'fixture', 'model', 'embed-small',
        'revision', 'v2', 'input_template', '{t}', 'output_template', '{v}',
        'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
        'configuration_hash', 'fedcba9876543210'
    )
);
SELECT pgcontext.register_semantic_rerank_source(
    'backup_docs', 'body', 'body', 'source_version'
);
SELECT pgcontext.create_document_chunk_projection('public.backup_document_chunks');
SELECT pgcontext.register_chunking_profile(
    'backup_chunks_v1', 'plain_text_v1', 64, 96, 8, 8, 8388608, false
);
SELECT pgcontext.register_chunking_profile(
    'backup_chunks_v2', 'plain_text_v1', 48, 64, 8, 8, 8388608, false
);
SELECT pgcontext.register_document_source(
    'backup_docs', 'body', 'body', 'source_version',
    'public.backup_document_chunks', 'backup_chunks_v1'
);
SELECT pgcontext.prepare_chunking_profile_alias('backup_chunks_v1', 'backup_chunks_v2');
SELECT pgcontext.promote_chunking_profile_alias('backup_chunks_v1', 'backup_chunks_v2');
SELECT * FROM pgcontext.create_embedding_migration('backup_docs', 'embed_v1', 'embed_v2', 3);
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
    profile_count bigint;
    rerank_source_count bigint;
    rerank_envelope jsonb;
    chunk_profile_count bigint;
    chunk_alias_count bigint;
    chunk_retained_count bigint;
    document_source_count bigint;
    chunk_job_count bigint;
    chunk_job_id bigint;
    chunk_lease_token bigint;
    current_chunk_count bigint;
    migration_count bigint;
    telemetry_status text;
    restored_query_count bigint;
    restored_hnsw_indexes bigint;
    exact_registration_count bigint;
    exact_plan_count bigint;
    exact_job_count bigint;
    exact_target_count bigint;
    exact_state text;
    exact_source_key text;
BEGIN
    SELECT source_key
      INTO nearest_source_key
      FROM pgcontext.search('backup_docs', '[0,0]'::vector, 1);
    IF nearest_source_key IS DISTINCT FROM '1' THEN
        RAISE EXCEPTION 'unexpected restored nearest source key: %', nearest_source_key;
    END IF;
    RAISE NOTICE 'backup_restore_nearest_verified';

    SELECT count(*) INTO exact_registration_count
      FROM pgcontext._visible_exact_first_registrations
     WHERE collection_id = (
               SELECT collection_id FROM pgcontext._visible_collections
                WHERE collection_name = 'backup_docs'
           );
    SELECT count(*) INTO exact_plan_count
      FROM pgcontext._visible_exact_first_plans AS plans
      JOIN pgcontext._visible_exact_first_registrations AS registrations
        USING (exact_first_registration_id)
     WHERE registrations.collection_id = (
               SELECT collection_id FROM pgcontext._visible_collections
                WHERE collection_name = 'backup_docs'
           );
    SELECT count(*) INTO exact_job_count
      FROM pgcontext._visible_exact_first_plans AS plans
      JOIN pgcontext._visible_exact_first_registrations AS registrations
        USING (exact_first_registration_id)
     WHERE registrations.collection_id = (
               SELECT collection_id FROM pgcontext._visible_collections
                WHERE collection_name = 'backup_docs'
           )
       AND plans.build_job_id IS NOT NULL;
    SELECT count(*) INTO exact_target_count
      FROM pgcontext._visible_exact_first_targets AS targets
      JOIN pgcontext._visible_exact_first_registrations AS registrations
        USING (exact_first_registration_id)
     WHERE registrations.collection_id = (
               SELECT collection_id FROM pgcontext._visible_collections
                WHERE collection_name = 'backup_docs'
           );
    SELECT readiness_state INTO exact_state
      FROM pgcontext.exact_first_readiness('backup_docs');
    SELECT source_key INTO exact_source_key
      FROM pgcontext.exact_first_search(
          'backup_docs', 'embedding', '[0,0]'::vector, 1
      );
    IF exact_registration_count <> 1 OR exact_plan_count <> 1
       OR exact_job_count <> 0 OR exact_target_count <> 0
       OR exact_state IS DISTINCT FROM 'exact_only'
       OR exact_source_key IS DISTINCT FROM '1' THEN
        RAISE EXCEPTION 'restored exact-first logical/transient contract failed';
    END IF;
    RAISE NOTICE 'backup_restore_exact_first_verified';

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

    SELECT count(*) INTO profile_count FROM pgcontext.embedding_profiles()
     WHERE collection_name = 'backup_docs';
    IF profile_count <> 2 THEN
        RAISE EXCEPTION 'unexpected restored embedding profile count: %', profile_count;
    END IF;
    SELECT count(*) INTO rerank_source_count
      FROM pgcontext._visible_semantic_rerank_sources
     WHERE source_name = 'body' AND status = 'ready';
    IF rerank_source_count <> 1 THEN
        RAISE EXCEPTION 'unexpected restored semantic rerank source count: %',
            rerank_source_count;
    END IF;
    SELECT pgcontext.prepare_semantic_rerank(
               'backup_docs', 'body', 'database internals',
               jsonb_build_array(
                   jsonb_build_object(
                       'occurrence_id', 1,
                       'point_id', (
                           SELECT point_id
                             FROM pgcontext._visible_collection_points
                            WHERE source_key = '1'
                              AND collection_id = (
                                      SELECT collection_id
                                        FROM pgcontext._visible_collections
                                       WHERE collection_name = 'backup_docs'
                                  )
                       ),
                       'fused_rank', 1,
                       'fused_score', 1.0,
                       'contributions', jsonb_build_array(
                           jsonb_build_object(
                               'profile', 'embed_v1', 'rank', 1,
                               'native_score', 0.0, 'weight', 1.0,
                               'contribution', 1.0
                           )
                       )
                   )
               ),
               'restore-fixture', 1
           )
      INTO rerank_envelope;
    IF rerank_envelope->'candidates'->0->>'text' IS DISTINCT FROM 'database internals' THEN
        RAISE EXCEPTION 'restored semantic rerank source did not hydrate current text';
    END IF;
    RAISE NOTICE 'backup_restore_embedding_profiles_verified';

    SELECT count(*) INTO chunk_profile_count
      FROM pgcontext._visible_chunking_profiles
     WHERE profile_name = 'backup_chunks_v1' AND status = 'ready';
    SELECT count(*) INTO document_source_count
      FROM pgcontext._visible_document_sources
     WHERE source_name = 'body' AND status = 'ready';
    SELECT count(*) INTO chunk_job_count
      FROM pgcontext._visible_document_chunk_jobs;
    SELECT count(*) INTO chunk_alias_count
      FROM pgcontext._visible_chunking_profile_aliases AS aliases
      JOIN pgcontext._visible_chunking_profiles AS profiles
        USING (chunking_profile_id)
     WHERE aliases.alias_name = 'backup_chunks_v1'
       AND profiles.profile_name = 'backup_chunks_v2';
    SELECT count(*) INTO chunk_retained_count
      FROM pgcontext._chunking_profile_alias_retained AS retained
      JOIN pgcontext._chunking_profile_aliases AS aliases
        USING (chunking_profile_alias_id)
      JOIN pgcontext._chunking_profiles AS profiles
        ON profiles.chunking_profile_id = retained.chunking_profile_id
     WHERE aliases.alias_name = 'backup_chunks_v1'
       AND profiles.profile_name = 'backup_chunks_v1';
    IF chunk_profile_count <> 1 OR document_source_count <> 1 OR chunk_job_count <> 0
       OR chunk_alias_count <> 1 OR chunk_retained_count <> 1 THEN
        RAISE EXCEPTION 'restored document chunk registration or transient-state contract failed';
    END IF;
    PERFORM pgcontext.enqueue_document_chunking('backup_docs', 'body', ARRAY['1']);
    SELECT job_id, lease_token INTO chunk_job_id, chunk_lease_token
      FROM pgcontext.claim_document_chunk_jobs(1, 60000, 'backup-restore-worker');
    PERFORM pgcontext.fake_process_document_chunk_job(chunk_job_id, chunk_lease_token);
    SELECT count(*) INTO current_chunk_count
      FROM pgcontext.current_document_chunks('backup_docs', 'body', ARRAY['1']);
    IF current_chunk_count = 0 THEN
        RAISE EXCEPTION 'restored document chunk source did not republish current chunks';
    END IF;
    RAISE NOTICE 'backup_restore_document_chunking_verified';

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
