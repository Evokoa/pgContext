#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_crash_restart_hnsw}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

validate_hnsw_order() {
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
                     FROM public.restart_hnsw_docs
                    ORDER BY embedding OPERATOR(pgcontext.${operator}) '[1,1]'::vector, id
                    LIMIT 10
              ) ordered" | tail -n 1)"
        exact_order="$(psql_db -Atc "
            SET enable_indexscan = off;
            SET enable_bitmapscan = off;
            SELECT array_agg(id)
              FROM (
                   SELECT id
                     FROM public.restart_hnsw_docs
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
              FROM public.restart_hnsw_docs
             ORDER BY embedding OPERATOR(pgcontext.${operator}) '[1,1]'::vector
             LIMIT 1")"
        if [[ "${index_plan}" != *"Index Scan using restart_hnsw_docs_${suffix}_idx"* ]]; then
            echo "expected ${suffix} HNSW index scan after ${phase}, got:" >&2
            echo "${index_plan}" >&2
            exit 1
        fi
        printf 'pgvector_hnsw_lifecycle_metric: %s:%s\n' "${suffix}" "${phase}"
    done
    printf 'hnsw_restart_index_scan: %s\n' "${phase}"
    printf 'hnsw_restart_nearest_rechecked: %s\n' "${phase}"
}

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.restart_hnsw_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    body text NOT NULL
);

INSERT INTO public.restart_hnsw_docs (id, embedding, body)
VALUES
    (1, '[1,0]'::vector, 'before restart one'),
    (2, '[0,1]'::vector, 'before restart two'),
    (9, '[3,3]'::vector, 'before restart nine'),
    (10, '[3,3]'::vector, 'tie ten');

CREATE INDEX restart_hnsw_docs_l2_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
CREATE INDEX restart_hnsw_docs_ip_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ip_ops);
CREATE INDEX restart_hnsw_docs_cosine_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops);
CREATE INDEX restart_hnsw_docs_l1_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_l1_ops);

INSERT INTO public.restart_hnsw_docs VALUES (11, '[4,4]'::vector, 'inserted');
UPDATE public.restart_hnsw_docs SET embedding = '[5,5]'::vector WHERE id = 2;
DELETE FROM public.restart_hnsw_docs WHERE id = 1;
VACUUM (ANALYZE) public.restart_hnsw_docs;
REINDEX TABLE public.restart_hnsw_docs;

CHECKPOINT;
SQL

validate_hnsw_order "before_restart"
cargo pgrx stop "${PG_VERSION}"
cargo pgrx start "${PG_VERSION}"
validate_hnsw_order "after_restart"
