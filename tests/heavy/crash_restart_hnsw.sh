#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_crash_restart_hnsw}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

validate_hnsw_order() {
    local phase="$1"
    local row suffix column operator query index_name
    local index_order exact_order index_plan
    local -a cases=(
        "l2|embedding|<->|'[1,1]'::vector|restart_hnsw_docs_l2_idx"
        "ip|embedding|<#>|'[1,1]'::vector|restart_hnsw_docs_ip_idx"
        "cosine|embedding|<=>|'[1,1]'::vector|restart_hnsw_docs_cosine_idx"
        "l1|embedding|<+>|'[1,1]'::vector|restart_hnsw_docs_l1_idx"
        "half_l2|half_value|<->|pgcontext.halfvec('[1,1]')|restart_hnsw_docs_half_l2_idx"
        "half_ip|half_value|<#>|pgcontext.halfvec('[1,1]')|restart_hnsw_docs_half_ip_idx"
        "half_cosine|half_value|<=>|pgcontext.halfvec('[1,1]')|restart_hnsw_docs_half_cosine_idx"
        "half_l1|half_value|<+>|pgcontext.halfvec('[1,1]')|restart_hnsw_docs_half_l1_idx"
        "sparse_l2|sparse_value|<->|pgcontext.sparsevec('{1:1,2:1}/2')|restart_hnsw_docs_sparse_l2_idx"
        "sparse_ip|sparse_value|<#>|pgcontext.sparsevec('{1:1,2:1}/2')|restart_hnsw_docs_sparse_ip_idx"
        "sparse_cosine|sparse_value|<=>|pgcontext.sparsevec('{1:1,2:1}/2')|restart_hnsw_docs_sparse_cosine_idx"
        "sparse_l1|sparse_value|<+>|pgcontext.sparsevec('{1:1,2:1}/2')|restart_hnsw_docs_sparse_l1_idx"
        "bit_hamming|bit_value|<~>|pgcontext.bitvec('10')|restart_hnsw_docs_bit_hamming_idx"
        "bit_jaccard|bit_value|<%>|pgcontext.bitvec('10')|restart_hnsw_docs_bit_jaccard_idx"
    )

    for row in "${cases[@]}"; do
        IFS='|' read -r suffix column operator query index_name <<<"${row}"
        index_order="$(psql_db -Atc "
            SET enable_seqscan = off;
            SET enable_bitmapscan = off;
            SELECT array_agg(id)
              FROM (
                   SELECT id
                     FROM public.restart_hnsw_docs
                    ORDER BY ${column} OPERATOR(pgcontext.${operator}) ${query}, id
                    LIMIT 10
              ) ordered" | tail -n 1)"
        exact_order="$(psql_db -Atc "
            SET enable_indexscan = off;
            SET enable_bitmapscan = off;
            SELECT array_agg(id)
              FROM (
                   SELECT id
                     FROM public.restart_hnsw_docs
                    ORDER BY ${column} OPERATOR(pgcontext.${operator}) ${query}, id
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
             ORDER BY ${column} OPERATOR(pgcontext.${operator}) ${query}
             LIMIT 1")"
        if [[ "${index_plan}" != *"Index Scan using ${index_name}"* ]]; then
            echo "expected ${suffix} HNSW index scan after ${phase}, got:" >&2
            echo "${index_plan}" >&2
            exit 1
        fi
        printf 'pgvector_hnsw_lifecycle_metric: %s:%s\n' "${suffix}" "${phase}"
    done
    printf 'hnsw_restart_index_scan: %s\n' "${phase}"
    printf 'hnsw_restart_nearest_rechecked: %s\n' "${phase}"
}

validate_mapped_attach() {
    local phase="$1"
    local mapped_attaches

    mapped_attaches="$(psql_db -At <<'SQL' | tail -n 1
SET enable_seqscan = off;
SET enable_bitmapscan = off;
SELECT id
  FROM public.restart_hnsw_docs
 ORDER BY embedding OPERATOR(pgcontext.<->) '[1,1]'::vector, id
 LIMIT 1;
SELECT mapped_attaches
  FROM pgcontext.hnsw_serving_stats();
SQL
)"
    if [[ ! "${mapped_attaches}" =~ ^[1-9][0-9]*$ ]]; then
        echo "expected mapped HNSW attachment after ${phase}, got: ${mapped_attaches}" >&2
        exit 1
    fi
    printf 'hnsw_mapped_attach: %s\n' "${phase}"
}

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.restart_hnsw_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    half_value halfvec NOT NULL,
    sparse_value sparsevec NOT NULL,
    bit_value bitvec NOT NULL,
    body text NOT NULL
);

INSERT INTO public.restart_hnsw_docs
VALUES
    (1, '[1,0]'::vector, '[1,0]'::halfvec, '{1:1}/2'::sparsevec, '10'::bitvec, 'before restart one'),
    (2, '[0,1]'::vector, '[0,1]'::halfvec, '{2:1}/2'::sparsevec, '01'::bitvec, 'before restart two'),
    (9, '[3,3]'::vector, '[3,3]'::halfvec, '{1:3,2:3}/2'::sparsevec, '11'::bitvec, 'before restart nine'),
    (10, '[3,3]'::vector, '[3,3]'::halfvec, '{1:3,2:3}/2'::sparsevec, '11'::bitvec, 'tie ten');

CREATE INDEX restart_hnsw_docs_l2_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
CREATE INDEX restart_hnsw_docs_ip_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ip_ops);
CREATE INDEX restart_hnsw_docs_cosine_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops);
CREATE INDEX restart_hnsw_docs_l1_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_l1_ops);

CREATE INDEX restart_hnsw_docs_half_l2_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (half_value pgcontext.halfvec_hnsw_ops);
CREATE INDEX restart_hnsw_docs_half_ip_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (half_value pgcontext.halfvec_hnsw_ip_ops);
CREATE INDEX restart_hnsw_docs_half_cosine_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (half_value pgcontext.halfvec_hnsw_cosine_ops);
CREATE INDEX restart_hnsw_docs_half_l1_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (half_value pgcontext.halfvec_hnsw_l1_ops);
CREATE INDEX restart_hnsw_docs_sparse_l2_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (sparse_value pgcontext.sparsevec_hnsw_ops);
CREATE INDEX restart_hnsw_docs_sparse_ip_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (sparse_value pgcontext.sparsevec_hnsw_ip_ops);
CREATE INDEX restart_hnsw_docs_sparse_cosine_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (sparse_value pgcontext.sparsevec_hnsw_cosine_ops);
CREATE INDEX restart_hnsw_docs_sparse_l1_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (sparse_value pgcontext.sparsevec_hnsw_l1_ops);
CREATE INDEX restart_hnsw_docs_bit_hamming_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (bit_value pgcontext.bitvec_hnsw_hamming_ops);
CREATE INDEX restart_hnsw_docs_bit_jaccard_idx
    ON public.restart_hnsw_docs USING pgcontext_hnsw (bit_value pgcontext.bitvec_hnsw_jaccard_ops);

INSERT INTO public.restart_hnsw_docs VALUES
    (11, '[4,4]'::vector, '[4,4]'::halfvec, '{1:4,2:4}/2'::sparsevec, '00'::bitvec, 'inserted');
UPDATE public.restart_hnsw_docs
   SET embedding = '[5,5]'::vector,
       half_value = '[5,5]'::halfvec,
       sparse_value = '{1:5,2:5}/2'::sparsevec,
       bit_value = '10'::bitvec
 WHERE id = 2;
DELETE FROM public.restart_hnsw_docs WHERE id = 1;
VACUUM (ANALYZE) public.restart_hnsw_docs;
REINDEX TABLE public.restart_hnsw_docs;

CHECKPOINT;
SQL

validate_hnsw_order "before_restart"
validate_mapped_attach "before_restart"
PGRX_DATA_DIR="$(psql_db -Atc 'SHOW data_directory' | tail -n 1)"
PG_CTL="$(pg_bin pg_ctl)"
"${PG_CTL}" -D "${PGRX_DATA_DIR}" stop -m immediate
cargo pgrx start "${PG_VERSION}"
validate_hnsw_order "after_restart"
validate_mapped_attach "after_restart"
