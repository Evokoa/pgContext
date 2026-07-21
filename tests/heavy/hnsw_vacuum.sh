#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_hnsw_vacuum}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.vacuum_hnsw_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    tenant text NOT NULL
);

INSERT INTO public.vacuum_hnsw_docs (id, embedding, tenant)
SELECT value,
       format('[%s,0]', value)::vector,
       CASE WHEN value % 2 = 0 THEN 'even' ELSE 'odd' END
  FROM generate_series(1, 32) AS value;

CREATE INDEX vacuum_hnsw_docs_embedding_idx
    ON public.vacuum_hnsw_docs USING pgcontext_hnsw (embedding);

CREATE TEMP TABLE hot_hnsw_observed AS
SELECT ctid AS old_tid FROM public.vacuum_hnsw_docs WHERE id = 9;
UPDATE public.vacuum_hnsw_docs SET tenant = 'hot-updated' WHERE id = 9;
DO $$
DECLARE
    old_tid tid;
    updated_tid tid;
    nearest_id bigint;
BEGIN
    SELECT observed.old_tid INTO old_tid FROM hot_hnsw_observed AS observed;
    SELECT ctid INTO updated_tid FROM public.vacuum_hnsw_docs WHERE id = 9;
    IF updated_tid = old_tid THEN RAISE EXCEPTION 'expected HOT tuple version to advance ctid'; END IF;
    SET LOCAL enable_seqscan = off;
    SELECT id INTO nearest_id FROM public.vacuum_hnsw_docs
     ORDER BY embedding OPERATOR(pgcontext.<->) '[9,0]'::vector LIMIT 1;
    IF nearest_id <> 9 THEN RAISE EXCEPTION 'HOT update changed HNSW ordering: %', nearest_id; END IF;
END
$$;

UPDATE public.vacuum_hnsw_docs
   SET embedding = '[100,0]'::vector
 WHERE id = 1;
DELETE FROM public.vacuum_hnsw_docs WHERE id IN (2, 3, 4);
INSERT INTO public.vacuum_hnsw_docs (id, embedding, tenant)
VALUES (200, '[200,0]'::vector, 'even');

VACUUM (ANALYZE) public.vacuum_hnsw_docs;

SET enable_seqscan = off;

DO $$
DECLARE
    nearest_id bigint;
    deleted_count bigint;
    status_count bigint;
    vacuum_index_count bigint;
BEGIN
    SELECT id
      INTO nearest_id
      FROM public.vacuum_hnsw_docs
     ORDER BY embedding OPERATOR(pgcontext.<->) '[100,0]'::vector
     LIMIT 1;
    IF nearest_id <> 1 THEN
        RAISE EXCEPTION 'unexpected nearest id after update/delete/vacuum: %', nearest_id;
    END IF;

    SELECT count(*) INTO deleted_count
      FROM public.vacuum_hnsw_docs
     WHERE id IN (2, 3, 4);
    IF deleted_count <> 0 THEN
        RAISE EXCEPTION 'deleted rows remained visible after vacuum: %', deleted_count;
    END IF;

    SELECT count(*) INTO status_count
      FROM pgcontext.index_status('public.vacuum_hnsw_docs_embedding_idx')
     WHERE access_method = 'pgcontext_hnsw'
       AND status::text = 'Ready';
    IF status_count <> 1 THEN
        RAISE EXCEPTION 'HNSW index was not ready after vacuum';
    END IF;

    SELECT count(*) INTO vacuum_index_count
      FROM pgcontext.vacuum_advice('public.vacuum_hnsw_docs_embedding_idx')
     WHERE index_name = 'vacuum_hnsw_docs_embedding_idx'
       AND access_method = 'pgcontext_hnsw';
    IF vacuum_index_count <> 1 THEN
        RAISE EXCEPTION 'vacuum advice did not report HNSW index';
    END IF;
END
$$;
SQL

printf 'hnsw_vacuum_nearest_rechecked\n'
printf 'hnsw_vacuum_deleted_rows_pruned\n'
printf 'hnsw_vacuum_index_ready\n'
printf 'hnsw_vacuum_advice_present\n'
printf 'hnsw_hot_update_rechecked\n'

psql_db <<'SQL'
DO $$
DECLARE
    pages bigint;
    nodes bigint;
    candidates bigint;
    rechecks bigint;
    exact boolean;
BEGIN
    PERFORM id
      FROM public.vacuum_hnsw_docs
     ORDER BY embedding OPERATOR(pgcontext.<->) '[100,0]'::vector
     LIMIT 1;
    SELECT work.page_visits, work.node_reads, work.candidates, work.rechecks, work.exact_strategy
      INTO pages, nodes, candidates, rechecks, exact
      FROM pgcontext.hnsw_last_scan_work() AS work;
    IF pages <= 0 OR nodes <= 0 OR candidates <= 0 OR rechecks <= 0 OR exact THEN
        RAISE EXCEPTION 'HNSW scan work counters are not a bounded page-traversal outcome';
    END IF;
END
$$;
SQL
printf 'hnsw_scan_work_counters_present\n'

psql_db <<'SQL'
REINDEX INDEX public.vacuum_hnsw_docs_embedding_idx;

DO $$
DECLARE
    nearest_id bigint;
    status_count bigint;
BEGIN
    SELECT id
      INTO nearest_id
      FROM public.vacuum_hnsw_docs
     ORDER BY embedding OPERATOR(pgcontext.<->) '[200,0]'::vector
     LIMIT 1;
    IF nearest_id <> 200 THEN
        RAISE EXCEPTION 'unexpected nearest id after HNSW REINDEX: %', nearest_id;
    END IF;

    SELECT count(*) INTO status_count
      FROM pgcontext.index_status('public.vacuum_hnsw_docs_embedding_idx')
     WHERE access_method = 'pgcontext_hnsw'
       AND status::text = 'Ready';
    IF status_count <> 1 THEN
        RAISE EXCEPTION 'HNSW index was not ready after REINDEX';
    END IF;
END
$$;
SQL

printf 'hnsw_reindex_ready\n'

psql_db <<'SQL'
CREATE TABLE public.tid_reuse_hnsw_docs (id bigint PRIMARY KEY, embedding vector NOT NULL);
INSERT INTO public.tid_reuse_hnsw_docs VALUES (1, '[1,0]'::vector);
CREATE INDEX tid_reuse_hnsw_docs_embedding_idx
    ON public.tid_reuse_hnsw_docs USING pgcontext_hnsw (embedding);

CREATE TEMP TABLE tid_reuse_observed AS
SELECT ctid AS old_tid FROM public.tid_reuse_hnsw_docs WHERE id = 1;
DELETE FROM public.tid_reuse_hnsw_docs WHERE id = 1;
VACUUM public.tid_reuse_hnsw_docs;
INSERT INTO public.tid_reuse_hnsw_docs VALUES (99, '[99,0]'::vector);

DO $$
DECLARE
    old_tid tid;
    replacement_tid tid;
    nearest_id bigint;
BEGIN
    SELECT observed.old_tid INTO old_tid FROM tid_reuse_observed AS observed;
    SELECT ctid INTO replacement_tid FROM public.tid_reuse_hnsw_docs WHERE id = 99;
    IF replacement_tid <> old_tid THEN
        RAISE EXCEPTION 'forced TID reuse did not occur: old %, replacement %', old_tid, replacement_tid;
    END IF;
    SET LOCAL enable_seqscan = off;
    SELECT id INTO nearest_id
      FROM public.tid_reuse_hnsw_docs
     ORDER BY embedding OPERATOR(pgcontext.<->) '[99,0]'::vector
     LIMIT 1;
    IF nearest_id <> 99 THEN
        RAISE EXCEPTION 'stale HNSW vector ranked after TID reuse: %', nearest_id;
    END IF;
END
$$;
SQL

printf 'hnsw_tid_reuse_replacement_rechecked\n'

psql_db <<'SQL'
BEGIN;
INSERT INTO public.vacuum_hnsw_docs VALUES (700, '[700,0]'::vector, 'abort');
ROLLBACK;

BEGIN;
SAVEPOINT hnsw_update;
UPDATE public.vacuum_hnsw_docs SET embedding = '[701,0]'::vector WHERE id = 200;
ROLLBACK TO SAVEPOINT hnsw_update;
COMMIT;

DO $$
DECLARE
    aborted_count bigint;
    nearest_id bigint;
BEGIN
    SET LOCAL enable_seqscan = off;
    SELECT count(*) INTO aborted_count FROM public.vacuum_hnsw_docs WHERE id = 700;
    IF aborted_count <> 0 THEN RAISE EXCEPTION 'aborted HNSW insert became visible'; END IF;
    SELECT id INTO nearest_id FROM public.vacuum_hnsw_docs
     ORDER BY embedding OPERATOR(pgcontext.<->) '[200,0]'::vector LIMIT 1;
    IF nearest_id <> 200 THEN RAISE EXCEPTION 'savepoint rollback left stale HNSW update: %', nearest_id; END IF;
END
$$;
SQL

long_snapshot_file="${HEAVY_TMPDIR}/${DBNAME}_long_snapshot.sql"
cat >"${long_snapshot_file}" <<'SQL'
BEGIN ISOLATION LEVEL REPEATABLE READ;
SET enable_seqscan = off;
SELECT id FROM public.vacuum_hnsw_docs ORDER BY embedding OPERATOR(pgcontext.<->) '[200,0]'::vector LIMIT 1;
SELECT pg_sleep(2);
SELECT id FROM public.vacuum_hnsw_docs ORDER BY embedding OPERATOR(pgcontext.<->) '[200,0]'::vector LIMIT 1;
COMMIT;
SQL
psql_db -Atf "${long_snapshot_file}" >"${long_snapshot_file}.out" &
snapshot_pid=$!
sleep 1
psql_db -c "DELETE FROM public.vacuum_hnsw_docs WHERE id = 200"
wait "${snapshot_pid}"
[[ "$(grep -cx '200' "${long_snapshot_file}.out")" == "2" ]]
rm -f "${long_snapshot_file}" "${long_snapshot_file}.out"
printf 'hnsw_transaction_abort_savepoint_long_snapshot_rechecked\n'
