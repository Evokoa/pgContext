#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_pgvector_hnsw_lifecycle}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

validate_metric_orders() {
    local phase="$1"
    local row suffix operator
    local index_order exact_order index_plan
    local -a cases=(
        'l2:<->'
        'ip:<#>'
        'cosine:<=>'
        'l1:<+>'
    )

    for row in "${cases[@]}"; do
        suffix="${row%%:*}"
        operator="${row#*:}"
        index_order="$(psql_db -Atc "
            SET enable_seqscan = off;
            SET enable_bitmapscan = off;
            SELECT array_agg(id)
              FROM (
                   SELECT id
                     FROM public.pgvector_hnsw_lifecycle
                    ORDER BY embedding OPERATOR(pgcontext.${operator}) '[1,1]'::vector, id
                    LIMIT 10
              ) ordered" | tail -n 1)"
        exact_order="$(psql_db -Atc "
            SET enable_indexscan = off;
            SET enable_bitmapscan = off;
            SELECT array_agg(id)
              FROM (
                   SELECT id
                     FROM public.pgvector_hnsw_lifecycle
                    ORDER BY embedding OPERATOR(pgcontext.${operator}) '[1,1]'::vector, id
                    LIMIT 10
              ) ordered" | tail -n 1)"
        if [[ "${index_order}" != "${exact_order}" ]]; then
            echo "${suffix} HNSW order differs from exact oracle after ${phase}" >&2
            echo "index: ${index_order}" >&2
            echo "exact: ${exact_order}" >&2
            exit 1
        fi
        index_plan="$(psql_db -Atc "
            SET enable_seqscan = off;
            SET enable_bitmapscan = off;
            EXPLAIN (COSTS TRUE, FORMAT TEXT)
            SELECT id
              FROM public.pgvector_hnsw_lifecycle
             ORDER BY embedding OPERATOR(pgcontext.${operator}) '[1,1]'::vector
             LIMIT 1")"
        if [[ "${index_plan}" != *"Index Scan using pgvector_hnsw_lifecycle_${suffix}_idx"* ]]; then
            echo "expected ${suffix} HNSW index scan after ${phase}, got:" >&2
            echo "${index_plan}" >&2
            exit 1
        fi
        printf 'pgvector_hnsw_lifecycle_metric: %s:%s\n' "${suffix}" "${phase}"
    done
}

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.pgvector_hnsw_lifecycle (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    body text NOT NULL
);

INSERT INTO public.pgvector_hnsw_lifecycle (id, embedding, body)
VALUES
    (1, '[1,0]'::vector, 'one'),
    (2, '[0,1]'::vector, 'two'),
    (9, '[3,3]'::vector, 'tie nine'),
    (10, '[3,3]'::vector, 'tie ten');

CREATE INDEX pgvector_hnsw_lifecycle_l2_idx
    ON public.pgvector_hnsw_lifecycle USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
CREATE INDEX pgvector_hnsw_lifecycle_ip_idx
    ON public.pgvector_hnsw_lifecycle USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ip_ops);
CREATE INDEX pgvector_hnsw_lifecycle_cosine_idx
    ON public.pgvector_hnsw_lifecycle USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops);
CREATE INDEX pgvector_hnsw_lifecycle_l1_idx
    ON public.pgvector_hnsw_lifecycle USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_l1_ops);

INSERT INTO public.pgvector_hnsw_lifecycle VALUES (11, '[4,4]'::vector, 'inserted');
UPDATE public.pgvector_hnsw_lifecycle
   SET embedding = '[5,5]'::vector, body = 'updated'
 WHERE id = 2;
DELETE FROM public.pgvector_hnsw_lifecycle WHERE id = 1;
VACUUM (ANALYZE) public.pgvector_hnsw_lifecycle;
REINDEX TABLE public.pgvector_hnsw_lifecycle;
CHECKPOINT;
SQL

validate_metric_orders "before_restart"
cargo pgrx stop "${PG_VERSION}"
cargo pgrx start "${PG_VERSION}"
validate_metric_orders "after_restart"

printf 'pgvector_hnsw_lifecycle_complete\n'
