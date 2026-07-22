#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_mapped_hnsw_lifecycle}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

mapped_index_directory() {
    local index_oid="$1"
    printf '%s/pgcontext_hnsw_mapped/%s\n' "${DATABASE_DIRECTORY}" "${index_oid}"
}

assert_generation_published() {
    local index_oid="$1"
    local directory
    local file_count
    directory="$(mapped_index_directory "${index_oid}")"
    file_count="$(find "${directory}" -maxdepth 1 -type f -name '*.pgctxseg' 2>/dev/null | wc -l | tr -d ' ')"
    if [[ "${file_count}" != "1" ]]; then
        echo "expected one mapped generation in ${directory}, found ${file_count}" >&2
        exit 1
    fi
}

assert_generation_reclaimed() {
    local index_oid="$1"
    local directory
    directory="$(mapped_index_directory "${index_oid}")"
    if [[ -e "${directory}" ]]; then
        echo "expected mapped index directory to be reclaimed: ${directory}" >&2
        exit 1
    fi
}

assert_drop_marker_count() {
    local index_oid="$1"
    local expected="$2"
    local actual
    actual="$(find "${DATABASE_DIRECTORY}/pgcontext_hnsw_mapped/.pending_drops" -maxdepth 2 \
        -type f -name ".pending_drop_${index_oid}_*.pgctxdrop" 2>/dev/null \
        | wc -l | tr -d ' ')"
    if [[ "${actual}" != "${expected}" ]]; then
        echo "expected ${expected} durable drop markers for index ${index_oid}, found ${actual}" >&2
        exit 1
    fi
}

publish_index_generation() {
    local table_name="$1"
    psql_db -At <<SQL >/dev/null
SET enable_seqscan = off;
SET enable_bitmapscan = off;
SET pgcontext.hnsw_shared_serving = off;
SELECT id
  FROM public.${table_name}
 ORDER BY embedding OPERATOR(pgcontext.<->) '[1,0]'::vector, id
 LIMIT 1;
SQL
}

start_and_install_extension

# Prepared transactions are disabled in a stock PostgreSQL cluster. Start this
# isolated pgrx server with a command-line override for the 2PC lifecycle gate,
# then restore its normal launch configuration even if the script fails.
PREPARED_OVERRIDE_ACTIVE=0
RESTRICTED_DROP_DIRECTORY=""
PGRX_DATA_DIRECTORY="$(psql_postgres -Atc 'SHOW data_directory' | tail -n 1)"
PG_CTL="$(pg_bin pg_ctl)"
restore_pgrx_server() {
    local status="$?"
    trap - EXIT
    if [[ -n "${RESTRICTED_DROP_DIRECTORY}" && -d "${RESTRICTED_DROP_DIRECTORY}" ]]; then
        chmod 700 "${RESTRICTED_DROP_DIRECTORY}" || true
    fi
    if [[ "${PREPARED_OVERRIDE_ACTIVE}" == "1" ]]; then
        "${PG_CTL}" -D "${PGRX_DATA_DIRECTORY}" -m fast -w stop >/dev/null || true
        cargo pgrx start "${PG_VERSION}" >/dev/null || true
    fi
    exit "${status}"
}
trap restore_pgrx_server EXIT
if [[ "$(psql_postgres -Atc 'SHOW max_prepared_transactions' | tail -n 1)" == "0" ]]; then
    cargo pgrx stop "${PG_VERSION}"
    PREPARED_OVERRIDE_ACTIVE=1
    "${PG_CTL}" -D "${PGRX_DATA_DIRECTORY}" \
        -o '-c max_prepared_transactions=64' -w start >/dev/null
fi
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;
CREATE TABLE public.mapped_drop_index_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL
);
INSERT INTO public.mapped_drop_index_docs
SELECT value, format('[%s,0]', value)::vector
  FROM generate_series(1, 32) AS value;
CREATE INDEX mapped_drop_index_docs_hnsw
    ON public.mapped_drop_index_docs
 USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
SQL

DATA_DIRECTORY="$(psql_db -Atc 'SHOW data_directory' | tail -n 1)"
DATABASE_RELATIVE_DIRECTORY="$(psql_db -Atc "SELECT pg_catalog.pg_relation_filepath('pg_catalog.pg_class'::regclass)" | tail -n 1)"
DATABASE_DIRECTORY="${DATA_DIRECTORY}/$(dirname "${DATABASE_RELATIVE_DIRECTORY}")"

publish_index_generation mapped_drop_index_docs
DROP_INDEX_OID="$(psql_db -Atc "SELECT 'mapped_drop_index_docs_hnsw'::regclass::oid::bigint" | tail -n 1)"
assert_generation_published "${DROP_INDEX_OID}"

psql_db <<'SQL'
BEGIN;
SAVEPOINT before_mapped_drop;
DROP INDEX public.mapped_drop_index_docs_hnsw;
ROLLBACK TO SAVEPOINT before_mapped_drop;
COMMIT;
SQL
assert_generation_published "${DROP_INDEX_OID}"
printf 'mapped_hnsw_drop_rollback_preserved\n'

psql_db -c 'DROP INDEX public.mapped_drop_index_docs_hnsw'
assert_generation_reclaimed "${DROP_INDEX_OID}"
printf 'mapped_hnsw_drop_index_reclaimed\n'

psql_db <<'SQL'
CREATE TABLE public.mapped_drop_table_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL
);
INSERT INTO public.mapped_drop_table_docs
SELECT value, format('[%s,0]', value)::vector
  FROM generate_series(1, 32) AS value;
CREATE INDEX mapped_drop_table_docs_hnsw
    ON public.mapped_drop_table_docs
 USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
SQL
publish_index_generation mapped_drop_table_docs
DROP_TABLE_INDEX_OID="$(psql_db -Atc "SELECT 'mapped_drop_table_docs_hnsw'::regclass::oid::bigint" | tail -n 1)"
assert_generation_published "${DROP_TABLE_INDEX_OID}"
psql_db -c 'DROP TABLE public.mapped_drop_table_docs'
assert_generation_reclaimed "${DROP_TABLE_INDEX_OID}"
printf 'mapped_hnsw_drop_table_reclaimed\n'

psql_db <<'SQL'
CREATE TABLE public.mapped_prepared_drop_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL
);
INSERT INTO public.mapped_prepared_drop_docs
SELECT value, format('[%s,0]', value)::vector
  FROM generate_series(1, 32) AS value;
CREATE INDEX mapped_prepared_drop_docs_hnsw
    ON public.mapped_prepared_drop_docs
 USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
CREATE TABLE public.mapped_prepared_sweeper_docs
    (LIKE public.mapped_prepared_drop_docs INCLUDING ALL);
INSERT INTO public.mapped_prepared_sweeper_docs
SELECT * FROM public.mapped_prepared_drop_docs;
CREATE INDEX mapped_prepared_sweeper_docs_hnsw
    ON public.mapped_prepared_sweeper_docs
 USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
SQL
publish_index_generation mapped_prepared_drop_docs
publish_index_generation mapped_prepared_sweeper_docs
PREPARED_COMMIT_INDEX_OID="$(psql_db -Atc "SELECT 'mapped_prepared_drop_docs_hnsw'::regclass::oid::bigint" | tail -n 1)"
assert_generation_published "${PREPARED_COMMIT_INDEX_OID}"
psql_db <<'SQL'
BEGIN;
DROP INDEX public.mapped_prepared_drop_docs_hnsw;
PREPARE TRANSACTION 'pgcontext_mapped_drop_commit';
SQL
publish_index_generation mapped_prepared_sweeper_docs
assert_generation_published "${PREPARED_COMMIT_INDEX_OID}"
assert_drop_marker_count "${PREPARED_COMMIT_INDEX_OID}" 1
psql_db -c "COMMIT PREPARED 'pgcontext_mapped_drop_commit'"
assert_generation_published "${PREPARED_COMMIT_INDEX_OID}"
assert_drop_marker_count "${PREPARED_COMMIT_INDEX_OID}" 1
"${PG_CTL}" -D "${PGRX_DATA_DIRECTORY}" -m immediate -w stop >/dev/null
if [[ "${PREPARED_OVERRIDE_ACTIVE}" == "1" ]]; then
    "${PG_CTL}" -D "${PGRX_DATA_DIRECTORY}" \
        -o '-c max_prepared_transactions=64' -w start >/dev/null
else
    "${PG_CTL}" -D "${PGRX_DATA_DIRECTORY}" -w start >/dev/null
fi
assert_generation_published "${PREPARED_COMMIT_INDEX_OID}"
assert_drop_marker_count "${PREPARED_COMMIT_INDEX_OID}" 1
printf 'mapped_hnsw_drop_marker_restart_preserved\n'
RESTRICTED_DROP_DIRECTORY="$(mapped_index_directory "${PREPARED_COMMIT_INDEX_OID}")"
chmod 000 "${RESTRICTED_DROP_DIRECTORY}"
for _restricted_reconcile_scan in $(seq 1 16); do
    publish_index_generation mapped_prepared_sweeper_docs
done
if [[ ! -d "${RESTRICTED_DROP_DIRECTORY}" ]]; then
    echo "expected failed cleanup to retain ${RESTRICTED_DROP_DIRECTORY}" >&2
    exit 1
fi
assert_drop_marker_count "${PREPARED_COMMIT_INDEX_OID}" 1
chmod 700 "${RESTRICTED_DROP_DIRECTORY}"
RESTRICTED_DROP_DIRECTORY=""
for _retry_reconcile_scan in $(seq 1 16); do
    publish_index_generation mapped_prepared_sweeper_docs
done
assert_generation_reclaimed "${PREPARED_COMMIT_INDEX_OID}"
assert_drop_marker_count "${PREPARED_COMMIT_INDEX_OID}" 0
psql_db <<'SQL'
CREATE INDEX mapped_prepared_drop_docs_replacement_hnsw
    ON public.mapped_prepared_drop_docs
 USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
SQL
publish_index_generation mapped_prepared_drop_docs
printf 'mapped_hnsw_prepared_commit_reconciled\n'

PREPARED_ABORT_INDEX_OID="$(psql_db -Atc "SELECT 'mapped_prepared_drop_docs_replacement_hnsw'::regclass::oid::bigint" | tail -n 1)"
assert_generation_published "${PREPARED_ABORT_INDEX_OID}"
psql_db <<'SQL'
BEGIN;
DROP INDEX public.mapped_prepared_drop_docs_replacement_hnsw;
PREPARE TRANSACTION 'pgcontext_mapped_drop_abort';
SQL
publish_index_generation mapped_prepared_sweeper_docs
assert_generation_published "${PREPARED_ABORT_INDEX_OID}"
assert_drop_marker_count "${PREPARED_ABORT_INDEX_OID}" 1
psql_db -c "ROLLBACK PREPARED 'pgcontext_mapped_drop_abort'"
for _abort_reconcile_scan in $(seq 1 16); do
    publish_index_generation mapped_prepared_drop_docs
done
assert_generation_published "${PREPARED_ABORT_INDEX_OID}"
assert_drop_marker_count "${PREPARED_ABORT_INDEX_OID}" 0
printf 'mapped_hnsw_prepared_abort_preserved\n'

for fairness_id in $(seq 1 33); do
    psql_db <<SQL >/dev/null
CREATE TABLE public.mapped_fairness_${fairness_id} (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL
);
INSERT INTO public.mapped_fairness_${fairness_id} VALUES (1, '[1,0]'::vector);
CREATE INDEX mapped_fairness_${fairness_id}_hnsw
    ON public.mapped_fairness_${fairness_id}
 USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
SET enable_seqscan = off;
SET enable_bitmapscan = off;
SET pgcontext.hnsw_shared_serving = off;
SELECT id FROM public.mapped_fairness_${fairness_id}
 ORDER BY embedding OPERATOR(pgcontext.<->) '[1,0]'::vector LIMIT 1;
BEGIN;
DROP INDEX public.mapped_fairness_${fairness_id}_hnsw;
PREPARE TRANSACTION 'pgcontext_mapped_fairness_${fairness_id}';
SQL
done
psql_db <<'SQL'
CREATE TABLE public.mapped_fairness_target (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL
);
INSERT INTO public.mapped_fairness_target VALUES (1, '[1,0]'::vector);
CREATE INDEX mapped_fairness_target_hnsw
    ON public.mapped_fairness_target
 USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
SQL
publish_index_generation mapped_fairness_target
FAIRNESS_TARGET_OID="$(psql_db -Atc "SELECT 'mapped_fairness_target_hnsw'::regclass::oid::bigint" | tail -n 1)"
psql_db <<'SQL'
BEGIN;
DROP INDEX public.mapped_fairness_target_hnsw;
PREPARE TRANSACTION 'pgcontext_mapped_fairness_target';
COMMIT PREPARED 'pgcontext_mapped_fairness_target';
SQL
# Simulate the pre-bucket publisher crashing before rename. Every bucket gets
# a full reconciliation window of stale temp entries, so the committed target
# is reachable only if bounded cleanup makes durable progress across fresh
# backend connections.
for bucket in $(seq 0 15); do
    printf -v bucket_hex '%02x' "${bucket}"
    bucket_directory="${DATABASE_DIRECTORY}/pgcontext_hnsw_mapped/.pending_drops/${bucket_hex}"
    mkdir -p "${bucket_directory}"
    for temp_id in $(seq 1 16); do
        touch "${bucket_directory}/.pending_drop_0_0.tmp.${temp_id}"
    done
done
STALE_TEMP_XID="$(psql_db -Atc 'SELECT pg_catalog.txid_current()::bigint % 4294967296' | tail -n 1)"
CURRENT_TEMP_DIRECTORY="${DATABASE_DIRECTORY}/pgcontext_hnsw_mapped/.pending_drop_temps"
mkdir -p "${CURRENT_TEMP_DIRECTORY}"
for temp_id in $(seq 1 33); do
    touch "${CURRENT_TEMP_DIRECTORY}/.pending_drop_0_${STALE_TEMP_XID}.tmp.${temp_id}"
done
CURSOR_PATH="${DATABASE_DIRECTORY}/pgcontext_hnsw_mapped/.pending_drop_bucket_cursor"
CURSOR_BEFORE="$(<"${CURSOR_PATH}")"
reconcile_pids=()
for _concurrent_reconcile_scan in $(seq 1 17); do
    publish_index_generation mapped_prepared_sweeper_docs &
    reconcile_pids+=("$!")
done
for reconcile_pid in "${reconcile_pids[@]}"; do
    wait "${reconcile_pid}"
done
CURSOR_AFTER="$(<"${CURSOR_PATH}")"
EXPECTED_CURSOR="$(((CURSOR_BEFORE + 17) % 16))"
if [[ "${CURSOR_AFTER}" != "${EXPECTED_CURSOR}" ]]; then
    echo "expected concurrent cursor ${EXPECTED_CURSOR}, found ${CURSOR_AFTER}" >&2
    exit 1
fi
printf 'mapped_hnsw_concurrent_cursor_progressed\n'
for _reconcile_scan in $(seq 1 36); do
    publish_index_generation mapped_prepared_sweeper_docs
done
assert_generation_reclaimed "${FAIRNESS_TARGET_OID}"
assert_drop_marker_count "${FAIRNESS_TARGET_OID}" 0
if [[ -n "$(find "${CURRENT_TEMP_DIRECTORY}" -maxdepth 1 -type f -print -quit)" ]]; then
    echo "expected current-format stale publication temps to be reclaimed" >&2
    exit 1
fi
printf 'mapped_hnsw_current_temps_reclaimed\n'
printf 'mapped_hnsw_stale_temps_do_not_starve\n'
printf 'mapped_hnsw_reconcile_window_advanced\n'
for fairness_id in $(seq 1 33); do
    psql_db -c "ROLLBACK PREPARED 'pgcontext_mapped_fairness_${fairness_id}'" >/dev/null
done

TEMP_DROP_OID="$(psql_db -At <<'SQL' | sed -n 's/^TEMP_DROP_OID=//p' | tail -n 1
CREATE TEMP TABLE mapped_temp_drop_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL
);
INSERT INTO mapped_temp_drop_docs
SELECT value, format('[%s,0]', value)::vector
  FROM generate_series(1, 16) AS value;
CREATE INDEX mapped_temp_drop_docs_hnsw
    ON mapped_temp_drop_docs
 USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
SET enable_seqscan = off;
SET enable_bitmapscan = off;
SET pgcontext.hnsw_shared_serving = off;
SELECT id FROM mapped_temp_drop_docs
 ORDER BY embedding OPERATOR(pgcontext.<->) '[1,0]'::vector LIMIT 1;
SELECT 'TEMP_DROP_OID=' || 'mapped_temp_drop_docs_hnsw'::regclass::oid::text;
DROP INDEX mapped_temp_drop_docs_hnsw;
SQL
)"
assert_generation_reclaimed "${TEMP_DROP_OID}"
printf 'mapped_hnsw_temp_drop_reclaimed\n'

TEMP_TEARDOWN_OID="$(psql_db -At <<'SQL' | sed -n 's/^TEMP_TEARDOWN_OID=//p' | tail -n 1
CREATE TEMP TABLE mapped_temp_teardown_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL
);
INSERT INTO mapped_temp_teardown_docs
SELECT value, format('[%s,0]', value)::vector
  FROM generate_series(1, 16) AS value;
CREATE INDEX mapped_temp_teardown_docs_hnsw
    ON mapped_temp_teardown_docs
 USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
SET enable_seqscan = off;
SET enable_bitmapscan = off;
SET pgcontext.hnsw_shared_serving = off;
SELECT id FROM mapped_temp_teardown_docs
 ORDER BY embedding OPERATOR(pgcontext.<->) '[1,0]'::vector LIMIT 1;
SELECT 'TEMP_TEARDOWN_OID=' || 'mapped_temp_teardown_docs_hnsw'::regclass::oid::text;
SQL
)"
assert_generation_reclaimed "${TEMP_TEARDOWN_OID}"
printf 'mapped_hnsw_temp_teardown_reclaimed\n'

DROP_DATABASE_NAME="${DBNAME}_dropdb"
drop_database "${DROP_DATABASE_NAME}"
create_database "${DROP_DATABASE_NAME}"
psql -h "${PGHOST}" -p "${PGPORT}" -d "${DROP_DATABASE_NAME}" -v ON_ERROR_STOP=1 <<'SQL'
CREATE EXTENSION pgcontext;
CREATE TABLE public.mapped_drop_database_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL
);
INSERT INTO public.mapped_drop_database_docs
SELECT value, format('[%s,0]', value)::vector
  FROM generate_series(1, 16) AS value;
CREATE INDEX mapped_drop_database_docs_hnsw
    ON public.mapped_drop_database_docs
 USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
SET enable_seqscan = off;
SET enable_bitmapscan = off;
SET pgcontext.hnsw_shared_serving = off;
SELECT id FROM public.mapped_drop_database_docs
 ORDER BY embedding OPERATOR(pgcontext.<->) '[1,0]'::vector LIMIT 1;
SQL
DROP_DATABASE_DATA_DIRECTORY="$(psql -h "${PGHOST}" -p "${PGPORT}" -d "${DROP_DATABASE_NAME}" -At -v ON_ERROR_STOP=1 -c 'SHOW data_directory' | tail -n 1)"
DROP_DATABASE_RELATIVE_DIRECTORY="$(psql -h "${PGHOST}" -p "${PGPORT}" -d "${DROP_DATABASE_NAME}" -At -v ON_ERROR_STOP=1 -c "SELECT pg_catalog.pg_relation_filepath('pg_catalog.pg_class'::regclass)" | tail -n 1)"
DROP_DATABASE_MAPPED_DIRECTORY="${DROP_DATABASE_DATA_DIRECTORY}/$(dirname "${DROP_DATABASE_RELATIVE_DIRECTORY}")/pgcontext_hnsw_mapped"
if [[ ! -d "${DROP_DATABASE_MAPPED_DIRECTORY}" ]]; then
    echo "expected mapped database directory before DROP DATABASE" >&2
    exit 1
fi
drop_database "${DROP_DATABASE_NAME}"
if [[ -e "${DROP_DATABASE_MAPPED_DIRECTORY}" ]]; then
    echo "expected DROP DATABASE to reclaim ${DROP_DATABASE_MAPPED_DIRECTORY}" >&2
    exit 1
fi
printf 'mapped_hnsw_drop_database_reclaimed\n'

printf 'mapped HNSW lifecycle cleanup gate passed\n'
