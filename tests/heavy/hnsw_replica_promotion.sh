#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_hnsw_replica_promotion}"
REPLICA_PORT="${REPLICA_PORT:-28917}"
REPLICA_HOST="${REPLICA_HOST:-127.0.0.1}"
REPLICA_USER="${REPLICA_USER:-pgcontext_hnsw_repl}"
PGRX_DATA_DIR="${PGRX_DATA_DIR:?set PGRX_DATA_DIR to the local pgrx primary data directory}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

require_simple_identifier "${DBNAME}" "DBNAME"
require_simple_identifier "${REPLICA_USER}" "REPLICA_USER"

PG_CTL="$(pg_bin pg_ctl)"
PG_BASEBACKUP="$(pg_bin pg_basebackup)"
REPLICA_DIR="${HEAVY_TMPDIR}/${DBNAME}_replica"
REPLICA_SOCKET="/tmp/pgctx_${DBNAME}_replica_sock"
REPLICA_LOG="${HEAVY_TMPDIR}/${DBNAME}_replica.log"
REPLICA_SLOT="${DBNAME}_slot"
HBA_MARKER="# pgcontext-hnsw-replica-${DBNAME}"
replica_started=0

cleanup() {
    local status=$?
    if [[ "${replica_started}" -eq 1 ]]; then
        "${PG_CTL}" -D "${REPLICA_DIR}" -m immediate -w stop >/dev/null 2>&1 || true
    fi
    psql_postgres -Atc "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name = '${REPLICA_SLOT}'" >/dev/null 2>&1 || true
    psql_postgres -c "DROP ROLE IF EXISTS ${REPLICA_USER}" >/dev/null 2>&1 || true
    /usr/bin/sed -i '' "/^${HBA_MARKER}$/,+1d" "${PGRX_DATA_DIR}/pg_hba.conf" >/dev/null 2>&1 || true
    "${PG_CTL}" -D "${PGRX_DATA_DIR}" reload >/dev/null 2>&1 || true
    if [[ "${status}" -eq 0 ]]; then
        rm -rf "${REPLICA_DIR}" "${REPLICA_SOCKET}" "${REPLICA_LOG}"
    else
        echo "preserved replica artifacts: ${REPLICA_DIR} ${REPLICA_LOG}" >&2
    fi
}
trap cleanup EXIT

psql_replica() {
    psql -h "${REPLICA_HOST}" -p "${REPLICA_PORT}" -d "${DBNAME}" -v ON_ERROR_STOP=1 "$@"
}

start_replica() {
    mkdir -p "${REPLICA_SOCKET}"
    "${PG_CTL}" -D "${REPLICA_DIR}" -l "${REPLICA_LOG}" \
        -o "-p ${REPLICA_PORT} -h ${REPLICA_HOST} -k ${REPLICA_SOCKET}" -w start
    replica_started=1
}

validate_promoted_oracle() {
    local indexed exact
    exact="$(psql_replica -At <<'SQL' | tail -n 1
SET enable_indexscan = off;
SELECT string_agg(id::text, ',' ORDER BY embedding OPERATOR(pgcontext.<->) '[9,0]'::vector)
  FROM (SELECT id, embedding FROM public.hnsw_replica_docs ORDER BY embedding OPERATOR(pgcontext.<->) '[9,0]'::vector LIMIT 3) AS ranked;
SQL
)"
    indexed="$(psql_replica -At <<'SQL' | tail -n 1
SET enable_seqscan = off;
SELECT string_agg(id::text, ',' ORDER BY embedding OPERATOR(pgcontext.<->) '[9,0]'::vector)
  FROM (SELECT id, embedding FROM public.hnsw_replica_docs ORDER BY embedding OPERATOR(pgcontext.<->) '[9,0]'::vector LIMIT 3) AS ranked;
SQL
)"
    if [[ "${indexed}" != "${exact}" ]]; then
        echo "promoted HNSW oracle mismatch: indexed=${indexed}, exact=${exact}" >&2
        exit 1
    fi
    printf 'hnsw_replica_promotion_oracle: passed\n'
}

start_and_install_extension
reset_database

printf '%s\nhost replication %s 127.0.0.1/32 trust\n' "${HBA_MARKER}" "${REPLICA_USER}" >>"${PGRX_DATA_DIR}/pg_hba.conf"
"${PG_CTL}" -D "${PGRX_DATA_DIR}" reload
psql_postgres -c "CREATE ROLE ${REPLICA_USER} REPLICATION LOGIN"

psql_db <<'SQL'
CREATE EXTENSION pgcontext;
CREATE TABLE public.hnsw_replica_docs (id bigint PRIMARY KEY, embedding vector NOT NULL);
INSERT INTO public.hnsw_replica_docs VALUES
  (1, '[1,0]'::vector), (2, '[2,0]'::vector), (9, '[9,0]'::vector);
CREATE INDEX hnsw_replica_docs_embedding_idx ON public.hnsw_replica_docs USING pgcontext_hnsw (embedding);
CHECKPOINT;
SQL

rm -rf "${REPLICA_DIR}"
"${PG_BASEBACKUP}" -h "${PGHOST}" -p "${PGPORT}" -U "${REPLICA_USER}" \
    -D "${REPLICA_DIR}" -R -X stream -C -S "${REPLICA_SLOT}"
start_replica

psql_db -c "INSERT INTO public.hnsw_replica_docs VALUES (10, '[10,0]'::vector)"
for _ in {1..30}; do
    if psql_replica -Atc "SELECT count(*) FROM public.hnsw_replica_docs" | grep -qx '4'; then break; fi
    sleep 1
done
psql_replica -Atc "SELECT count(*) FROM public.hnsw_replica_docs" | grep -qx '4'

"${PG_CTL}" -D "${REPLICA_DIR}" promote -w
validate_promoted_oracle
