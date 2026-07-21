#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_low_memory_build}"
ROW_COUNT="${ROW_COUNT:-5000}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

if [[ ! "${ROW_COUNT}" =~ ^[1-9][0-9]*$ ]]; then
    echo "ROW_COUNT must be a positive integer" >&2
    exit 2
fi

start_and_install_extension
reset_database

psql_db -v row_count="${ROW_COUNT}" <<'SQL'
CREATE EXTENSION pgcontext;
SELECT set_config('pgcontext_heavy.row_count', :'row_count', false);

CREATE TABLE public.low_memory_items (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL
);

INSERT INTO public.low_memory_items (id, embedding)
SELECT value, format('[%s,0]', value)::vector
  FROM generate_series(1, :row_count::bigint) AS value;

SET maintenance_work_mem = '1MB';
SET pgcontext.hnsw_m = 128;
SET pgcontext.hnsw_ef_construction = 128;

DO $$
DECLARE
    index_count bigint;
    build_error text;
BEGIN
    BEGIN
        EXECUTE 'CREATE INDEX low_memory_items_embedding_bad_idx
                    ON public.low_memory_items USING pgcontext_hnsw (embedding)';
        RAISE EXCEPTION 'low-budget HNSW build unexpectedly succeeded';
    EXCEPTION WHEN invalid_parameter_value THEN
        GET STACKED DIAGNOSTICS build_error = MESSAGE_TEXT;
        IF build_error NOT LIKE 'HNSW build estimated memory % exceeds maintenance_work_mem budget %' THEN
            RAISE EXCEPTION 'low-budget HNSW build failed for wrong reason: %',
                build_error;
        END IF;
    END;

    SELECT count(*)
      INTO index_count
      FROM pg_catalog.pg_class
     WHERE relname = 'low_memory_items_embedding_bad_idx';
    IF index_count <> 0 THEN
        RAISE EXCEPTION 'failed low-budget build left an index relation behind';
    END IF;
    RAISE NOTICE 'low_memory_rejected_bad_build';
    RAISE NOTICE 'low_memory_failed_build_cleaned';
END
$$;

SET pgcontext.hnsw_m = 4;
SET pgcontext.hnsw_ef_construction = 8;
-- This gate isolates low-memory construction. Use the maximum bounded search
-- frontier so approximate recall does not make the build-memory assertion
-- depend on the sparse m=4 topology selected for the successful build.
SET pgcontext.hnsw_ef_search = 4096;

CREATE INDEX low_memory_items_embedding_idx
    ON public.low_memory_items USING pgcontext_hnsw (embedding);

SET enable_seqscan = off;

DO $$
DECLARE
    ordered_ids text;
    row_count real := current_setting('pgcontext_heavy.row_count')::real;
    index_reltuples real;
BEGIN
    SELECT string_agg(id::text, ',' ORDER BY ordinal)
      INTO ordered_ids
      FROM (
          SELECT row_number() OVER () AS ordinal, id
            FROM public.low_memory_items
           ORDER BY embedding OPERATOR(pgcontext.<->) '[3,0]'::vector
           LIMIT 5
      ) rows;
    IF ordered_ids <> '3,2,4,1,5' THEN
        RAISE EXCEPTION 'low-memory HNSW index returned wrong order: %', ordered_ids;
    END IF;
    RAISE NOTICE 'low_memory_index_order_verified';

    SELECT pg_class.reltuples
      INTO index_reltuples
      FROM pg_catalog.pg_class
     WHERE relname = 'low_memory_items_embedding_idx';
    IF index_reltuples <> row_count THEN
        RAISE EXCEPTION 'low-memory HNSW reltuples mismatch: expected %, got %',
            row_count, index_reltuples;
    END IF;
    RAISE NOTICE 'low_memory_reltuples_verified';
END
$$;
SQL
