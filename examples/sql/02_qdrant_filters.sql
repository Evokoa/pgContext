CREATE EXTENSION IF NOT EXISTS pgcontext;

DROP TABLE IF EXISTS public.example_filter_docs;
CREATE TABLE public.example_filter_docs (
    id bigint PRIMARY KEY,
    embedding pgcontext.vector NOT NULL,
    tenant_id text NOT NULL,
    status text,
    metadata jsonb NOT NULL
);

INSERT INTO public.example_filter_docs (id, embedding, tenant_id, status, metadata) VALUES
    (10, '[1,0]'::pgcontext.vector, 'acme', 'published', '{"topic":"postgres","tier":"gold"}'),
    (20, '[2,0]'::pgcontext.vector, 'acme', 'draft', '{"topic":"rust","tier":"gold"}'),
    (30, '[0,2]'::pgcontext.vector, 'other', 'published', '{"topic":"postgres","tier":"silver"}');

SELECT pgcontext.create_collection('example_filter_docs', 'public.example_filter_docs');
SELECT pgcontext.register_vector('example_filter_docs', 'embedding', 'embedding', 2, 'l2');
SELECT pgcontext.register_filter_column('example_filter_docs', 'tenant_id', 'tenant_id');
SELECT pgcontext.register_filter_column('example_filter_docs', 'status', 'status');
SELECT pgcontext.register_jsonb_path('example_filter_docs', 'topic', 'metadata', ARRAY['topic']);
SELECT pgcontext.bulk_upsert_points('example_filter_docs', ARRAY['10', '20', '30'], 2);

SELECT source_key, score
FROM pgcontext.search(
    'example_filter_docs',
    '[0,0]'::pgcontext.vector,
    '{"must":[{"key":"tenant_id","match":"acme"},{"key":"topic","match":"postgres"}]}',
    10
);

SELECT pgcontext.count(
    'example_filter_docs',
    '{"must":[{"key":"tenant_id","match":"acme"}]}'
);

SELECT value, count
FROM pgcontext.facet('example_filter_docs', 'topic', NULL, 10);

SELECT group_value, source_key, score
FROM pgcontext.grouped_search('example_filter_docs', '[0,0]'::pgcontext.vector, 'tenant_id', 1, 10);

SELECT source_key, updated
FROM pgcontext.set_payload(
    'example_filter_docs',
    ARRAY['10'],
    '{"status":"archived","topic":"database"}'::jsonb
);

SELECT batch_number, processed_count, deleted_count, missing_count
FROM pgcontext.bulk_delete_points('example_filter_docs', ARRAY['20', 'missing'], 100);
