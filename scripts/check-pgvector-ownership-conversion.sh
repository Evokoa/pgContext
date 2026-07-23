#!/usr/bin/env bash
# Live fast/restricted-online pgvector ownership-conversion certification.
set -euo pipefail

PSQL=${PGCONTEXT_CONVERSION_PSQL:-psql}
PG_DUMP=${PGCONTEXT_CONVERSION_PG_DUMP:-pg_dump}
PG_RESTORE=${PGCONTEXT_CONVERSION_PG_RESTORE:-pg_restore}
DB=${PGCONTEXT_CONVERSION_DB:-pgcontext_pgvector_conversion_check}
RESTORE_DB=${PGCONTEXT_CONVERSION_RESTORE_DB:-pgcontext_pgvector_conversion_restore_check}
DUMP_FILE=${TMPDIR:-/tmp}/pgcontext-pgvector-conversion-${$}.dump

if [[ ! "${DB}" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] \
  || [[ ! "${RESTORE_DB}" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
  echo "FAIL: conversion database names must be simple SQL identifiers" >&2
  exit 2
fi

cleanup() {
  if [[ -n "${sleeping_backend_shell_pid:-}" ]] \
    && kill -0 "${sleeping_backend_shell_pid}" 2>/dev/null; then
    kill "${sleeping_backend_shell_pid}" 2>/dev/null || true
    wait "${sleeping_backend_shell_pid}" 2>/dev/null || true
  fi
  ${PSQL} -d postgres -v ON_ERROR_STOP=1 \
    -c "DROP DATABASE IF EXISTS ${RESTORE_DB};" >/dev/null 2>&1 || true
  rm -f "${DUMP_FILE}"
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

q() {
  ${PSQL} -d "${DB}" -v ON_ERROR_STOP=1 -Atq -c "$1"
}

expect_failure() {
  local description=$1
  local sql=$2
  local output_file=${TMPDIR:-/tmp}/pgcontext-pgvector-conversion-failure.out
  if ${PSQL} -d "${DB}" -v ON_ERROR_STOP=1 -c "${sql}" >"${output_file}" 2>&1; then
    fail "${description} unexpectedly succeeded"
  fi
}

expect_failure_matching() {
  local description=$1
  local sql=$2
  local expected=$3
  local output_file=${TMPDIR:-/tmp}/pgcontext-pgvector-conversion-failure.out
  if ${PSQL} -d "${DB}" -v ON_ERROR_STOP=1 -c "${sql}" >"${output_file}" 2>&1; then
    fail "${description} unexpectedly succeeded"
  fi
  if ! grep -Fq "${expected}" "${output_file}"; then
    fail "${description} failed for the wrong reason; expected '${expected}'"
  fi
}

q_owner() {
  q "SET SESSION AUTHORIZATION conversion_owner; $1"
}

q_owner_top_level() {
  ${PSQL} -d "${DB}" -v ON_ERROR_STOP=1 -Atq \
    -c "SET SESSION AUTHORIZATION conversion_owner" \
    -c "$1"
}

${PSQL} -d postgres -v ON_ERROR_STOP=1 \
  -c "DROP DATABASE IF EXISTS ${DB};" \
  -c "CREATE DATABASE ${DB};" >/dev/null

q "CREATE EXTENSION vector;
   CREATE EXTENSION pgcontext;
   CREATE EXTENSION pgcontext_pgvector;
   DROP ROLE IF EXISTS conversion_owner;
   DROP ROLE IF EXISTS conversion_intruder;
   CREATE ROLE conversion_owner;
   GRANT USAGE ON SCHEMA public, pgcontext TO conversion_owner;
   GRANT CREATE ON SCHEMA public TO conversion_owner" >/dev/null

# Fast mode must be metadata-only, preserve values/NOT NULL, and rebuild every
# supported source ANN index under a canonical pgContext opclass.
q "CREATE TABLE conversion_fast (
     id bigint PRIMARY KEY,
     embedding public.vector(3) NOT NULL
   );
   INSERT INTO conversion_fast VALUES
     (1, '[1,0,0]'), (2, '[0,1,0]'), (3, '[0,0,1]');
   CREATE INDEX conversion_fast_embedding_hnsw
     ON conversion_fast USING hnsw (embedding public.vector_cosine_ops);
   ALTER TABLE conversion_fast OWNER TO conversion_owner" >/dev/null
fast_before=$(q "SELECT pg_catalog.string_agg(id || ':' || embedding::text, ',' ORDER BY id)
                   FROM conversion_fast")
fast_filenode_before=$(q "SELECT relfilenode FROM pg_catalog.pg_class
                           WHERE oid = 'conversion_fast'::pg_catalog.regclass")
fast_id=$(q_owner "SELECT conversion_id
               FROM pgcontext.start_pgvector_ownership_conversion(
                 'conversion_fast'::pg_catalog.regclass,
                 'embedding',
                 'fast',
                 'cosine',
                 application_dependencies_reviewed => true
               )")
[[ -n "${fast_id}" ]] || fail "fast conversion did not create a persisted job"
fast_status=$(q_owner "SELECT status FROM pgcontext.run_pgvector_ownership_conversion(
                   ${fast_id}, sessions_drained => true)")
[[ "${fast_status}" == "completed" ]] || fail "fast conversion ended in ${fast_status}"
fast_validation=$(q "SELECT total_rows = 3
                            AND processed_rows = 3
                            AND mismatch_count = 0
                            AND source_checksum IS NOT NULL
                            AND source_checksum = shadow_checksum
                       FROM pgcontext._visible_pgvector_ownership_conversions
                      WHERE conversion_id = ${fast_id}")
[[ "${fast_validation}" == "t" ]] \
  || fail "fast conversion did not persist exact row-count/checksum validation"
fast_after=$(q "SELECT pg_catalog.string_agg(id || ':' || embedding::text, ',' ORDER BY id)
                  FROM conversion_fast")
[[ "${fast_after}" == "${fast_before}" ]] || fail "fast conversion changed vector values"
fast_filenode_after=$(q "SELECT relfilenode FROM pg_catalog.pg_class
                          WHERE oid = 'conversion_fast'::pg_catalog.regclass")
[[ "${fast_filenode_after}" == "${fast_filenode_before}" ]] \
  || fail "fast conversion rewrote the source heap"
fast_binding=$(q "SELECT type_namespace.nspname || '.' || type.typname || ':' || attribute.attnotnull
                    FROM pg_catalog.pg_attribute AS attribute
                    JOIN pg_catalog.pg_type AS type ON type.oid = attribute.atttypid
                    JOIN pg_catalog.pg_namespace AS type_namespace ON type_namespace.oid = type.typnamespace
                   WHERE attribute.attrelid = 'conversion_fast'::pg_catalog.regclass
                     AND attribute.attname = 'embedding'")
[[ "${fast_binding}" == "pgcontext.vector:true" ]] \
  || fail "fast conversion produced binding ${fast_binding}"
fast_index=$(q "SELECT access_method.amname || ':' || namespace.nspname || '.' || opclass.opcname
                  FROM pg_catalog.pg_class AS index_relation
                  JOIN pg_catalog.pg_index AS index ON index.indexrelid = index_relation.oid
                  JOIN pg_catalog.pg_am AS access_method ON access_method.oid = index_relation.relam
                  JOIN pg_catalog.pg_opclass AS opclass ON opclass.oid = index.indclass[0]
                  JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = opclass.opcnamespace
                 WHERE index_relation.relname = 'conversion_fast_embedding_hnsw'")
[[ "${fast_index}" == "pgcontext_hnsw:pgcontext.vector_hnsw_cosine_ops" ]] \
  || fail "fast conversion rebuilt unexpected index ${fast_index}"
expect_failure_matching "fast conversion dimension invariant" \
  "INSERT INTO conversion_fast VALUES (4, '[1,0]'::pgcontext.vector)" \
  "violates check constraint"

# Fast mode refuses a session that still has named prepared SQL, then remains
# resumable and can be rolled back without touching the source.
q "CREATE TABLE conversion_prepared (id bigint, embedding public.vector(3));
   ALTER TABLE conversion_prepared OWNER TO conversion_owner" >/dev/null
expect_failure_matching "missing application dependency attestation" \
  "SET SESSION AUTHORIZATION conversion_owner;
   SELECT * FROM pgcontext.start_pgvector_ownership_conversion(
     'conversion_prepared'::pg_catalog.regclass, 'embedding', 'fast', 'cosine'
   )" \
  "application_dependencies_reviewed => true"
prepared_id=$(q_owner "SELECT conversion_id
                   FROM pgcontext.start_pgvector_ownership_conversion(
                     'conversion_prepared'::pg_catalog.regclass,
                     'embedding',
                     'fast',
                     'cosine',
                     application_dependencies_reviewed => true
                   )")
expect_failure_matching "fast conversion with prepared SQL" \
  "SET SESSION AUTHORIZATION conversion_owner;
   PREPARE conversion_held AS SELECT 1;
   SELECT * FROM pgcontext.run_pgvector_ownership_conversion(
     ${prepared_id}, sessions_drained => true
   )" \
  "current session has prepared statements"
prepared_status=$(q_owner "SELECT status FROM pgcontext.rollback_pgvector_ownership_conversion(${prepared_id})")
[[ "${prepared_status}" == "rolled_back" ]] || fail "planned fast rollback ended in ${prepared_status}"

q "CREATE TABLE conversion_ivfflat (id bigint, embedding public.vector(3));
   INSERT INTO conversion_ivfflat VALUES (1, '[1,0,0]'), (2, '[0,1,0]');
   CREATE INDEX conversion_ivfflat_ann
     ON conversion_ivfflat USING ivfflat (embedding public.vector_l2_ops)
     WITH (lists = 1);
   ALTER TABLE conversion_ivfflat OWNER TO conversion_owner" >/dev/null
ivfflat_id=$(q_owner "SELECT conversion_id
                        FROM pgcontext.start_pgvector_ownership_conversion(
                          'conversion_ivfflat'::pg_catalog.regclass,
                          'embedding',
                          'fast',
                          'l2',
                          application_dependencies_reviewed => true
                        )")
ivfflat_status=$(q_owner "SELECT status FROM pgcontext.run_pgvector_ownership_conversion(
                           ${ivfflat_id}, sessions_drained => true)")
[[ "${ivfflat_status}" == "completed" ]] || fail "IVFFlat conversion ended in ${ivfflat_status}"
ivfflat_replacement=$(q "SELECT access_method.amname || ':' || opclass.opcname
                           FROM pg_catalog.pg_class AS relation
                           JOIN pg_catalog.pg_index AS index ON index.indexrelid = relation.oid
                           JOIN pg_catalog.pg_am AS access_method ON access_method.oid = relation.relam
                           JOIN pg_catalog.pg_opclass AS opclass ON opclass.oid = index.indclass[0]
                          WHERE relation.relname = 'conversion_ivfflat_ann'")
[[ "${ivfflat_replacement}" == "pgcontext_hnsw:vector_hnsw_ops" ]] \
  || fail "IVFFlat was not rebuilt as canonical HNSW: ${ivfflat_replacement}"

# The supported online profile backfills in bounded calls, exposes the
# concurrent command rather than nesting it in SPI, preserves writes in both
# directions, and restores untouched pgvector objects on rollback.
q "CREATE TABLE conversion_online (
     id bigint PRIMARY KEY,
     embedding public.halfvec(3) NOT NULL
   );
   INSERT INTO conversion_online VALUES
     (1, '[1,0,0]'), (2, '[0,1,0]'), (3, '[0,0,1]');
   CREATE INDEX conversion_online_embedding_hnsw
     ON conversion_online USING hnsw (embedding public.halfvec_cosine_ops);
   ALTER TABLE conversion_online OWNER TO conversion_owner;
   REVOKE CREATE ON SCHEMA public FROM conversion_owner" >/dev/null
expect_failure_matching "online index-build schema privilege" \
  "SET SESSION AUTHORIZATION conversion_owner;
   SELECT * FROM pgcontext.start_pgvector_ownership_conversion(
     'conversion_online'::pg_catalog.regclass,
     'embedding',
     'restricted_online',
     'cosine',
     application_uses_column_lists => true,
     application_dependencies_reviewed => true
   )" \
  "requires CREATE on schema public"
q "GRANT CREATE ON SCHEMA public TO conversion_owner" >/dev/null
online_id=$(q_owner "SELECT conversion_id
                 FROM pgcontext.start_pgvector_ownership_conversion(
                   'conversion_online'::pg_catalog.regclass,
                   'embedding',
                   'restricted_online',
                   'cosine',
                   application_uses_column_lists => true,
                   application_dependencies_reviewed => true
                 )")
shadow_column=$(q "SELECT shadow_column_name
                     FROM pgcontext._visible_pgvector_ownership_conversions
                    WHERE conversion_id = ${online_id}")
q "INSERT INTO conversion_online (id, embedding) VALUES (4, '[0.5,0.5,0]');
   UPDATE conversion_online
      SET \"${shadow_column}\" = '[9,9,9]'::pgcontext.halfvec
    WHERE id = 1" >/dev/null
shadow_guard=$(q "SELECT embedding::text = \"${shadow_column}\"::text
                    FROM conversion_online WHERE id = 1")
[[ "${shadow_guard}" == "t" ]] || fail "online trigger allowed a direct shadow overwrite"

# The checkpoint belongs to the catalog, not the backend that produced it.
# Have one long-lived conversion-owner backend commit a bounded batch, then
# terminate that same backend while it sleeps. A new backend must advance from
# the cursor committed by the terminated producer rather than restart the job.
online_status=backfilling
${PSQL} -d "${DB}" -v ON_ERROR_STOP=1 -Atq \
  -c "SET SESSION AUTHORIZATION conversion_owner;
      SET application_name = 'pgcontext_conversion_resume_gate'" \
  -c "SELECT status
        FROM pgcontext.run_pgvector_ownership_conversion(${online_id}, 1)" \
  -c "SELECT pg_catalog.pg_sleep(30)" >/dev/null 2>&1 &
sleeping_backend_shell_pid=$!
sleeping_backend_pid=
for _ in $(seq 1 20); do
  sleeping_backend_pid=$(q "SELECT pid
                              FROM pg_catalog.pg_stat_activity
                             WHERE application_name = 'pgcontext_conversion_resume_gate'
                               AND wait_event = 'PgSleep'
                             LIMIT 1")
  [[ -z "${sleeping_backend_pid}" ]] || break
  sleep 0.1
done
[[ -n "${sleeping_backend_pid}" ]] || fail "could not observe backend selected for termination"
cursor_before_termination=$(q "SELECT backfill_cursor::text
                                 FROM pgcontext._visible_pgvector_ownership_conversions
                                WHERE conversion_id = ${online_id}")
cursor_progress=$(q "SELECT backfill_cursor <> '(0,0)' AND processed_rows <= 1
                       FROM pgcontext._visible_pgvector_ownership_conversions
                      WHERE conversion_id = ${online_id}")
[[ "${cursor_progress}" == "t" ]] \
  || fail "producer backend did not commit bounded cursor progress before termination"
terminated=$(q "SELECT pg_catalog.pg_terminate_backend(${sleeping_backend_pid})")
[[ "${terminated}" == "t" ]] || fail "could not terminate conversion backend"
wait "${sleeping_backend_shell_pid}" 2>/dev/null || true
online_status=$(q_owner "SELECT status FROM pgcontext.run_pgvector_ownership_conversion(${online_id}, 1)")
cursor_after_termination=$(q "SELECT backfill_cursor::text
                                FROM pgcontext._visible_pgvector_ownership_conversions
                               WHERE conversion_id = ${online_id}")
[[ "${cursor_after_termination}" != "${cursor_before_termination}" ]] \
  || fail "online conversion did not resume beyond the persisted cursor after backend termination"
for _ in $(seq 1 9); do
  online_status=$(q_owner "SELECT status FROM pgcontext.run_pgvector_ownership_conversion(${online_id}, 1)")
  [[ "${online_status}" == "backfilling" ]] || break
done
[[ "${online_status}" == "index_pending" ]] \
  || fail "online conversion did not finish bounded backfill: ${online_status}"
index_command=$(q_owner "SELECT next_command FROM pgcontext.run_pgvector_ownership_conversion(${online_id})")
[[ "${index_command}" == CREATE\ INDEX\ CONCURRENTLY* ]] \
  || fail "online conversion did not emit a concurrent index command: ${index_command}"
q_owner_top_level "${index_command}" >/dev/null
trigger_name=$(q "SELECT trigger_name
                    FROM pgcontext._visible_pgvector_ownership_conversions
                   WHERE conversion_id = ${online_id}")
q "ALTER TABLE conversion_online DISABLE TRIGGER \"${trigger_name}\";
   UPDATE conversion_online SET embedding = '[0.75,0.25,0]' WHERE id = 2;
   ALTER TABLE conversion_online ENABLE TRIGGER \"${trigger_name}\"" >/dev/null
online_status=$(q_owner "SELECT status FROM pgcontext.run_pgvector_ownership_conversion(${online_id})")
[[ "${online_status}" == "backfilling" ]] \
  || fail "post-index mismatch did not return to backfilling: ${online_status}"
for _ in $(seq 1 10); do
  online_status=$(q_owner "SELECT status FROM pgcontext.run_pgvector_ownership_conversion(${online_id}, 1)")
  [[ "${online_status}" == "backfilling" ]] || break
done
[[ "${online_status}" == "index_pending" ]] \
  || fail "mismatch recovery did not return to index_pending: ${online_status}"
online_status=$(q_owner "SELECT status FROM pgcontext.run_pgvector_ownership_conversion(${online_id})")
[[ "${online_status}" == "ready" ]] || fail "online index acknowledgement ended in ${online_status}"
online_validation=$(q "SELECT total_rows = (SELECT count(*) FROM conversion_online)
                              AND processed_rows = total_rows
                              AND mismatch_count = 0
                              AND source_checksum IS NOT NULL
                              AND source_checksum = shadow_checksum
                         FROM pgcontext._visible_pgvector_ownership_conversions
                        WHERE conversion_id = ${online_id}")
[[ "${online_validation}" == "t" ]] \
  || fail "online conversion did not persist exact row-count/checksum validation"
online_status=$(q_owner "SELECT status FROM pgcontext.cutover_pgvector_ownership_conversion(
                     ${online_id}, sessions_drained => true)")
[[ "${online_status}" == "cutover" ]] || fail "online cutover ended in ${online_status}"
online_binding=$(q "SELECT namespace.nspname || '.' || type.typname || ':' || attribute.attnotnull
                      FROM pg_catalog.pg_attribute AS attribute
                      JOIN pg_catalog.pg_type AS type ON type.oid = attribute.atttypid
                      JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = type.typnamespace
                     WHERE attribute.attrelid = 'conversion_online'::pg_catalog.regclass
                       AND attribute.attname = 'embedding'")
[[ "${online_binding}" == "pgcontext.halfvec:true" ]] \
  || fail "online cutover produced binding ${online_binding}"
backup_column=$(q "SELECT backup_column_name
                     FROM pgcontext._visible_pgvector_ownership_conversions
                    WHERE conversion_id = ${online_id}")
online_exact_equivalent=$(q "SELECT coalesce(pg_catalog.max(pg_catalog.abs(
                                      (embedding OPERATOR(pgcontext.<=>)
                                        '[0.2,0.8,0]'::pgcontext.halfvec)::float8
                                      - (\"${backup_column}\" OPERATOR(public.<=>)
                                        '[0.2,0.8,0]'::public.halfvec)::float8
                                    )) <= 0.000001, true)
                               FROM conversion_online")
[[ "${online_exact_equivalent}" == "t" ]] \
  || fail "online cutover changed exact halfvec distances beyond tolerance"
q "UPDATE conversion_online SET embedding = '[0.25,0.75,0]' WHERE id = 1" >/dev/null
reverse_guard=$(q "SELECT embedding::text = \"${backup_column}\"::text
                     FROM conversion_online WHERE id = 1")
[[ "${reverse_guard}" == "t" ]] || fail "cutover trigger did not maintain the rollback column"
q "DROP TRIGGER \"${trigger_name}\" ON conversion_online;
   DROP EXTENSION pgcontext_pgvector" >/dev/null
online_status=$(q_owner "SELECT status FROM pgcontext.rollback_pgvector_ownership_conversion(${online_id})")
[[ "${online_status}" == "rolled_back" ]] \
  || fail "online rollback after trigger loss ended in ${online_status}"
q "CREATE EXTENSION pgcontext_pgvector" >/dev/null
rollback_binding=$(q "SELECT namespace.nspname || '.' || type.typname || ':' || attribute.attnotnull
                        FROM pg_catalog.pg_attribute AS attribute
                        JOIN pg_catalog.pg_type AS type ON type.oid = attribute.atttypid
                        JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = type.typnamespace
                       WHERE attribute.attrelid = 'conversion_online'::pg_catalog.regclass
                         AND attribute.attname = 'embedding'")
[[ "${rollback_binding}" == "public.halfvec:true" ]] \
  || fail "online rollback produced binding ${rollback_binding}"
rollback_index=$(q "SELECT access_method.amname || ':' || opclass.opcname
                      FROM pg_catalog.pg_class AS index_relation
                      JOIN pg_catalog.pg_index AS index ON index.indexrelid = index_relation.oid
                      JOIN pg_catalog.pg_am AS access_method ON access_method.oid = index_relation.relam
                      JOIN pg_catalog.pg_opclass AS opclass ON opclass.oid = index.indclass[0]
                     WHERE index_relation.relname = 'conversion_online_embedding_hnsw'")
[[ "${rollback_index}" == "hnsw:halfvec_cosine_ops" ]] \
  || fail "online rollback did not preserve the original index: ${rollback_index}"

# Finalization is explicitly irreversible and leaves only canonical data/index
# objects, allowing both the bridge and pgvector itself to be dropped.
q "CREATE TABLE conversion_final (
     id bigint PRIMARY KEY,
     embedding public.vector(3)
   );
   INSERT INTO conversion_final VALUES
     (1, '[1,0,0]'), (2, NULL), (3, '[0,1,0]');
   ALTER TABLE conversion_final OWNER TO conversion_owner" >/dev/null
final_exact_before=$(q "SELECT pg_catalog.string_agg(
                               id || ':' || pg_catalog.round(
                                 (embedding OPERATOR(public.<->) '[0.25,0.75,0]'::public.vector)::numeric,
                                 6
                               )::text,
                               ',' ORDER BY id
                             )
                        FROM conversion_final
                       WHERE embedding IS NOT NULL")
final_id=$(q_owner "SELECT conversion_id
                FROM pgcontext.start_pgvector_ownership_conversion(
                  'conversion_final'::pg_catalog.regclass,
                  'embedding',
                  'restricted_online',
                  'l2',
                  application_uses_column_lists => true,
                  application_dependencies_reviewed => true
                )")
for _ in $(seq 1 10); do
  final_status=$(q_owner "SELECT status FROM pgcontext.run_pgvector_ownership_conversion(${final_id}, 1)")
  [[ "${final_status}" == "backfilling" ]] || break
done
[[ "${final_status}" == "index_pending" ]] || fail "final conversion backfill ended in ${final_status}"
final_command=$(q_owner "SELECT next_command FROM pgcontext.run_pgvector_ownership_conversion(${final_id})")
q_owner_top_level "${final_command}" >/dev/null
q_owner "SELECT status FROM pgcontext.run_pgvector_ownership_conversion(${final_id})" >/dev/null
q_owner "SELECT status FROM pgcontext.cutover_pgvector_ownership_conversion(
     ${final_id}, sessions_drained => true)" >/dev/null
final_status=$(q_owner "SELECT status FROM pgcontext.finalize_pgvector_ownership_conversion(${final_id})")
[[ "${final_status}" == "completed" ]] || fail "online finalization ended in ${final_status}"
final_validation=$(q "SELECT total_rows = 3
                             AND processed_rows = 3
                             AND mismatch_count = 0
                             AND source_checksum IS NOT NULL
                             AND source_checksum = shadow_checksum
                        FROM pgcontext._visible_pgvector_ownership_conversions
                       WHERE conversion_id = ${final_id}")
[[ "${final_validation}" == "t" ]] \
  || fail "final conversion did not persist exact row-count/checksum validation"
final_exact_after=$(q "SELECT pg_catalog.string_agg(
                              id || ':' || pg_catalog.round(
                                (embedding OPERATOR(pgcontext.<->) '[0.25,0.75,0]'::pgcontext.vector)::numeric,
                                6
                              )::text,
                              ',' ORDER BY id
                            )
                       FROM conversion_final
                      WHERE embedding IS NOT NULL")
[[ "${final_exact_after}" == "${final_exact_before}" ]] \
  || fail "finalized conversion changed exact distance results"

# Every supported pgvector dense type/metric pairing must preserve exact
# distances and rebuild to the corresponding canonical pgContext opclass.
for matrix_type in vector halfvec; do
  for matrix_metric in l2 inner_product cosine l1; do
    case "${matrix_metric}" in
      l2)
        matrix_operator='<->'
        source_opclass="${matrix_type}_l2_ops"
        target_opclass="${matrix_type}_hnsw_ops"
        ;;
      inner_product)
        matrix_operator='<#>'
        source_opclass="${matrix_type}_ip_ops"
        target_opclass="${matrix_type}_hnsw_ip_ops"
        ;;
      cosine)
        matrix_operator='<=>'
        source_opclass="${matrix_type}_cosine_ops"
        target_opclass="${matrix_type}_hnsw_cosine_ops"
        ;;
      l1)
        matrix_operator='<+>'
        source_opclass="${matrix_type}_l1_ops"
        target_opclass="${matrix_type}_hnsw_l1_ops"
        ;;
    esac
    matrix_table="conversion_matrix_${matrix_type}_${matrix_metric}"
    q "CREATE TABLE ${matrix_table} (
         id bigint PRIMARY KEY,
         embedding public.${matrix_type}(3) NOT NULL
       );
       INSERT INTO ${matrix_table} VALUES
         (1, '[1,0,0]'), (2, '[0,1,0]'), (3, '[0.5,0.5,0.5]');
       CREATE INDEX ${matrix_table}_ann
         ON ${matrix_table} USING hnsw (embedding public.${source_opclass});
       ALTER TABLE ${matrix_table} OWNER TO conversion_owner" >/dev/null
    matrix_before=$(q "SELECT pg_catalog.string_agg(
                              id || ':' || pg_catalog.to_char(
                                (embedding OPERATOR(public.${matrix_operator})
                                  '[0.2,0.8,0.1]'::public.${matrix_type})::float8,
                                'FM999999990.000000'
                              ),
                              ',' ORDER BY id
                            )
                       FROM ${matrix_table}")
    matrix_id=$(q_owner "SELECT conversion_id
                           FROM pgcontext.start_pgvector_ownership_conversion(
                             '${matrix_table}'::pg_catalog.regclass,
                             'embedding',
                             'fast',
                             '${matrix_metric}',
                             application_dependencies_reviewed => true
                           )")
    matrix_status=$(q_owner "SELECT status
                               FROM pgcontext.run_pgvector_ownership_conversion(
                                 ${matrix_id}, sessions_drained => true
                               )")
    [[ "${matrix_status}" == "completed" ]] \
      || fail "${matrix_type}/${matrix_metric} conversion ended in ${matrix_status}"
    matrix_after=$(q "SELECT pg_catalog.string_agg(
                             id || ':' || pg_catalog.to_char(
                               (embedding OPERATOR(pgcontext.${matrix_operator})
                                 '[0.2,0.8,0.1]'::pgcontext.${matrix_type})::float8,
                               'FM999999990.000000'
                             ),
                             ',' ORDER BY id
                           )
                      FROM ${matrix_table}")
    [[ "${matrix_after}" == "${matrix_before}" ]] \
      || fail "${matrix_type}/${matrix_metric} conversion changed exact distances"
    matrix_index=$(q "SELECT access_method.amname || ':' || opclass.opcname
                        FROM pg_catalog.pg_class AS relation
                        JOIN pg_catalog.pg_index AS index ON index.indexrelid = relation.oid
                        JOIN pg_catalog.pg_am AS access_method ON access_method.oid = relation.relam
                        JOIN pg_catalog.pg_opclass AS opclass ON opclass.oid = index.indclass[0]
                       WHERE relation.relname = '${matrix_table}_ann'")
    [[ "${matrix_index}" == "pgcontext_hnsw:${target_opclass}" ]] \
      || fail "${matrix_type}/${matrix_metric} rebuilt unexpected index ${matrix_index}"
  done
done

# Representative dependency and authorization refusals fail before any DDL.
q "CREATE TABLE conversion_blocked (
     id bigint,
     embedding public.vector(3) DEFAULT '[1,0,0]'::public.vector
   );
   ALTER TABLE conversion_blocked OWNER TO conversion_owner;
   CREATE TABLE conversion_rls (id bigint, embedding public.vector(3));
   ALTER TABLE conversion_rls OWNER TO conversion_owner;
   ALTER TABLE conversion_rls ENABLE ROW LEVEL SECURITY;
   ALTER TABLE conversion_rls FORCE ROW LEVEL SECURITY;
   CREATE TABLE conversion_renamed (id bigint, embedding public.vector(3));
   ALTER TABLE conversion_renamed OWNER TO conversion_owner;
   CREATE TABLE conversion_index_options (id bigint, embedding public.vector(3));
   CREATE INDEX conversion_index_options_hnsw
     ON conversion_index_options USING hnsw (embedding public.vector_l2_ops)
     WITH (m = 8, ef_construction = 32);
   ALTER TABLE conversion_index_options OWNER TO conversion_owner;
   CREATE TABLE conversion_index_comment (id bigint, embedding public.vector(3));
   CREATE INDEX conversion_index_comment_hnsw
     ON conversion_index_comment USING hnsw (embedding public.vector_l2_ops);
   COMMENT ON INDEX conversion_index_comment_hnsw IS 'application metadata';
   ALTER TABLE conversion_index_comment OWNER TO conversion_owner;
   CREATE ROLE conversion_intruder;
   GRANT USAGE ON SCHEMA pgcontext TO conversion_intruder;
   CREATE SCHEMA conversion_intruder_schema AUTHORIZATION conversion_intruder;
   CREATE FUNCTION conversion_intruder_schema.probe_conversion(bigint)
     RETURNS bool
     IMMUTABLE
     LANGUAGE plpgsql
     COST 0.0001
     AS \$\$ BEGIN RAISE EXCEPTION 'hidden conversion metadata reached probe'; END \$\$" >/dev/null
hidden_jobs=$(q "SET SESSION AUTHORIZATION conversion_intruder;
                  SELECT count(*)
                    FROM pgcontext._visible_pgvector_ownership_conversions
                   WHERE conversion_intruder_schema.probe_conversion(conversion_id)")
[[ "${hidden_jobs}" == "0" ]] \
  || fail "security-barrier ownership view exposed ${hidden_jobs} hidden jobs"
expect_failure_matching "column default dependency" \
  "SET SESSION AUTHORIZATION conversion_owner;
   SELECT * FROM pgcontext.start_pgvector_ownership_conversion(
     'conversion_blocked'::pg_catalog.regclass, 'embedding', 'fast', 'cosine',
     application_dependencies_reviewed => true
   )" \
  "column defaults are not supported"
expect_failure_matching "FORCE RLS without a policy" \
  "SET SESSION AUTHORIZATION conversion_owner;
   SELECT * FROM pgcontext.start_pgvector_ownership_conversion(
     'conversion_rls'::pg_catalog.regclass, 'embedding', 'fast', 'cosine',
     application_dependencies_reviewed => true
   )" \
  "row-level security enabled on the table is not supported"
expect_failure_matching "unrepresentable source index options" \
  "SET SESSION AUTHORIZATION conversion_owner;
   SELECT * FROM pgcontext.start_pgvector_ownership_conversion(
     'conversion_index_options'::pg_catalog.regclass, 'embedding', 'fast', 'l2',
     application_dependencies_reviewed => true
   )" \
  "per-index options that pgcontext_hnsw cannot preserve"
expect_failure_matching "source index comment preservation" \
  "SET SESSION AUTHORIZATION conversion_owner;
   SELECT * FROM pgcontext.start_pgvector_ownership_conversion(
     'conversion_index_comment'::pg_catalog.regclass, 'embedding', 'fast', 'l2',
     application_dependencies_reviewed => true
   )" \
  "has a comment that ownership conversion cannot preserve"
expect_failure_matching "non-owner conversion" \
  "SET SESSION AUTHORIZATION conversion_intruder;
   SELECT * FROM pgcontext.start_pgvector_ownership_conversion(
     'conversion_online'::pg_catalog.regclass, 'embedding', 'fast', 'cosine'
   )" \
  "must own conversion target"

renamed_id=$(q_owner "SELECT conversion_id
                        FROM pgcontext.start_pgvector_ownership_conversion(
                          'conversion_renamed'::pg_catalog.regclass,
                          'embedding',
                          'restricted_online',
                          'cosine',
                          application_uses_column_lists => true,
                          application_dependencies_reviewed => true
                        )")
q "ALTER TABLE conversion_renamed RENAME TO conversion_renamed_away" >/dev/null
expect_failure_matching "OID-bound rollback after relation rename" \
  "SET SESSION AUTHORIZATION conversion_owner;
   SELECT * FROM pgcontext.rollback_pgvector_ownership_conversion(${renamed_id})" \
  "conversion source binding changed"
q "ALTER TABLE conversion_renamed_away RENAME TO conversion_renamed" >/dev/null
renamed_trigger=$(q "SELECT trigger_name
                       FROM pgcontext._visible_pgvector_ownership_conversions
                      WHERE conversion_id = ${renamed_id}")
q "DROP TRIGGER \"${renamed_trigger}\" ON conversion_renamed" >/dev/null
renamed_status=$(q_owner "SELECT status FROM pgcontext.rollback_pgvector_ownership_conversion(${renamed_id})")
[[ "${renamed_status}" == "rolled_back" ]] \
  || fail "pre-cutover rollback after trigger loss ended in ${renamed_status}"

expect_failure_matching "direct private transition helper" \
  "SET SESSION AUTHORIZATION conversion_owner;
   SELECT pgcontext._transition_pgvector_ownership_conversion(
     ${fast_id}, 'completed', 'completed', NULL, 3, 3, 0, NULL, NULL, NULL, NULL, NULL
   )" \
  "ownership catalog helpers are internal"

q "CREATE SCHEMA conversion_custom;
   CREATE OPERATOR CLASS conversion_custom.vector_l2_ops
     FOR TYPE public.vector USING hnsw AS
     OPERATOR 1 public.<-> (public.vector, public.vector) FOR ORDER BY pg_catalog.float_ops,
     FUNCTION 1 public.vector_l2_squared_distance(public.vector, public.vector);
   CREATE TABLE conversion_custom_index (id bigint, embedding public.vector(3));
   CREATE INDEX conversion_custom_index_hnsw
     ON conversion_custom_index USING hnsw
       (embedding conversion_custom.vector_l2_ops);
   ALTER TABLE conversion_custom_index OWNER TO conversion_owner" >/dev/null
expect_failure_matching "custom ANN opclass" \
  "SET SESSION AUTHORIZATION conversion_owner;
   SELECT * FROM pgcontext.start_pgvector_ownership_conversion(
     'conversion_custom_index'::pg_catalog.regclass, 'embedding', 'fast', 'l2',
     application_dependencies_reviewed => true
   )" \
  "no certified pgContext equivalent"

q "CREATE SCHEMA vector_moved;
   ALTER EXTENSION vector SET SCHEMA vector_moved" >/dev/null
expect_failure_matching "relocated pgvector extension" \
  "SET SESSION AUTHORIZATION conversion_owner;
   SELECT * FROM pgcontext.start_pgvector_ownership_conversion(
     'conversion_online'::pg_catalog.regclass, 'embedding', 'fast', 'cosine'
   )" \
  "requires the certified"
q "ALTER EXTENSION vector SET SCHEMA public;
   DROP SCHEMA vector_moved" >/dev/null

# pgvector sparsevec uses a different packed representation. It must reject the
# metadata-only fast path, then preserve values and same-transaction DML through
# the validated restricted-online rewrite.
q "CREATE TABLE conversion_sparse (
     id bigint PRIMARY KEY,
     embedding public.sparsevec(4) NOT NULL
   );
   INSERT INTO conversion_sparse VALUES
     (1, '{1:1,4:0.5}/4'), (2, '{2:1}/4'), (3, '{3:-2}/4');
   CREATE INDEX conversion_sparse_source_hnsw
     ON conversion_sparse USING hnsw (embedding public.sparsevec_cosine_ops);
   ALTER TABLE conversion_sparse OWNER TO conversion_owner" >/dev/null
expect_failure_matching "sparsevec fast conversion" \
  "SET SESSION AUTHORIZATION conversion_owner;
   SELECT * FROM pgcontext.start_pgvector_ownership_conversion(
     'conversion_sparse'::pg_catalog.regclass, 'embedding', 'fast', 'cosine',
     application_dependencies_reviewed => true
   )" \
  "use mode => 'restricted_online'"
sparse_id=$(q_owner "SELECT conversion_id
                       FROM pgcontext.start_pgvector_ownership_conversion(
                         'conversion_sparse'::pg_catalog.regclass,
                         'embedding',
                         'restricted_online',
                         'cosine',
                         application_uses_column_lists => true,
                         application_dependencies_reviewed => true
                       )")
q_owner "INSERT INTO conversion_sparse (id, embedding)
           VALUES (4, '{1:0.25,2:0.75}/4'::public.sparsevec)" >/dev/null
for _ in $(seq 1 10); do
  sparse_status=$(q_owner "SELECT status
                             FROM pgcontext.run_pgvector_ownership_conversion(${sparse_id}, 1)")
  [[ "${sparse_status}" == "backfilling" ]] || break
done
[[ "${sparse_status}" == "index_pending" ]] \
  || fail "sparsevec conversion backfill ended in ${sparse_status}"
sparse_command=$(q_owner "SELECT next_command
                            FROM pgcontext.run_pgvector_ownership_conversion(${sparse_id})")
q_owner_top_level "${sparse_command}" >/dev/null
q_owner "SELECT status
           FROM pgcontext.run_pgvector_ownership_conversion(${sparse_id})" >/dev/null
q_owner "SELECT status
           FROM pgcontext.cutover_pgvector_ownership_conversion(
             ${sparse_id}, sessions_drained => true
           )" >/dev/null
q_owner "INSERT INTO conversion_sparse (id, embedding)
           VALUES (5, '{2:-0.5,4:1.25}/4'::pgcontext.sparsevec)" >/dev/null
sparse_status=$(q_owner "SELECT status
                           FROM pgcontext.finalize_pgvector_ownership_conversion(${sparse_id})")
[[ "${sparse_status}" == "completed" ]] \
  || fail "sparsevec finalization ended in ${sparse_status}"
sparse_result=$(q "SELECT type_namespace.nspname || '.' || type.typname || ':'
                          || pg_catalog.string_agg(
                               rows.id || '=' || rows.embedding::text,
                               ',' ORDER BY rows.id
                             )
                     FROM conversion_sparse AS rows
                     JOIN pg_catalog.pg_attribute AS attribute
                       ON attribute.attrelid = 'conversion_sparse'::pg_catalog.regclass
                      AND attribute.attname = 'embedding'
                     JOIN pg_catalog.pg_type AS type ON type.oid = attribute.atttypid
                     JOIN pg_catalog.pg_namespace AS type_namespace
                       ON type_namespace.oid = type.typnamespace
                    GROUP BY type_namespace.nspname, type.typname")
[[ "${sparse_result}" == "pgcontext.sparsevec:1={1:1,4:0.5}/4,2={2:1}/4,3={3:-2}/4,4={1:0.25,2:0.75}/4,5={2:-0.5,4:1.25}/4" ]] \
  || fail "sparsevec ownership conversion changed type or values: ${sparse_result}"

q "CREATE TABLE conversion_sparse_oversized (
     id bigint PRIMARY KEY,
     embedding public.sparsevec(16001) NOT NULL
   );
   INSERT INTO conversion_sparse_oversized VALUES (1, '{1:1}/16001');
   ALTER TABLE conversion_sparse_oversized OWNER TO conversion_owner" >/dev/null
expect_failure_matching "oversized sparsevec ownership conversion" \
  "SET SESSION AUTHORIZATION conversion_owner;
   SELECT * FROM pgcontext.start_pgvector_ownership_conversion(
     'conversion_sparse_oversized'::pg_catalog.regclass,
     'embedding',
     'restricted_online',
     'cosine',
     application_uses_column_lists => true,
     application_dependencies_reviewed => true
   )" \
  "large-dimension sparse support is planned"

q "DROP TABLE conversion_online, conversion_prepared, conversion_blocked,
              conversion_rls, conversion_renamed, conversion_custom_index,
              conversion_index_options, conversion_index_comment,
              conversion_sparse_oversized;
   DROP SCHEMA conversion_custom CASCADE;
   DROP SCHEMA conversion_intruder_schema CASCADE;
   DROP OWNED BY conversion_intruder;
   DROP ROLE conversion_intruder;
   DROP EXTENSION pgcontext_pgvector;
   DROP EXTENSION vector" >/dev/null
canonical_rows=$(q "SELECT count(*) FROM conversion_fast")
[[ "${canonical_rows}" == "3" ]] || fail "canonical fast data failed after pgvector removal"
canonical_nearest=$(q "SELECT id FROM conversion_final
                        WHERE embedding IS NOT NULL
                        ORDER BY embedding OPERATOR(pgcontext.<->) '[1,0,0]'::pgcontext.vector
                        LIMIT 1")
[[ "${canonical_nearest}" == "1" ]] || fail "finalized canonical HNSW failed after pgvector removal"
sparse_nearest=$(q "SELECT id FROM conversion_sparse
                      ORDER BY embedding OPERATOR(pgcontext.<=>)
                               '{1:1,4:0.5}/4'::pgcontext.sparsevec
                      LIMIT 1")
[[ "${sparse_nearest}" == "1" ]] \
  || fail "finalized canonical sparsevec HNSW failed after pgvector removal"

# A finalized database must dump and restore without the bridge or pgvector.
${PG_DUMP} -d "${DB}" --format=custom --file="${DUMP_FILE}"
${PSQL} -d postgres -v ON_ERROR_STOP=1 \
  -c "DROP DATABASE IF EXISTS ${RESTORE_DB};" \
  -c "CREATE DATABASE ${RESTORE_DB};" >/dev/null
${PG_RESTORE} --exit-on-error --no-owner --dbname="${RESTORE_DB}" "${DUMP_FILE}"
restored_extensions=$(${PSQL} -d "${RESTORE_DB}" -v ON_ERROR_STOP=1 -Atq \
  -c "SELECT pg_catalog.string_agg(extname, ',' ORDER BY extname)
        FROM pg_catalog.pg_extension
       WHERE extname IN ('pgcontext', 'pgcontext_pgvector', 'vector')")
[[ "${restored_extensions}" == "pgcontext" ]] \
  || fail "restored database has unexpected vector extensions: ${restored_extensions}"
restored_exact=$(${PSQL} -d "${RESTORE_DB}" -v ON_ERROR_STOP=1 -Atq \
  -c "SELECT pg_catalog.string_agg(
             id || ':' || pg_catalog.round(
               (embedding OPERATOR(pgcontext.<->) '[0.25,0.75,0]'::pgcontext.vector)::numeric,
               6
             )::text,
             ',' ORDER BY id
           )
        FROM conversion_final
       WHERE embedding IS NOT NULL")
[[ "${restored_exact}" == "${final_exact_after}" ]] \
  || fail "dump/restore changed finalized exact distance results"
restored_index=$(${PSQL} -d "${RESTORE_DB}" -v ON_ERROR_STOP=1 -Atq \
  -c "SELECT access_method.amname || ':' || opclass.opcname
        FROM pg_catalog.pg_class AS relation
        JOIN pg_catalog.pg_index AS index ON index.indexrelid = relation.oid
        JOIN pg_catalog.pg_am AS access_method ON access_method.oid = relation.relam
        JOIN pg_catalog.pg_opclass AS opclass ON opclass.oid = index.indclass[0]
       WHERE index.indrelid = 'conversion_final'::pg_catalog.regclass
         AND access_method.amname = 'pgcontext_hnsw'")
[[ "${restored_index}" == "pgcontext_hnsw:vector_hnsw_ops" ]] \
  || fail "dump/restore produced unexpected canonical index ${restored_index}"
${PSQL} -d postgres -v ON_ERROR_STOP=1 -c "DROP DATABASE ${RESTORE_DB};" >/dev/null
rm -f "${DUMP_FILE}"

q "REASSIGN OWNED BY conversion_owner TO CURRENT_USER;
   DROP OWNED BY conversion_owner;
   DROP ROLE conversion_owner" >/dev/null

${PSQL} -d postgres -v ON_ERROR_STOP=1 -c "DROP DATABASE ${DB};" >/dev/null
echo "pgvector ownership conversion verification passed (dense metric matrix, sparse rewrite, backend resume, rollback, dump/restore, and pgvector removal)"
