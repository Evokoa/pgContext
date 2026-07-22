CREATE EXTENSION IF NOT EXISTS pgcontext;

DROP TABLE IF EXISTS public.example_named_vectors;
CREATE TABLE public.example_named_vectors (
    id bigint PRIMARY KEY,
    title_embedding pgcontext.vector NOT NULL,
    body_embedding pgcontext.vector NOT NULL,
    sparse_terms pgcontext.sparsevec
);

INSERT INTO public.example_named_vectors (id, title_embedding, body_embedding, sparse_terms) VALUES
    (1, '[1,0,0]'::pgcontext.vector, '[0.9,0.1,0]'::pgcontext.vector, pgcontext.sparsevec('{1:0.8,3:0.2}/10')),
    (2, '[0,1,0]'::pgcontext.vector, '[0.1,0.8,0.1]'::pgcontext.vector, pgcontext.sparsevec('{2:0.7,4:0.4}/10'));

SELECT pgcontext.create_collection('example_named_vectors', 'public.example_named_vectors');

SELECT pgcontext.register_vector('example_named_vectors', 'title', 'title_embedding', 3, 'cosine');
SELECT pgcontext.register_sparse_vector('example_named_vectors', 'lexical', 'sparse_terms', 10, 'inner_product');

SELECT pgcontext.configure_sparse_vector(
    'example_named_vectors',
    'lexical',
    '{"format":"application_owned_column"}'::jsonb,
    '{"strategy":"exact"}'::jsonb,
    'ready'
);

SELECT pgcontext.upsert_points('example_named_vectors', ARRAY['1', '2']);

SELECT source_key, score
FROM pgcontext.search('example_named_vectors', 'title', '[1,0,0]'::pgcontext.vector, 2);

SELECT source_key, score
FROM pgcontext.search_sparse(
    'example_named_vectors',
    'lexical',
    pgcontext.sparsevec('{1:1}/10'),
    2
);

SELECT source_key, score
FROM pgcontext.query(
    'example_named_vectors',
    '[1,0,0]'::pgcontext.vector,
    'lexical',
    pgcontext.sparsevec('{1:1}/10'),
    2
);

SELECT pgcontext.register_vector('example_named_vectors', 'body', 'body_embedding', 3, 'cosine');

SELECT source_key, score
FROM pgcontext.search('example_named_vectors', 'body', '[1,0,0]'::pgcontext.vector, 2);
