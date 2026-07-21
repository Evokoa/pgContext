CREATE EXTENSION IF NOT EXISTS pgcontext;

DROP TABLE IF EXISTS public.example_hybrid_docs;
CREATE TABLE public.example_hybrid_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    tenant_id text NOT NULL,
    body text NOT NULL
);

INSERT INTO public.example_hybrid_docs (id, embedding, tenant_id, body) VALUES
    (1, '[1,0]'::vector, 'acme', 'postgres vector search'),
    (2, '[0,1]'::vector, 'acme', 'rust extension guide'),
    (3, '[4,0]'::vector, 'other', 'tenant isolation diagnostics'),
    (4, '[0,4]'::vector, 'acme', 'recommendation and discovery');

SELECT pgcontext.create_collection('example_hybrid_docs', 'public.example_hybrid_docs');
SELECT pgcontext.register_vector('example_hybrid_docs', 'embedding', 'embedding', 2, 'l2');
SELECT pgcontext.register_filter_column('example_hybrid_docs', 'tenant_id', 'tenant_id');
SELECT pgcontext.upsert_points('example_hybrid_docs', ARRAY['1', '2', '3', '4']);

SELECT source_key, score
FROM pgcontext.query('example_hybrid_docs', '[1,0]'::vector, 'postgres search', 'body', 3);

SELECT source_key, score
FROM pgcontext.recommend(
    'example_hybrid_docs',
    ARRAY(SELECT point_id FROM pgcontext.search('example_hybrid_docs', '[1,0]'::vector, 1)),
    ARRAY[]::bigint[],
    3
);

SELECT source_key, score
FROM pgcontext.discover(
    'example_hybrid_docs',
    ARRAY(SELECT point_id FROM pgcontext.search('example_hybrid_docs', '[1,0]'::vector, 1)),
    3
);

SELECT pgcontext.record_query_stat(
    'example_hybrid_docs',
    'tenant:acme',
    'search_filtered',
    2,
    4,
    18.5
);

SELECT cohort, query_kind, avg_latency_ms, latency_bucket, total_candidates
FROM pgcontext.query_cohort_stats()
WHERE collection_name = 'example_hybrid_docs';

SELECT pgcontext.query_rerank(
    pgcontext.query_prefetch(ARRAY[
        pgcontext.query_weight(pgcontext.query_nearest('[1,0]'::vector, 20), 0.8),
        pgcontext.query_formula(pgcontext.query_discover(ARRAY[1], 10), '$score * 0.5')
    ]),
    5
);
