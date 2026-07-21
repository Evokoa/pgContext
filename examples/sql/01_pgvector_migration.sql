CREATE EXTENSION IF NOT EXISTS pgcontext;

DROP TABLE IF EXISTS public.example_pgvector_docs;
CREATE TABLE public.example_pgvector_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    body text NOT NULL
);

INSERT INTO public.example_pgvector_docs (id, embedding, body) VALUES
    (1, '[1,0,0]'::vector, 'postgres vector migration'),
    (2, '[0,1,0]'::vector, 'hybrid search with text'),
    (3, '[0,0,1]'::vector, 'recall validation example');

SELECT pgcontext.create_collection('example_pgvector_docs', 'public.example_pgvector_docs');
SELECT pgcontext.register_vector('example_pgvector_docs', 'embedding', 'embedding', 3, 'cosine');
SELECT pgcontext.upsert_points('example_pgvector_docs', ARRAY['1', '2', '3']);

SELECT source_key, score
FROM pgcontext.search('example_pgvector_docs', '[1,0,0]'::vector, 3);

SELECT id,
       pgcontext.cosine_distance(embedding, '[1,0,0]'::vector) AS score
FROM public.example_pgvector_docs
ORDER BY embedding OPERATOR(pgcontext.<=>) '[1,0,0]'::vector;

WITH exact AS (
    SELECT array_agg(point_id ORDER BY score, point_id) AS point_ids
    FROM pgcontext.search('example_pgvector_docs', '[1,0,0]'::vector, 3)
)
SELECT *
FROM pgcontext.recall_check(
    (SELECT point_ids FROM exact),
    (SELECT point_ids FROM exact),
    1.0
);
