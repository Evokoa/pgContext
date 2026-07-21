#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_filtered_ann_recall}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.filtered_hnsw_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    tenant text NOT NULL
);

INSERT INTO public.filtered_hnsw_docs (id, embedding, tenant)
SELECT value,
       format('[%s,0]', value)::vector,
       CASE WHEN value BETWEEN 10 AND 19 THEN 'acme' ELSE 'other' END
  FROM generate_series(1, 40) AS value;

CREATE INDEX filtered_hnsw_docs_embedding_idx
    ON public.filtered_hnsw_docs USING pgcontext_hnsw (embedding);

SELECT pgcontext.create_collection('filtered_hnsw_docs', 'public.filtered_hnsw_docs');
SELECT pgcontext.register_vector('filtered_hnsw_docs', 'embedding', 'embedding', 2, 'l2');
SELECT pgcontext.attach_hnsw_index(
    'filtered_hnsw_docs',
    'embedding',
    'public.filtered_hnsw_docs_embedding_idx'
);
SELECT pgcontext.register_filter_column('filtered_hnsw_docs', 'tenant', 'tenant');
SELECT pgcontext.upsert_points(
    'filtered_hnsw_docs',
    ARRAY(SELECT id::text FROM public.filtered_hnsw_docs ORDER BY id)
);

DO $$
DECLARE
    exact_ids bigint[];
    public_ids bigint[];
    public_count bigint;
    matching_count bigint;
    no_match_count bigint;
    page_visits bigint;
    node_reads bigint;
    candidate_count bigint;
    exact_strategy boolean;
    recall_value double precision;
    recall_status text;
BEGIN
    -- Force several bounded candidate masks: no single HNSW traversal receives
    -- more than two source heap TIDs, while the public query must still fill k.
    SET LOCAL pgcontext.hnsw_candidate_budget = 2;
    SET LOCAL pgcontext.hnsw_iterative_expansion_limit = 10;

    SELECT array_agg(id ORDER BY embedding OPERATOR(pgcontext.<->) '[12,0]'::vector, id)
      INTO exact_ids
      FROM (
          SELECT id, embedding
            FROM public.filtered_hnsw_docs
           WHERE tenant = 'acme'
           ORDER BY embedding OPERATOR(pgcontext.<->) '[12,0]'::vector, id
           LIMIT 5
      ) exact_rows;

    SELECT array_agg(source_key::bigint ORDER BY ordinal)
      INTO public_ids
      FROM (
          SELECT row_number() OVER () AS ordinal, source_key
            FROM pgcontext.search(
                'filtered_hnsw_docs',
                '[12,0]'::vector,
                '{"must":[{"key":"tenant","match":"acme"}]}',
                5
            )
      ) public_rows;

    IF exact_ids IS NULL OR public_ids IS NULL OR exact_ids <> public_ids THEN
        RAISE EXCEPTION 'public filtered HNSW search did not match exact ids: exact %, public %',
            exact_ids, public_ids;
    END IF;

    SELECT recall, status::text
      INTO recall_value, recall_status
      FROM pgcontext.recall_check(exact_ids, public_ids, 1.0);
    IF recall_status <> 'Passing' OR recall_value <> 1.0 THEN
        RAISE EXCEPTION 'public filtered HNSW recall failed: status %, recall %',
            recall_status, recall_value;
    END IF;

    SELECT count(*) INTO matching_count
      FROM unnest(public_ids) AS candidate_id
      JOIN public.filtered_hnsw_docs AS docs ON docs.id = candidate_id
     WHERE docs.tenant = 'acme';
    IF matching_count <> array_length(public_ids, 1) THEN
        RAISE EXCEPTION 'public filtered HNSW results escaped tenant recheck: results %, matching %',
            array_length(public_ids, 1), matching_count;
    END IF;

    SELECT count(*) INTO public_count
      FROM pgcontext.search(
          'filtered_hnsw_docs',
          '[12,0]'::vector,
          '{"must":[{"key":"tenant","match":"acme"}]}',
          5
      );
    IF public_count <> 5 THEN
        RAISE EXCEPTION 'public filtered HNSW search did not fill k after bounded masks: %', public_count;
    END IF;

    SELECT work.page_visits, work.node_reads, work.candidates, work.exact_strategy
      INTO page_visits, node_reads, candidate_count, exact_strategy
      FROM pgcontext.hnsw_last_scan_work() AS work;
    IF page_visits <= 0 OR node_reads <= 0 OR candidate_count <= 0 OR exact_strategy THEN
        RAISE EXCEPTION 'public filtered search did not report persisted masked HNSW work: pages %, nodes %, candidates %, exact %',
            page_visits, node_reads, candidate_count, exact_strategy;
    END IF;

    SELECT count(*)
      INTO no_match_count
      FROM pgcontext.search(
          'filtered_hnsw_docs',
          '[12,0]'::vector,
          '{"must":[{"key":"tenant","match":"missing"}]}',
          5
      );
    IF no_match_count <> 0 THEN
        RAISE EXCEPTION 'public no-match filtered HNSW query returned rows: %', no_match_count;
    END IF;
END
$$;
SQL

# These remain separate, independently runnable heavy gates: the first covers
# collection authorization and RLS, and the second covers partition lifecycle.
test -f "${SCRIPT_DIR}/rls_acl_boundary.sh"
test -f "${SCRIPT_DIR}/partitioned_collections.sh"

printf 'filtered_ann_public_exact_oracle_verified\n'
printf 'filtered_ann_public_masked_hnsw_verified\n'
printf 'filtered_ann_public_iterative_fill_verified\n'
printf 'filtered_ann_public_recall_passing\n'
printf 'filtered_ann_public_tenant_recheck_passing\n'
printf 'filtered_ann_public_no_match_empty\n'
printf 'filtered_ann_rls_acl_boundary_retained\n'
printf 'filtered_ann_partitioned_collections_retained\n'
