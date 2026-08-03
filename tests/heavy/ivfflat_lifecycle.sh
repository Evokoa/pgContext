#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_ivfflat_lifecycle}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

start_and_install_extension
reset_database
psql_postgres <<'SQL'
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = 'pgcontext_ivf_reader_v4'
    ) THEN
        CREATE ROLE pgcontext_ivf_reader_v4;
    END IF;
END
$$;
SQL

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.ivf_docs (
    id bigint PRIMARY KEY,
    tenant text NOT NULL,
    embedding pgcontext.vector(4) NOT NULL
);
INSERT INTO public.ivf_docs
SELECT id,
       CASE WHEN id % 2 = 0 THEN 'even' ELSE 'odd' END,
       format('[%s,%s,%s,%s]', id%31, id%29, id%23, id%19)::pgcontext.vector
  FROM generate_series(1, 10000) id;

SET maintenance_work_mem = '1MB';
SET pgcontext.ivfflat_build_parallel_workers = 4;
CREATE INDEX ivf_docs_embedding_idx
    ON public.ivf_docs USING pgcontext_ivfflat
       (embedding pgcontext.vector_ivfflat_ops)
       WITH (lists = 32, quantization = sq8);

DO $$
BEGIN
    IF (pgcontext.ivfflat_index_info('public.ivf_docs_embedding_idx'::regclass)->>'verified')::boolean IS NOT TRUE THEN
        RAISE EXCEPTION 'initial IVFFlat verification failed';
    END IF;
END
$$;

UPDATE public.ivf_docs SET tenant = 'hot' WHERE id = 7;
UPDATE public.ivf_docs SET embedding = '[0,0,0,0]' WHERE id = 8;
DELETE FROM public.ivf_docs WHERE id IN (9, 10, 11);
INSERT INTO public.ivf_docs VALUES (20001, 'new', '[1,1,1,1]');
VACUUM (ANALYZE) public.ivf_docs;

SET enable_seqscan = off;
SET enable_bitmapscan = off;
SET pgcontext.ivfflat_iterative_scan = strict_order;
SET pgcontext.ivfflat_max_probes = 32;

DO $$
DECLARE
    nearest bigint;
    deleted bigint;
    deltas bigint;
BEGIN
    SELECT id INTO nearest
      FROM public.ivf_docs
     ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0,0,0]'::pgcontext.vector
     LIMIT 1;
    IF nearest <> 8 THEN RAISE EXCEPTION 'unexpected post-DML nearest row: %', nearest; END IF;
    SELECT count(*) INTO deleted FROM public.ivf_docs WHERE id IN (9,10,11);
    IF deleted <> 0 THEN RAISE EXCEPTION 'deleted IVFFlat rows remain visible'; END IF;
    SELECT (pgcontext.ivfflat_index_info('public.ivf_docs_embedding_idx'::regclass)->>'delta_records')::bigint
      INTO deltas;
    IF deltas < 5 THEN RAISE EXCEPTION 'expected update/delete/insert delta records, got %', deltas; END IF;
END
$$;

CREATE TABLE public.ivf_delta_widen (
    id integer PRIMARY KEY,
    tenant integer NOT NULL,
    embedding pgcontext.vector(2) NOT NULL
);
INSERT INTO public.ivf_delta_widen VALUES
    (1, 1, '[0,0]'),
    (2, 1, '[0.1,0]'),
    (3, 2, '[10,10]'),
    (4, 2, '[10.1,10]');
CREATE INDEX ivf_delta_widen_idx
    ON public.ivf_delta_widen USING pgcontext_ivfflat
       (embedding pgcontext.vector_ivfflat_ops)
       WITH (lists = 2);
DELETE FROM public.ivf_delta_widen WHERE id = 1;
INSERT INTO public.ivf_delta_widen VALUES (5, 2, '[0.05,0]');
VACUUM public.ivf_delta_widen;

SET pgcontext.ivfflat_probes = 1;
SET pgcontext.ivfflat_max_probes = 2;
SET pgcontext.ivfflat_candidate_budget = 6;
SET pgcontext.ivfflat_iterative_scan = relaxed_order;
DO $$
DECLARE
    ids integer[];
    visited bigint;
    deltas bigint;
    rounds bigint;
BEGIN
    SELECT array_agg(id)
      INTO ids
      FROM (
            SELECT id
              FROM public.ivf_delta_widen
             WHERE tenant = 2
             ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector
             LIMIT 3
      ) nearest;
    IF ids <> ARRAY[5,3,4] THEN
        RAISE EXCEPTION 'unexpected delta-widening rows: %', ids;
    END IF;
    IF 1 = ANY(ids) OR cardinality(ids) <> cardinality(ARRAY(SELECT DISTINCT unnest(ids))) THEN
        RAISE EXCEPTION 'delta widening returned a tombstone or duplicate: %', ids;
    END IF;
    SELECT visited_postings, delta_records, widening_rounds
      INTO visited, deltas, rounds
      FROM pgcontext.ivfflat_last_scan_work();
    IF visited <> 4 OR deltas <> 2 OR rounds <> 1 OR visited + deltas <> 6 THEN
        RAISE EXCEPTION
            'unexpected delta widening work: postings %, deltas %, rounds %',
            visited, deltas, rounds;
    END IF;
END
$$;
RESET pgcontext.ivfflat_probes;
RESET pgcontext.ivfflat_max_probes;
RESET pgcontext.ivfflat_candidate_budget;
RESET pgcontext.ivfflat_iterative_scan;

REINDEX INDEX public.ivf_docs_embedding_idx;
DO $$
BEGIN
    IF (pgcontext.ivfflat_index_info('public.ivf_docs_embedding_idx'::regclass)->>'delta_records')::bigint <> 0 THEN
        RAISE EXCEPTION 'REINDEX did not fold IVFFlat deltas';
    END IF;
END
$$;

CREATE TABLE public.ivf_cic (
    id bigint PRIMARY KEY,
    tenant text NOT NULL,
    embedding pgcontext.vector(4) NOT NULL
);
INSERT INTO public.ivf_cic SELECT * FROM public.ivf_docs;
CREATE INDEX CONCURRENTLY ivf_cic_embedding_idx
    ON public.ivf_cic USING pgcontext_ivfflat
       (embedding pgcontext.vector_ivfflat_ops)
       WITH (lists = 32, quantization = pq, pq_subvector_dimensions = 2);

CREATE TABLE public.ivf_partitioned (
    id bigint,
    embedding pgcontext.vector(4) NOT NULL
) PARTITION BY RANGE (id);
CREATE TABLE public.ivf_partitioned_a PARTITION OF public.ivf_partitioned FOR VALUES FROM (0) TO (100);
CREATE TABLE public.ivf_partitioned_b PARTITION OF public.ivf_partitioned FOR VALUES FROM (100) TO (200);
INSERT INTO public.ivf_partitioned VALUES (1, '[1,0,0,0]'), (101, '[0,1,0,0]');
CREATE INDEX ivf_partitioned_embedding_idx
    ON public.ivf_partitioned USING pgcontext_ivfflat
       (embedding pgcontext.vector_ivfflat_ops)
       WITH (lists = 2, quantization = sq8);

GRANT USAGE ON SCHEMA public, pgcontext TO pgcontext_ivf_reader_v4;
GRANT SELECT ON public.ivf_docs TO pgcontext_ivf_reader_v4;
ALTER TABLE public.ivf_docs ENABLE ROW LEVEL SECURITY;
CREATE POLICY ivf_docs_tenant ON public.ivf_docs
    FOR SELECT TO pgcontext_ivf_reader_v4
    USING (tenant = current_setting('app.tenant', true));

SET ROLE pgcontext_ivf_reader_v4;
SET app.tenant = 'even';
SET enable_seqscan = off;
SET enable_bitmapscan = off;
SET pgcontext.ivfflat_iterative_scan = strict_order;
DO $$
DECLARE
    forbidden bigint;
BEGIN
    SELECT count(*) INTO forbidden
      FROM (
            SELECT tenant
              FROM public.ivf_docs
             ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0,0,0]'::pgcontext.vector
             LIMIT 100
      ) visible
     WHERE tenant <> 'even';
    IF forbidden <> 0 THEN
        RAISE EXCEPTION 'IVFFlat surfaced % rows rejected by RLS', forbidden;
    END IF;
END
$$;
RESET ROLE;
SQL

# Hold the heap lock in one session, then start compaction while that session
# is paused before its index write. Heap-before-index lock ordering lets the
# DML finish; index-before-heap creates a deterministic deadlock cycle.
dml_log="${HEAVY_TMPDIR}/${DBNAME}_ivfflat_dml_lock.log"
compact_log="${HEAVY_TMPDIR}/${DBNAME}_ivfflat_compact_lock.log"
PGOPTIONS="${PGOPTIONS:-} -c search_path=public,pgcontext" \
    psql -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" -v ON_ERROR_STOP=1 \
    >"${dml_log}" 2>&1 <<'SQL' &
BEGIN;
LOCK TABLE public.ivf_docs IN ROW EXCLUSIVE MODE;
SELECT pg_catalog.pg_sleep(1.5);
UPDATE public.ivf_docs SET embedding = '[2,2,2,2]' WHERE id = 12;
COMMIT;
SQL
dml_pid=$!

observed_heap_holder=false
for _ in $(seq 1 50); do
    if psql_db -Atc "SELECT count(*)
                       FROM pg_catalog.pg_stat_activity
                      WHERE datname = current_database()
                        AND query LIKE '%pg_catalog.pg_sleep(1.5)%'
                        AND wait_event = 'PgSleep'" | grep -qx '1'; then
        observed_heap_holder=true
        break
    fi
    sleep 0.1
done
if [[ "${observed_heap_holder}" != "true" ]]; then
    echo "did not observe the concurrent IVFFlat DML heap-lock fixture" >&2
    cat "${dml_log}" >&2
    wait "${dml_pid}" || true
    exit 1
fi

PGOPTIONS="${PGOPTIONS:-} -c search_path=public,pgcontext -c statement_timeout=10s -c deadlock_timeout=200ms" \
    psql -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" -v ON_ERROR_STOP=1 \
    -Atc "SELECT pgcontext.compact_ivfflat('public.ivf_docs_embedding_idx'::regclass)->>'generation'" \
    >"${compact_log}" 2>&1
wait "${dml_pid}"

if ! grep -Eq '^[0-9]+$' "${compact_log}"; then
    echo "concurrent IVFFlat compaction did not publish a generation" >&2
    cat "${compact_log}" >&2
    exit 1
fi
assert_sql_equals \
    "SELECT (pgcontext.ivfflat_index_info('public.ivf_docs_embedding_idx'::regclass)->>'verified')::boolean" \
    "t"
rm -f "${dml_log}" "${compact_log}"

# Release two compactors from the same advisory-lock barrier. The SQL function
# must not acquire an index AccessShare lock while decoding its regclass
# argument: both sessions need to serialize directly on AccessExclusive.
barrier_key=737467310042
barrier_log="${HEAVY_TMPDIR}/${DBNAME}_ivfflat_compactor_barrier.log"
compact_one_log="${HEAVY_TMPDIR}/${DBNAME}_ivfflat_compactor_one.log"
compact_two_log="${HEAVY_TMPDIR}/${DBNAME}_ivfflat_compactor_two.log"
generation_before="$(psql_db -Atc "SELECT (pgcontext.ivfflat_index_info('public.ivf_docs_embedding_idx'::regclass)->>'generation')::bigint")"

PGOPTIONS="${PGOPTIONS:-} -c search_path=public,pgcontext" \
    psql -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" -v ON_ERROR_STOP=1 \
    >"${barrier_log}" 2>&1 <<SQL &
SELECT pg_catalog.pg_advisory_lock(${barrier_key});
SELECT pg_catalog.pg_sleep(5);
SELECT pg_catalog.pg_advisory_unlock(${barrier_key});
SQL
barrier_pid=$!

observed_barrier=false
for _ in $(seq 1 50); do
    if psql_db -Atc "SELECT count(*)
                       FROM pg_catalog.pg_stat_activity
                      WHERE datname = current_database()
                        AND query LIKE '%pg_catalog.pg_sleep(5)%'
                        AND wait_event = 'PgSleep'" | grep -qx '1'; then
        observed_barrier=true
        break
    fi
    sleep 0.1
done
if [[ "${observed_barrier}" != "true" ]]; then
    echo "did not observe the IVFFlat compactor barrier" >&2
    cat "${barrier_log}" >&2
    wait "${barrier_pid}" || true
    exit 1
fi

for compact_log_path in "${compact_one_log}" "${compact_two_log}"; do
    PGOPTIONS="${PGOPTIONS:-} -c search_path=public,pgcontext -c statement_timeout=30s -c deadlock_timeout=200ms" \
        psql -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" -v ON_ERROR_STOP=1 \
        >"${compact_log_path}" 2>&1 <<SQL &
BEGIN;
SELECT pg_catalog.pg_advisory_lock_shared(${barrier_key});
SELECT pgcontext.compact_ivfflat('public.ivf_docs_embedding_idx'::regclass)->>'generation';
SELECT pg_catalog.pg_advisory_unlock_shared(${barrier_key});
COMMIT;
SQL
    if [[ "${compact_log_path}" == "${compact_one_log}" ]]; then
        compact_one_pid=$!
    else
        compact_two_pid=$!
    fi
done

observed_waiters=false
for _ in $(seq 1 50); do
    if psql_db -Atc "SELECT count(*)
                       FROM pg_catalog.pg_stat_activity
                      WHERE datname = current_database()
                        AND wait_event_type = 'Lock'
                        AND query LIKE '%pg_advisory_lock_shared(${barrier_key})%'" | grep -qx '2'; then
        observed_waiters=true
        break
    fi
    sleep 0.1
done
if [[ "${observed_waiters}" != "true" ]]; then
    echo "did not observe both IVFFlat compactors at the barrier" >&2
    cat "${compact_one_log}" "${compact_two_log}" >&2
    wait "${compact_one_pid}" || true
    wait "${compact_two_pid}" || true
    wait "${barrier_pid}" || true
    exit 1
fi

wait "${barrier_pid}"
wait "${compact_one_pid}"
wait "${compact_two_pid}"
generation_after="$(psql_db -Atc "SELECT (pgcontext.ivfflat_index_info('public.ivf_docs_embedding_idx'::regclass)->>'generation')::bigint")"
if [[ "${generation_after}" -ne "$((generation_before + 2))" ]]; then
    echo "concurrent IVFFlat compactors did not publish two generations" >&2
    cat "${compact_one_log}" "${compact_two_log}" >&2
    exit 1
fi
assert_sql_equals \
    "SELECT (pgcontext.ivfflat_index_info('public.ivf_docs_embedding_idx'::regclass)->>'verified')::boolean" \
    "t"
rm -f "${barrier_log}" "${compact_one_log}" "${compact_two_log}"

dump_path="${HEAVY_TMPDIR}/${DBNAME}_ivfflat.dump"
restore_db="${DBNAME}_restore"
require_simple_identifier "${restore_db}" "restore database"
"$(pg_bin pg_dump)" -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" -Fc -f "${dump_path}"
drop_database "${restore_db}"
create_database "${restore_db}"
"$(pg_bin pg_restore)" -h "${PGHOST}" -p "${PGPORT}" -d "${restore_db}" \
    --exit-on-error "${dump_path}"
psql -h "${PGHOST}" -p "${PGPORT}" -d "${restore_db}" -v ON_ERROR_STOP=1 <<'SQL'
SET enable_seqscan = off;
SELECT id
  FROM public.ivf_docs
 ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0,0,0]'::pgcontext.vector
 LIMIT 1;
SELECT pgcontext.ivfflat_index_info('public.ivf_docs_embedding_idx'::regclass)->>'verified';
SQL
drop_database "${restore_db}"
rm -f "${dump_path}"

printf 'ivfflat_external_parallel_build_verified\n'
printf 'ivfflat_dml_vacuum_verified\n'
printf 'ivfflat_delta_tombstone_widening_verified\n'
printf 'ivfflat_reindex_verified\n'
printf 'ivfflat_create_index_concurrently_verified\n'
printf 'ivfflat_partitioned_index_verified\n'
printf 'ivfflat_concurrent_dml_compaction_lock_order_verified\n'
printf 'ivfflat_concurrent_compactors_verified\n'
printf 'ivfflat_rls_acl_boundary_verified\n'
printf 'ivfflat_dump_restore_verified\n'
