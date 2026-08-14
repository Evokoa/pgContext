#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_ivfflat_replica}"
REPLICA_PORT="${REPLICA_PORT:-28927}"
PRIMARY_PORT="${PRIMARY_PORT:-28917}"
REPLICA_HOST="${REPLICA_HOST:-127.0.0.1}"
REPLICA_USER="${REPLICA_USER:-pgcontext_ivf_repl}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

require_simple_identifier "${DBNAME}" "DBNAME"
require_simple_identifier "${REPLICA_USER}" "REPLICA_USER"

PG_CTL="$(pg_bin pg_ctl)"
PG_BASEBACKUP="$(pg_bin pg_basebackup)"
INITDB="$(pg_bin initdb)"
PRIMARY_DIR="${HEAVY_TMPDIR}/${DBNAME}_primary"
SOCKET_ROOT="${HEAVY_SOCKET_ROOT:-/tmp}"
PRIMARY_SOCKET="${SOCKET_ROOT}/pgctx_ivf_primary_${PRIMARY_PORT}.sock"
PRIMARY_LOG="${HEAVY_TMPDIR}/${DBNAME}_primary.log"
REPLICA_DIR="${HEAVY_TMPDIR}/${DBNAME}_replica"
REPLICA_SOCKET="${SOCKET_ROOT}/pgctx_ivf_replica_${REPLICA_PORT}.sock"
REPLICA_LOG="${HEAVY_TMPDIR}/${DBNAME}_replica.log"
REPLICA_SLOT="${DBNAME}_slot"
primary_started=0
replica_started=0

cleanup() {
    local status=$?
    if [[ "${replica_started}" -eq 1 ]]; then
        "${PG_CTL}" -D "${REPLICA_DIR}" -m immediate -w stop >/dev/null 2>&1 || true
    fi
    if [[ "${primary_started}" -eq 1 ]]; then
        psql_postgres -Atc "SELECT pg_drop_replication_slot(slot_name) FROM pg_replication_slots WHERE slot_name = '${REPLICA_SLOT}'" >/dev/null 2>&1 || true
        psql_postgres -c "DROP ROLE IF EXISTS ${REPLICA_USER}" >/dev/null 2>&1 || true
        "${PG_CTL}" -D "${PRIMARY_DIR}" -m fast -w stop >/dev/null 2>&1 || true
    fi
    if [[ "${status}" -eq 0 ]]; then
        rm -rf "${PRIMARY_DIR}" "${PRIMARY_SOCKET}" "${REPLICA_DIR}" "${REPLICA_SOCKET}"
        rm -f "${PRIMARY_LOG}" "${REPLICA_LOG}"
    else
        echo "preserved replica artifacts: ${REPLICA_DIR} ${REPLICA_LOG}" >&2
    fi
}
trap cleanup EXIT

psql_replica() {
    psql -h "${REPLICA_HOST}" -p "${REPLICA_PORT}" -d "${DBNAME}" -v ON_ERROR_STOP=1 "$@"
}

if [[ "${PRIMARY_DIR}" != "${HEAVY_TMPDIR}/"* || "${PRIMARY_DIR}" == "${HEAVY_TMPDIR}" ]]; then
    echo "refusing unsafe primary cleanup target: ${PRIMARY_DIR}" >&2
    exit 2
fi
rm -rf "${PRIMARY_DIR}" "${PRIMARY_SOCKET}"
rm -f "${PRIMARY_LOG}"
cargo pgrx install -p context-pg --no-default-features --features "${PG_FEATURE}" --pg-config "${PG_CONFIG}"
"${INITDB}" -D "${PRIMARY_DIR}" --auth=trust --no-locale
mkdir -p "${PRIMARY_SOCKET}"
"${PG_CTL}" -D "${PRIMARY_DIR}" -l "${PRIMARY_LOG}" \
    -o "-p ${PRIMARY_PORT} -h ${PGHOST} -k ${PRIMARY_SOCKET}" -w start
primary_started=1
PGPORT="${PRIMARY_PORT}"
reset_database

psql_postgres -c "CREATE ROLE ${REPLICA_USER} REPLICATION LOGIN"

psql_db <<'SQL'
CREATE EXTENSION pgcontext;
CREATE TABLE public.ivf_replica_docs (
    id bigint PRIMARY KEY,
    embedding pgcontext.vector(4) NOT NULL
);
INSERT INTO public.ivf_replica_docs
SELECT id, format('[%s,%s,%s,%s]', id%31, id%29, id%23, id%19)::pgcontext.vector
  FROM generate_series(1, 20000) id;
SET maintenance_work_mem = '2MB';
SET pgcontext.ivfflat_build_parallel_workers = 4;
CREATE INDEX ivf_replica_docs_idx
    ON public.ivf_replica_docs USING pgcontext_ivfflat
       (embedding pgcontext.vector_ivfflat_ops)
       WITH (lists = 64, quantization = pq, pq_subvector_dimensions = 2);
CHECKPOINT;
SQL

if [[ "${REPLICA_DIR}" != "${HEAVY_TMPDIR}/"* || "${REPLICA_DIR}" == "${HEAVY_TMPDIR}" ]]; then
    echo "refusing unsafe replica cleanup target: ${REPLICA_DIR}" >&2
    exit 2
fi
rm -rf "${REPLICA_DIR}"
"${PG_BASEBACKUP}" -h "${PGHOST}" -p "${PGPORT}" -U "${REPLICA_USER}" \
    -D "${REPLICA_DIR}" -R -X stream -C -S "${REPLICA_SLOT}"
mkdir -p "${REPLICA_SOCKET}"
"${PG_CTL}" -D "${REPLICA_DIR}" -l "${REPLICA_LOG}" \
    -o "-p ${REPLICA_PORT} -h ${REPLICA_HOST} -k ${REPLICA_SOCKET}" -w start
replica_started=1

psql_db <<'SQL'
UPDATE public.ivf_replica_docs SET embedding = '[0,0,0,0]' WHERE id = 7;
DELETE FROM public.ivf_replica_docs WHERE id = 8;
INSERT INTO public.ivf_replica_docs VALUES (20001, '[1,1,1,1]');
SQL

for _ in {1..30}; do
    if psql_replica -Atc "SELECT count(*) FROM public.ivf_replica_docs WHERE id = 20001" | grep -qx '1'; then
        break
    fi
    sleep 1
done
psql_replica -Atc "SELECT count(*) FROM public.ivf_replica_docs WHERE id = 20001" | grep -qx '1'

"${PG_CTL}" -D "${REPLICA_DIR}" promote -w
psql_replica <<'SQL'
SET enable_seqscan = off;
SET enable_bitmapscan = off;
SET pgcontext.ivfflat_iterative_scan = strict_order;
SET pgcontext.ivfflat_max_probes = 64;
DO $$
DECLARE
    nearest bigint;
BEGIN
    IF (pgcontext.ivfflat_index_info('public.ivf_replica_docs_idx'::regclass)->>'verified')::boolean IS NOT TRUE THEN
        RAISE EXCEPTION 'promoted IVFFlat index failed verification';
    END IF;
    SELECT id INTO nearest
      FROM public.ivf_replica_docs
     ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0,0,0]'::pgcontext.vector
     LIMIT 1;
    IF nearest <> 7 THEN RAISE EXCEPTION 'unexpected promoted nearest row: %', nearest; END IF;
    IF EXISTS (SELECT 1 FROM public.ivf_replica_docs WHERE id = 8) THEN
        RAISE EXCEPTION 'deleted row remained on promoted replica';
    END IF;
END
$$;
SQL

printf 'ivfflat_physical_replica_catchup_verified\n'
printf 'ivfflat_standby_promotion_verified\n'
