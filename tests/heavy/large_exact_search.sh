#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_large_exact_search}"
if [[ "${LARGE_EXACT_FULL:-0}" == "1" ]]; then
    ROW_COUNT="${ROW_COUNT:-1000000}"
else
    ROW_COUNT="${ROW_COUNT:-25000}"
fi
BATCH_SIZE="${BATCH_SIZE:-10000}"
TOP_K="${TOP_K:-25}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

if [[ ! "${ROW_COUNT}" =~ ^[1-9][0-9]*$ ]]; then
    echo "ROW_COUNT must be a positive integer" >&2
    exit 2
fi
if [[ ! "${BATCH_SIZE}" =~ ^[1-9][0-9]*$ ]]; then
    echo "BATCH_SIZE must be a positive integer" >&2
    exit 2
fi
if [[ ! "${TOP_K}" =~ ^[1-9][0-9]*$ ]]; then
    echo "TOP_K must be a positive integer" >&2
    exit 2
fi
if (( TOP_K > ROW_COUNT )); then
    echo "TOP_K must be less than or equal to ROW_COUNT" >&2
    exit 2
fi

start_and_install_extension
reset_database

psql_db \
    -v row_count="${ROW_COUNT}" \
    -v batch_size="${BATCH_SIZE}" \
    -v top_k="${TOP_K}" <<'SQL'
CREATE EXTENSION pgcontext;
SELECT set_config('pgcontext_heavy.row_count', :'row_count', false);
SELECT set_config('pgcontext_heavy.batch_size', :'batch_size', false);
SELECT set_config('pgcontext_heavy.top_k', :'top_k', false);

CREATE TABLE public.large_exact_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    tenant text NOT NULL,
    body text NOT NULL
);

INSERT INTO public.large_exact_docs (id, embedding, tenant, body)
SELECT value,
       format('[%s,0]', value)::vector,
       CASE WHEN value % 10 = 0 THEN 'tenant_zero' ELSE 'tenant_rest' END,
       format('large exact fixture %s', value)
  FROM generate_series(1, :row_count::bigint) AS value;

SELECT pgcontext.create_collection('large_exact_docs', 'public.large_exact_docs');
SELECT pgcontext.register_vector('large_exact_docs', 'embedding', 'embedding', 2, 'l2');
SELECT pgcontext.register_filter_column('large_exact_docs', 'tenant', 'tenant');

DO $$
DECLARE
    row_count bigint := current_setting('pgcontext_heavy.row_count')::bigint;
    batch_size bigint := current_setting('pgcontext_heavy.batch_size')::bigint;
    lower_bound bigint := 1;
    upper_bound bigint;
    source_keys text[];
BEGIN
    WHILE lower_bound <= row_count LOOP
        upper_bound := LEAST(lower_bound + batch_size - 1, row_count);

        SELECT array_agg(value::text ORDER BY value)
          INTO source_keys
          FROM generate_series(lower_bound, upper_bound) AS value;

        PERFORM pgcontext.upsert_points('large_exact_docs', source_keys);
        lower_bound := upper_bound + 1;
    END LOOP;
END
$$;

DO $$
DECLARE
    row_count bigint := current_setting('pgcontext_heavy.row_count')::bigint;
    top_k integer := current_setting('pgcontext_heavy.top_k')::integer;
    source_rows bigint;
    active_points bigint;
    query_vector vector := format('[%s,0]', (current_setting('pgcontext_heavy.row_count')::numeric / 2) + 0.25)::vector;
    mismatch_count bigint;
    no_match_count bigint;
BEGIN
    SELECT count(*) INTO source_rows FROM public.large_exact_docs;
    IF source_rows <> row_count THEN
        RAISE EXCEPTION 'source table row count mismatch: expected %, got %',
            row_count, source_rows;
    END IF;

    SELECT count(*)
      INTO active_points
      FROM pgcontext._collection_points AS points
      JOIN pgcontext._collections AS collections USING (collection_id)
     WHERE collections.collection_name = 'large_exact_docs'
       AND points.deleted_at IS NULL;
    IF active_points <> row_count THEN
        RAISE EXCEPTION 'active point count mismatch: expected %, got %',
            row_count, active_points;
    END IF;
    RAISE NOTICE 'large_exact_rows_loaded';

    WITH search_rows AS (
        SELECT row_number() OVER (ORDER BY score, source_key::bigint) AS ordinal,
               source_key,
               score
          FROM pgcontext.search('large_exact_docs', query_vector, top_k)
    ),
    oracle_rows AS (
        SELECT row_number() OVER (ORDER BY distance, id) AS ordinal,
               id::text AS source_key,
               distance AS score
          FROM (
              SELECT id,
                     embedding OPERATOR(pgcontext.<->) query_vector AS distance
                FROM public.large_exact_docs
               ORDER BY embedding OPERATOR(pgcontext.<->) query_vector, id
               LIMIT top_k
          ) exact_rows
    )
    SELECT count(*)
      INTO mismatch_count
      FROM search_rows
      FULL JOIN oracle_rows USING (ordinal)
     WHERE search_rows.source_key IS DISTINCT FROM oracle_rows.source_key
        OR abs(search_rows.score - oracle_rows.score) > 0.0001;
    IF mismatch_count <> 0 THEN
        RAISE EXCEPTION 'large exact search diverged from SQL oracle: % mismatches',
            mismatch_count;
    END IF;
    RAISE NOTICE 'large_exact_oracle_match';

    SELECT count(*)
      INTO no_match_count
      FROM pgcontext.search(
          'large_exact_docs',
          query_vector,
          '{"must":[{"key":"tenant","match":"missing"}]}',
          top_k
      );
    IF no_match_count <> 0 THEN
        RAISE EXCEPTION 'missing-tenant filter returned % rows', no_match_count;
    END IF;
    RAISE NOTICE 'large_exact_missing_filter_empty';
END
$$;

DO $$
BEGIN
    PERFORM *
      FROM pgcontext.search('large_exact_docs', '[1,2,3]'::vector, 1);
    RAISE EXCEPTION 'dimension mismatch search unexpectedly succeeded';
EXCEPTION WHEN invalid_parameter_value THEN
    RAISE NOTICE 'large_exact_dimension_mismatch_rejected';
END
$$;

DO $$
BEGIN
    PERFORM *
      FROM pgcontext.search(
          'large_exact_docs',
          '[1,0]'::vector,
          '{"must":[{"key":"unknown","match":"x"}]}',
          1
      );
    RAISE EXCEPTION 'unknown filter search unexpectedly succeeded';
EXCEPTION WHEN invalid_parameter_value THEN
    RAISE NOTICE 'large_exact_unknown_filter_rejected';
END
$$;
SQL
