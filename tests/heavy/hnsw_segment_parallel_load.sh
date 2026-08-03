#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_hnsw_segment_parallel_load}"
SOAK_SECONDS="${SOAK_SECONDS:-2}"
CLIENT_LEVELS="${CLIENT_LEVELS:-1 16 32 64}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

start_and_install_extension
reset_database

query_sql="${HEAVY_TMPDIR}/${DBNAME}_query.sql"
update_sql="${HEAVY_TMPDIR}/${DBNAME}_update.sql"
cleanup() {
    rm -f "${query_sql}" "${update_sql}"
}
trap cleanup EXIT

psql_db <<'SQL'
CREATE EXTENSION pgcontext;
DO $$
BEGIN
    BEGIN
        PERFORM set_config('pgcontext.hnsw_delta_segment_limit', '10001', false);
        RAISE EXCEPTION 'delta segment limit accepted a value above 10000';
    EXCEPTION WHEN invalid_parameter_value THEN
        NULL;
    END;
END
$$;
CREATE TABLE public.segment_load_docs (
    id bigint PRIMARY KEY,
    embedding vector(2) NOT NULL
);
INSERT INTO public.segment_load_docs
SELECT n, format('[%s,1]', n)::vector FROM generate_series(1, 32) n;
CREATE INDEX segment_load_docs_hnsw ON public.segment_load_docs
USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops);
SET pgcontext.hnsw_delta_segment_limit = 4;
INSERT INTO public.segment_load_docs
SELECT n, format('[%s,1]', n)::vector FROM generate_series(33, 41) n;
DO $$
DECLARE
    denials_before bigint;
    denials_after bigint;
    serial_before bigint;
    serial_after bigint;
BEGIN
    SELECT parallel_admission_denials, serial_segment_degradations
      INTO denials_before, serial_before
      FROM pgcontext.hnsw_serving_stats();
    PERFORM set_config('enable_seqscan', 'off', true);
    PERFORM set_config('pgcontext.hnsw_segment_parallel_workers', '4', true);
    PERFORM set_config('pgcontext.hnsw_shared_serving_budget_mb', '0', true);
    PERFORM id
      FROM public.segment_load_docs
     ORDER BY embedding OPERATOR(pgcontext.<=>) '[64,1]'::pgcontext.vector
     LIMIT 5;
    SELECT parallel_admission_denials, serial_segment_degradations
      INTO denials_after, serial_after
      FROM pgcontext.hnsw_serving_stats();
    IF denials_after <= denials_before OR serial_after <= serial_before THEN
        RAISE EXCEPTION
            'zero-budget parallel search did not degrade serially: denials % -> %, serial % -> %',
            denials_before, denials_after, serial_before, serial_after;
    END IF;
END
$$;
INSERT INTO public.segment_load_docs
SELECT n, format('[%s,1]', n)::vector FROM generate_series(42, 96) n;
DO $$
DECLARE
    segments integer;
BEGIN
    SELECT segment_count INTO segments
      FROM pgcontext.hnsw_segment_stats('segment_load_docs_hnsw'::regclass);
    IF segments <> 16 THEN
        RAISE EXCEPTION 'parallel-load fixture expected 16 segments, saw %', segments;
    END IF;
END
$$;
SQL
printf 'hnsw_segment_parallel_budget_degradation_verified\n'

cat >"${query_sql}" <<'SQL'
SET enable_seqscan = off;
SET pgcontext.hnsw_segment_parallel_workers = 4;
SET pgcontext.hnsw_mmap_serving = false;
SET pgcontext.hnsw_shared_serving = false;
SELECT id FROM public.segment_load_docs
 ORDER BY embedding OPERATOR(pgcontext.<=>) '[64,1]'::pgcontext.vector
 LIMIT 5;
SQL

for clients in ${CLIENT_LEVELS}; do
    jobs="${clients}"
    if [[ "${jobs}" -gt 8 ]]; then jobs=8; fi
    pgbench -n -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" \
        -c "${clients}" -j "${jobs}" -T "${SOAK_SECONDS}" -f "${query_sql}" >/dev/null
    printf 'hnsw_segment_parallel_clients_%s_verified\n' "${clients}"
done

# The backend wait loop must process PostgreSQL interrupts while pure workers
# are active. A correlated scan makes the timeout deterministic without a
# server-side sleep in the worker tasks.
if psql_db -v ON_ERROR_STOP=1 <<'SQL'
SET statement_timeout = '5ms';
SET enable_seqscan = off;
SET pgcontext.hnsw_segment_parallel_workers = 4;
SELECT count(*)
  FROM generate_series(1, 100000) g
 CROSS JOIN LATERAL (
       SELECT id FROM public.segment_load_docs
        ORDER BY embedding OPERATOR(pgcontext.<=>)
                 format('[%s,1]', g)::pgcontext.vector
        LIMIT 5
 ) ranked;
SQL
then
    echo "parallel HNSW cancellation fixture unexpectedly completed" >&2
    exit 1
fi
printf 'hnsw_segment_parallel_cancellation_verified\n'

# Catch cancellation inside one backend and immediately reuse that same
# session. This fails if ProcessInterrupts longjmps before pooled results are
# drained and session-level admission locks are released.
psql_db <<'SQL'
SET statement_timeout = '5ms';
DO $$
BEGIN
    BEGIN
        PERFORM count(*)
          FROM generate_series(1, 100000) g
         CROSS JOIN LATERAL (
               SELECT id FROM public.segment_load_docs
                ORDER BY embedding OPERATOR(pgcontext.<=>)
                         format('[%s,1]', g)::pgcontext.vector
                LIMIT 5
         ) ranked;
        RAISE EXCEPTION 'same-session cancellation fixture unexpectedly completed';
    EXCEPTION WHEN query_canceled THEN
        NULL;
    END;
END
$$;
RESET statement_timeout;
DO $$
DECLARE
    leaked bigint;
    recovered bigint;
BEGIN
    SELECT count(*) INTO leaked
      FROM pg_locks
     WHERE locktype = 'advisory'
       AND pid = pg_backend_pid()
       AND classid = 1346850899;
    IF leaked <> 0 THEN
        RAISE EXCEPTION 'same-session cancellation leaked % parallel admission locks', leaked;
    END IF;
    SET LOCAL enable_seqscan = off;
    SET LOCAL pgcontext.hnsw_segment_parallel_workers = 4;
    SELECT count(*) INTO recovered
      FROM (
          SELECT id FROM public.segment_load_docs
           ORDER BY embedding OPERATOR(pgcontext.<=>) '[64,1]'::pgcontext.vector
           LIMIT 5
      ) ranked;
    IF recovered <> 5 THEN
        RAISE EXCEPTION 'same-session parallel recovery returned % rows', recovered;
    END IF;
END
$$;
SQL
printf 'hnsw_segment_same_session_interrupt_recovery_verified\n'

cat >"${update_sql}" <<'SQL'
\set id random(1, 96)
UPDATE public.segment_load_docs
   SET embedding = format('[%s,1]', 100000 + :id)::pgcontext.vector
 WHERE id = :id;
SQL

pgbench -n -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" \
    -c 16 -j 8 -T "${SOAK_SECONDS}" -f "${query_sql}" >/dev/null &
query_load_pid=$!
pgbench -n -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" \
    -c 16 -j 8 -T "${SOAK_SECONDS}" -f "${update_sql}" >/dev/null
wait "${query_load_pid}"
printf 'hnsw_segment_concurrent_append_query_snapshot_verified\n'
psql_db -c 'VACUUM public.segment_load_docs'
psql_db <<'SQL'
DO $$
DECLARE
    segments integer;
BEGIN
    SELECT segment_count INTO segments
      FROM pgcontext.hnsw_segment_stats('segment_load_docs_hnsw'::regclass);
    IF segments > 16 THEN
        RAISE EXCEPTION 'mixed load exceeded bounded segment directory: %', segments;
    END IF;
END
$$;
SQL
printf 'hnsw_segment_mixed_load_bounded_verified\n'

# Retire more rows than the requested LIMIT. The segmented scan must read the
# overlay first and overfetch base successors rather than filling the result
# with farther delta rows or returning fewer than five rows.
psql_db <<'SQL'
CREATE TABLE public.segment_retirement_probe (
    id bigint PRIMARY KEY,
    embedding vector(2) NOT NULL
);
INSERT INTO public.segment_retirement_probe
SELECT n, format('[%s,0]', n)::vector FROM generate_series(1, 80) n;
CREATE INDEX segment_retirement_probe_hnsw
    ON public.segment_retirement_probe USING pgcontext_hnsw
       (embedding pgcontext.vector_hnsw_ops);
SET pgcontext.hnsw_delta_segment_limit = 4;
DELETE FROM public.segment_retirement_probe WHERE id <= 40;
VACUUM public.segment_retirement_probe;
DO $$
DECLARE
    indexed bigint[];
    exact bigint[];
    segments integer;
    active bigint;
BEGIN
    SET LOCAL enable_seqscan = off;
    SELECT array_agg(id ORDER BY distance, id) INTO indexed
      FROM (
          SELECT id, embedding OPERATOR(pgcontext.<->) '[1,0]'::vector AS distance
            FROM public.segment_retirement_probe
           ORDER BY embedding OPERATOR(pgcontext.<->) '[1,0]'::vector, id
           LIMIT 5
      ) ranked;
    SET LOCAL enable_indexscan = off;
    SET LOCAL enable_seqscan = on;
    SELECT array_agg(id ORDER BY distance, id) INTO exact
      FROM (
          SELECT id, embedding OPERATOR(pgcontext.<->) '[1,0]'::vector AS distance
            FROM public.segment_retirement_probe
           ORDER BY embedding OPERATOR(pgcontext.<->) '[1,0]'::vector, id
           LIMIT 5
      ) ranked;
    IF indexed IS DISTINCT FROM exact OR cardinality(indexed) <> 5 THEN
        RAISE EXCEPTION 'retirement-heavy top-k mismatch: indexed %, exact %', indexed, exact;
    END IF;
    SELECT segment_count, active_delta_records
      INTO segments, active
      FROM pgcontext.hnsw_segment_stats('segment_retirement_probe_hnsw');
    IF segments > 16 OR active > 4 THEN
        RAISE EXCEPTION 'bounded VACUUM state violated: segments %, active %', segments, active;
    END IF;
END
$$;
SQL
printf 'hnsw_segment_retirement_topk_and_chunked_vacuum_verified\n'

# Exercise heap-TID reuse across an older tombstone segment and a newer live
# graph segment. Chronological replay must allow the newer graph row to
# resurrect the physical TID before and after one bounded pair compaction.
psql_db <<'SQL'
CREATE TABLE public.segment_tid_reuse_probe (
    id bigint PRIMARY KEY,
    embedding vector(2) NOT NULL
);
INSERT INTO public.segment_tid_reuse_probe VALUES (1, '[0,0]');
CREATE INDEX segment_tid_reuse_probe_hnsw
    ON public.segment_tid_reuse_probe USING pgcontext_hnsw
       (embedding pgcontext.vector_hnsw_ops);
SET pgcontext.hnsw_delta_segment_limit = 1;
DELETE FROM public.segment_tid_reuse_probe WHERE id = 1;
VACUUM public.segment_tid_reuse_probe;
INSERT INTO public.segment_tid_reuse_probe VALUES (2, '[1,0]');
INSERT INTO public.segment_tid_reuse_probe VALUES (3, '[9,0]');
DO $$
DECLARE
    nearest bigint;
    epoch bigint;
BEGIN
    SET LOCAL enable_seqscan = off;
    SELECT id INTO nearest FROM public.segment_tid_reuse_probe
     ORDER BY embedding OPERATOR(pgcontext.<->) '[1,0]'::vector LIMIT 1;
    IF nearest <> 2 THEN
        RAISE EXCEPTION 'newer reused-TID row was retired before compaction: %', nearest;
    END IF;
    SELECT directory_epoch INTO epoch
      FROM pgcontext.hnsw_segment_stats('segment_tid_reuse_probe_hnsw');
    PERFORM pgcontext._compact_hnsw_segment_pair(
        'segment_tid_reuse_probe_hnsw'::regclass, epoch
    );
    SELECT id INTO nearest FROM public.segment_tid_reuse_probe
     ORDER BY embedding OPERATOR(pgcontext.<->) '[1,0]'::vector LIMIT 1;
    IF nearest <> 2 THEN
        RAISE EXCEPTION 'newer reused-TID row was retired after compaction: %', nearest;
    END IF;
END
$$;
SQL
printf 'hnsw_segment_tid_reuse_chronology_verified\n'
