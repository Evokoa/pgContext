#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_physical_backup_wal_replay}"
BACKUP_PORT="${BACKUP_PORT:-28917}"
BACKUP_HOST="${BACKUP_HOST:-127.0.0.1}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

require_simple_identifier "${DBNAME}" "DBNAME"

PG_BASEBACKUP="$(pg_bin pg_basebackup)"
PG_CTL="$(pg_bin pg_ctl)"
BACKUP_DIR="${HEAVY_TMPDIR}/${DBNAME}_basebackup"
BACKUP_SOCKET_DIR="/tmp/pgctx_${DBNAME}_sock"
BACKUP_LOG="${HEAVY_TMPDIR}/${DBNAME}_backup_cluster.log"

backup_started=0
cleanup() {
    local status=$?
    if [[ "${backup_started}" -eq 1 ]]; then
        "${PG_CTL}" -D "${BACKUP_DIR}" -m immediate -w stop >/dev/null 2>&1 || true
    fi
    if [[ "${status}" -eq 0 ]]; then
        rm -rf "${BACKUP_DIR}" "${BACKUP_SOCKET_DIR}" "${BACKUP_LOG}"
    else
        echo "preserved failed backup cluster artifacts under ${BACKUP_DIR}" >&2
        echo "preserved backup cluster log at ${BACKUP_LOG}" >&2
    fi
}
trap cleanup EXIT

psql_backup() {
    psql -h "${BACKUP_HOST}" -p "${BACKUP_PORT}" -d "${DBNAME}" -v ON_ERROR_STOP=1 "$@"
}

start_backup_cluster() {
    mkdir -p "${BACKUP_SOCKET_DIR}"
    "${PG_CTL}" \
        -D "${BACKUP_DIR}" \
        -l "${BACKUP_LOG}" \
        -o "-p ${BACKUP_PORT} -h ${BACKUP_HOST} -k ${BACKUP_SOCKET_DIR}" \
        -w start
    backup_started=1
}

stop_backup_cluster_immediate() {
    "${PG_CTL}" -D "${BACKUP_DIR}" -m immediate -w stop
    backup_started=0
}

validate_backup_state() {
    local expected_exact="$1"
    local expected_indexed="$2"
    local expected_count="$3"
    local phase="$4"

    psql_backup <<SQL
DO \$\$
DECLARE
    exact_source_key text;
    indexed_source_key text;
    exact_count bigint;
    indexed_count bigint;
BEGIN
    SELECT source_key
      INTO exact_source_key
      FROM pgcontext.search('wal_exact_docs', '[${expected_exact},0]'::vector, 1);
    IF exact_source_key IS DISTINCT FROM '${expected_exact}' THEN
        RAISE EXCEPTION 'unexpected exact collection nearest source key: %', exact_source_key;
    END IF;
    RAISE NOTICE 'physical_backup_exact_nearest_verified: ${phase}';

    SELECT source_key
      INTO indexed_source_key
      FROM pgcontext.search('wal_indexed_docs', '[${expected_indexed},0]'::vector, 1);
    IF indexed_source_key IS DISTINCT FROM '${expected_indexed}' THEN
        RAISE EXCEPTION 'unexpected indexed collection nearest source key: %', indexed_source_key;
    END IF;
    RAISE NOTICE 'physical_backup_indexed_nearest_verified: ${phase}';

    SELECT count(*) INTO exact_count FROM pgcontext.scroll('wal_exact_docs', NULL, 20);
    IF exact_count IS DISTINCT FROM ${expected_count} THEN
        RAISE EXCEPTION 'unexpected exact collection point count after physical backup/replay: %', exact_count;
    END IF;
    RAISE NOTICE 'physical_backup_exact_scroll_verified: ${phase}';

    SELECT count(*) INTO indexed_count FROM pgcontext.scroll('wal_indexed_docs', NULL, 20);
    IF indexed_count IS DISTINCT FROM ${expected_count} THEN
        RAISE EXCEPTION 'unexpected indexed collection point count after physical backup/replay: %', indexed_count;
    END IF;
    RAISE NOTICE 'physical_backup_indexed_scroll_verified: ${phase}';

    PERFORM 1 FROM pgcontext.index_status('public.wal_indexed_docs_embedding_idx')
     WHERE access_method = 'pgcontext_hnsw'
       AND status::text = 'Ready';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'HNSW index was not ready after physical backup/replay';
    END IF;
    RAISE NOTICE 'physical_backup_hnsw_ready: ${phase}';
END
\$\$;
SQL
}

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.wal_exact_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    body text NOT NULL
);

CREATE TABLE public.wal_indexed_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    body text NOT NULL
);

INSERT INTO public.wal_exact_docs (id, embedding, body)
VALUES (1, '[1,0]'::vector, 'exact before backup'), (2, '[2,0]'::vector, 'exact neighbor');

INSERT INTO public.wal_indexed_docs (id, embedding, body)
VALUES (1, '[1,0]'::vector, 'indexed before backup'), (2, '[2,0]'::vector, 'indexed neighbor');

SELECT * FROM pgcontext.create_collection('wal_exact_docs', 'public.wal_exact_docs');
SELECT * FROM pgcontext.register_vector('wal_exact_docs', 'embedding', 'embedding', 2, 'l2');
SELECT * FROM pgcontext.upsert_points('wal_exact_docs', ARRAY['1', '2']);

SELECT * FROM pgcontext.create_collection('wal_indexed_docs', 'public.wal_indexed_docs');
SELECT * FROM pgcontext.register_vector('wal_indexed_docs', 'embedding', 'embedding', 2, 'l2');
SELECT * FROM pgcontext.upsert_points('wal_indexed_docs', ARRAY['1', '2']);
CREATE INDEX wal_indexed_docs_embedding_idx ON public.wal_indexed_docs USING pgcontext_hnsw (embedding);

CHECKPOINT;
SQL

rm -rf "${BACKUP_DIR}"
"${PG_BASEBACKUP}" -h "${PGHOST}" -p "${PGPORT}" -D "${BACKUP_DIR}" -X stream --checkpoint=fast
printf 'physical_backup_basebackup_created\n'

start_backup_cluster
validate_backup_state "1" "1" "2" "before_replay"

psql_backup <<'SQL'
INSERT INTO public.wal_exact_docs (id, embedding, body)
VALUES (9, '[9,0]'::vector, 'exact after backup');
INSERT INTO public.wal_indexed_docs (id, embedding, body)
VALUES (9, '[9,0]'::vector, 'indexed after backup');
SELECT * FROM pgcontext.upsert_points('wal_exact_docs', ARRAY['9']);
SELECT * FROM pgcontext.upsert_points('wal_indexed_docs', ARRAY['9']);
SQL

stop_backup_cluster_immediate
start_backup_cluster
printf 'physical_backup_restarted_after_writes\n'
validate_backup_state "9" "9" "3" "after_replay"
