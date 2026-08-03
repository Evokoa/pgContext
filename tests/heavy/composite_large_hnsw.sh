#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_composite_large_hnsw}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.composite_large_hnsw_docs (
    id bigint PRIMARY KEY,
    embedding vector(2) NOT NULL
);
INSERT INTO public.composite_large_hnsw_docs
SELECT value,
       format('[%s,%s]', value % 997, (value * 31) % 991)::vector
  FROM generate_series(1, 15500) AS value;

SELECT pgcontext.create_collection(
    'composite_large_hnsw_docs',
    'public.composite_large_hnsw_docs'
);
SELECT pgcontext.register_vector(
    'composite_large_hnsw_docs', 'embedding', 'embedding', 2, 'l2'
);
SELECT pgcontext.backfill_points('composite_large_hnsw_docs', 20000);

CREATE INDEX composite_large_hnsw_docs_hnsw
    ON public.composite_large_hnsw_docs
    USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
SELECT pgcontext.attach_hnsw_index(
    'composite_large_hnsw_docs',
    'embedding',
    'public.composite_large_hnsw_docs_hnsw'
);

DO $verify$
DECLARE
    result_count bigint;
BEGIN
    SELECT count(*)
      INTO result_count
      FROM pgcontext.execute_query(
          'composite_large_hnsw_docs',
          pgcontext.query_nearest('[1,31]'::vector, 5)
      );
    IF result_count <> 5 THEN
        RAISE EXCEPTION 'large typed HNSW returned %, expected 5', result_count;
    END IF;
END
$verify$;

SQL

observed="0"
for _ in $(seq 1 100); do
    observed="$(psql_db -Atc "
        SELECT count(*)
          FROM pgcontext.query_execution_stats()
         WHERE collection_name = 'composite_large_hnsw_docs'
           AND strategy = 'dense_hnsw'
           AND completion = 'complete'
    ")"
    [[ "${observed}" -gt 0 ]] && break
    sleep 0.05
done
if [[ "${observed}" -eq 0 ]]; then
    echo "large typed HNSW did not record a complete dense_hnsw execution" >&2
    exit 1
fi

printf 'composite_large_hnsw: ok (15500 active rows under default query budgets)\n'
