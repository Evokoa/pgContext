#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_hnsw_bounded_serving}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;
CREATE TABLE public.bounded_hnsw_docs (id bigint PRIMARY KEY, embedding vector NOT NULL);
INSERT INTO public.bounded_hnsw_docs
SELECT value, format('[%s,0]', value)::vector FROM generate_series(1, 256) AS value;
CREATE INDEX bounded_hnsw_docs_embedding_idx
    ON public.bounded_hnsw_docs USING pgcontext_hnsw (embedding);
SET enable_seqscan = off;
DO $$
DECLARE
    nearest bigint;
    pages bigint;
    nodes bigint;
    candidates bigint;
    rechecks bigint;
    exact boolean;
BEGIN
    SELECT id INTO nearest FROM public.bounded_hnsw_docs
     ORDER BY embedding OPERATOR(pgcontext.<->) '[128,0]'::vector LIMIT 1;
    IF nearest <> 128 THEN RAISE EXCEPTION 'bounded HNSW nearest result is wrong: %', nearest; END IF;
    SELECT work.page_visits, work.node_reads, work.candidates, work.rechecks, work.exact_strategy
      INTO pages, nodes, candidates, rechecks, exact FROM pgcontext.hnsw_last_scan_work() AS work;
    IF pages <= 0 OR nodes <= 0 OR candidates <= 0 OR candidates >= 256 OR rechecks <= 0 OR exact THEN
        RAISE EXCEPTION 'bounded HNSW scan work is invalid: pages %, nodes %, candidates %, rechecks %, exact %', pages, nodes, candidates, rechecks, exact;
    END IF;
END $$;
SQL

plan="$(psql_db -Atc "SET enable_seqscan = off; EXPLAIN (COSTS true) SELECT id FROM public.bounded_hnsw_docs ORDER BY embedding OPERATOR(pgcontext.<->) '[128,0]'::vector LIMIT 1;")"
[[ "${plan}" == *"Index Scan"* ]]

printf 'hnsw_bounded_scale_costing_work_verified\n'
DBNAME="${DBNAME}_concurrent" "${SCRIPT_DIR}/concurrent_read_write.sh"
printf 'hnsw_bounded_concurrent_lifecycle_verified\n'
