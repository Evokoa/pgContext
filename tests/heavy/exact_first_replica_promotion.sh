#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_exact_first_replica}"
REPLICA_PORT="${REPLICA_PORT:-28928}"
PRIMARY_PORT="${PRIMARY_PORT:-28918}"
REPLICA_HOST="${REPLICA_HOST:-127.0.0.1}"
REPLICA_USER="${REPLICA_USER:-pgcontext_exact_first_repl}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

require_simple_identifier "${DBNAME}" "DBNAME"
require_simple_identifier "${REPLICA_USER}" "REPLICA_USER"

PG_CTL="$(pg_bin pg_ctl)"
PG_BASEBACKUP="$(pg_bin pg_basebackup)"
INITDB="$(pg_bin initdb)"
PRIMARY_DIR="${HEAVY_TMPDIR}/${DBNAME}_primary"
PRIMARY_SOCKET="/private/tmp/pgctx_exact_first_primary_${PRIMARY_PORT}"
PRIMARY_LOG="${HEAVY_TMPDIR}/${DBNAME}_primary.log"
REPLICA_DIR="${HEAVY_TMPDIR}/${DBNAME}_replica"
REPLICA_SOCKET="/private/tmp/pgctx_exact_first_replica_${REPLICA_PORT}"
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
        echo "preserved exact-first replica artifacts: ${REPLICA_DIR} ${REPLICA_LOG}" >&2
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
CREATE TABLE public.p14_replica_docs (
    id bigint PRIMARY KEY,
    embedding pgcontext.vector(2) NOT NULL
);
INSERT INTO public.p14_replica_docs
SELECT id, format('[%s,%s]', id % 1000, id / 1000)::pgcontext.vector
  FROM generate_series(1, 20000) AS id;
ANALYZE public.p14_replica_docs;
SELECT pgcontext.create_collection('p14_replica', 'public.p14_replica_docs');
SELECT * FROM pgcontext.register_exact_first(
    'p14_replica', 'public.p14_replica_docs',
    jsonb_build_object(
        'version', 'exact_first_registration_v1',
        'key_column', 'id',
        'bindings', jsonb_build_array(jsonb_build_object(
            'name', 'embedding', 'column', 'embedding', 'kind', 'dense',
            'dimensions', 2, 'metric', 'l2'
        ))
    )
);
CREATE TABLE public.p14_replica_plan AS
SELECT * FROM pgcontext.exact_first_advisor(
    'p14_replica',
    jsonb_build_object(
        'version', 'exact_first_advisor_v1',
        'memory_budget_bytes', 1073741824,
        'build_window_seconds', 3600,
        'update_millihertz', 0,
        'filter_selectivity_bps', 10000
    )
);
SELECT * FROM pgcontext.apply_exact_first_plan(
    'p14_replica', (SELECT plan_revision FROM public.p14_replica_plan), 'enqueue'
);
CREATE TABLE public.p14_replica_claim AS
SELECT * FROM pgcontext.claim_exact_first_build('p14_replica', 'replica-builder', 60000);
SQL

DDL="$(psql_db -Atc 'SELECT generated_ddl FROM public.p14_replica_claim')"
PLAN_REVISION="$(psql_db -Atc 'SELECT plan_revision FROM public.p14_replica_claim')"
LEASE_TOKEN="$(psql_db -Atc 'SELECT lease_token FROM public.p14_replica_claim')"
[[ "${DDL}" == CREATE\ INDEX\ CONCURRENTLY* ]]
PGOPTIONS="${PGOPTIONS:-} -c search_path=public,pgcontext" \
    psql -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" -v ON_ERROR_STOP=1 -c "${DDL}"
psql_db -v plan_revision="${PLAN_REVISION}" -v lease_token="${LEASE_TOKEN}" <<'SQL'
SELECT * FROM pgcontext.publish_exact_first_build(
    'p14_replica', :'plan_revision'::bigint, :'lease_token'::bigint
);
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
UPDATE public.p14_replica_docs SET embedding = '[0,0]' WHERE id = 7;
DELETE FROM public.p14_replica_docs WHERE id = 8;
INSERT INTO public.p14_replica_docs VALUES (20001, '[1,1]');
SQL

for _ in {1..30}; do
    if psql_replica -Atc "SELECT count(*) FROM public.p14_replica_docs WHERE id = 20001" | grep -qx '1'; then
        break
    fi
    sleep 1
done
psql_replica -Atc "SELECT count(*) FROM public.p14_replica_docs WHERE id = 20001" | grep -qx '1'

"${PG_CTL}" -D "${REPLICA_DIR}" promote -w
psql_replica <<'SQL'
DO $p14_replica$
DECLARE
    exact_nearest text;
    indexed_nearest bigint;
    state text;
BEGIN
    SELECT source_key INTO exact_nearest
      FROM pgcontext.exact_first_search(
          'p14_replica', 'embedding', '[0,0]'::pgcontext.vector, 1
      );
    SELECT readiness_state INTO state
      FROM pgcontext.exact_first_readiness('p14_replica');
    SET LOCAL enable_seqscan = off;
    SELECT id INTO indexed_nearest
      FROM public.p14_replica_docs
     ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector, id
     LIMIT 1;
    IF exact_nearest IS DISTINCT FROM '7' OR indexed_nearest IS DISTINCT FROM 7
       OR state IS DISTINCT FROM 'indexed' THEN
        RAISE EXCEPTION 'promoted exact-first state/result mismatch: %, %, %',
            exact_nearest, indexed_nearest, state;
    END IF;
    IF EXISTS (SELECT 1 FROM public.p14_replica_docs WHERE id = 8) THEN
        RAISE EXCEPTION 'deleted row remained on promoted exact-first replica';
    END IF;
END
$p14_replica$;
SQL

printf 'exact_first_physical_replica_catchup_verified\n'
printf 'exact_first_standby_promotion_verified\n'
