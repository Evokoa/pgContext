CREATE EXTENSION IF NOT EXISTS pgcontext;

DROP TABLE IF EXISTS public.example_model_docs_v1;
DROP TABLE IF EXISTS public.example_model_docs_v2;

CREATE TABLE public.example_model_docs_v1 (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL
);

CREATE TABLE public.example_model_docs_v2 (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL
);

INSERT INTO public.example_model_docs_v1 (id, embedding) VALUES
    (1, '[0.1,0.2,0.3,0.4]'::vector),
    (2, '[0.2,0.1,0.5,0.7]'::vector);

INSERT INTO public.example_model_docs_v2 (id, embedding)
SELECT id, embedding FROM public.example_model_docs_v1;

SELECT pgcontext.create_collection('example_model_docs_v1', 'public.example_model_docs_v1');
SELECT pgcontext.create_collection('example_model_docs_v2', 'public.example_model_docs_v2');
SELECT pgcontext.register_vector('example_model_docs_v1', 'embedding', 'embedding', 4, 'cosine');
SELECT pgcontext.register_vector('example_model_docs_v2', 'embedding', 'embedding', 4, 'cosine');
SELECT pgcontext.upsert_points('example_model_docs_v1', ARRAY['1', '2']);
SELECT pgcontext.backfill_points('example_model_docs_v2', 100);

SELECT pgcontext.register_model_version('example_model_docs_v1', 'embedder', 'v1', 4, 'cosine');
SELECT pgcontext.register_model_version('example_model_docs_v2', 'embedder', 'v2', 4, 'cosine');
SELECT pgcontext.create_collection_alias('example_model_docs_live', 'example_model_docs_v1');
SELECT pgcontext.create_collection_alias('example_model_docs_live', 'example_model_docs_v2');

SELECT pgcontext.binary_quantize('[0.1,0.2,0.3,0.4]'::vector) AS binary_codes;
SELECT pgcontext.scalar_quantize('[0.1,0.2,0.3,0.4]'::vector, 0.0, 1.0, 256) AS sq8_codes;

SELECT source_key, score
FROM pgcontext.search('example_model_docs_live', '[0.1,0.2,0.3,0.4]'::vector, 2);
