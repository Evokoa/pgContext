\set ON_ERROR_STOP on
\pset pager off

CREATE EXTENSION IF NOT EXISTS pgcontext;

SELECT pgcontext.drop_collection('playground_docs');
DROP TABLE IF EXISTS public.pgcontext_playground_docs CASCADE;
CREATE TABLE public.pgcontext_playground_docs (
    id text PRIMARY KEY,
    embedding pgcontext.vector(3) NOT NULL,
    category text NOT NULL,
    metadata jsonb NOT NULL,
    body text NOT NULL
);

INSERT INTO public.pgcontext_playground_docs VALUES
    ('postgres', '[1,0,0]', 'database', '{"language":"sql"}', 'PostgreSQL internals and indexing'),
    ('rust',     '[0.8,0.2,0]', 'systems', '{"language":"rust"}', 'Rust extension development'),
    ('vectors',  '[0.7,0.1,0.2]', 'database', '{"language":"sql"}', 'Vector retrieval in PostgreSQL'),
    ('garden',   '[0,0,1]', 'other', '{"language":"english"}', 'Seasonal garden notes');

SELECT * FROM pgcontext.create_collection(
    'playground_docs',
    'public.pgcontext_playground_docs'
);
SELECT pgcontext.register_vector(
    'playground_docs', 'embedding', 'embedding', 3, 'cosine'
);
SELECT pgcontext.register_filter_column(
    'playground_docs', 'category', 'category'
);
SELECT pgcontext.register_jsonb_path(
    'playground_docs', 'language', 'metadata', ARRAY['language']
);
SELECT pgcontext.upsert_points(
    'playground_docs', ARRAY['postgres', 'rust', 'vectors', 'garden']
);

CREATE INDEX pgcontext_playground_docs_hnsw
ON public.pgcontext_playground_docs
USING pgcontext_hnsw (
    embedding pgcontext.vector_hnsw_cosine_ops
);

\echo ''
\echo 'Exact collection search'
SELECT source_key, score
FROM pgcontext.search('playground_docs', '[1,0,0]'::pgcontext.vector, 4);

\echo ''
\echo 'Metadata-filtered search'
SELECT source_key, score
FROM pgcontext.search(
    'playground_docs',
    '[1,0,0]'::pgcontext.vector,
    '{"must":[{"key":"category","match":"database"}]}',
    4
);

\echo ''
\echo 'Persisted HNSW ordered scan'
SET enable_seqscan = off;
SELECT id, embedding OPERATOR(pgcontext.<=>) '[1,0,0]'::pgcontext.vector AS distance
FROM public.pgcontext_playground_docs
ORDER BY embedding OPERATOR(pgcontext.<=>) '[1,0,0]'::pgcontext.vector
LIMIT 3;
RESET enable_seqscan;
