CREATE EXTENSION IF NOT EXISTS pgcontext;

DROP TABLE IF EXISTS public.example_model_docs_v1;
DROP TABLE IF EXISTS public.example_model_docs_v2;

CREATE TABLE public.example_model_docs_v1 (
    id bigint PRIMARY KEY,
    embedding pgcontext.vector(4) NOT NULL
);

CREATE TABLE public.example_model_docs_v2 (
    id bigint PRIMARY KEY,
    embedding pgcontext.vector(4) NOT NULL
);

INSERT INTO public.example_model_docs_v1 (id, embedding) VALUES
    (1, '[0.1,0.2,0.3,0.4]'::pgcontext.vector),
    (2, '[0.2,0.1,0.5,0.7]'::pgcontext.vector);

INSERT INTO public.example_model_docs_v2 (id, embedding)
SELECT id, embedding FROM public.example_model_docs_v1;

SELECT pgcontext.create_collection('example_model_docs_v1', 'public.example_model_docs_v1');
SELECT pgcontext.create_collection('example_model_docs_v2', 'public.example_model_docs_v2');
SELECT pgcontext.register_vector('example_model_docs_v1', 'embedding', 'embedding', 4, 'cosine');
SELECT pgcontext.register_vector('example_model_docs_v2', 'embedding', 'embedding', 4, 'cosine');
SELECT pgcontext.upsert_points('example_model_docs_v1', ARRAY['1', '2']);
SELECT pgcontext.backfill_points('example_model_docs_v2', 100);

CREATE INDEX example_model_docs_v1_hnsw ON public.example_model_docs_v1
    USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops);
CREATE INDEX example_model_docs_v2_hnsw ON public.example_model_docs_v2
    USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops);
SELECT pgcontext.register_embedding_profile(
    'example_model_docs_v1', 'embedder_v1', 'embedding',
    'public.example_model_docs_v1_hnsw',
    jsonb_build_object(
        'representation', 'dense', 'dimensions', 4, 'normalization', 'none',
        'metric', 'cosine', 'provider', 'example', 'model', 'embedder',
        'revision', 'v1', 'input_template', '{text}', 'output_template', '{vector}',
        'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
        'configuration_hash', '0123456789abcdef'
    )
);
SELECT pgcontext.register_embedding_profile(
    'example_model_docs_v2', 'embedder_v2', 'embedding',
    'public.example_model_docs_v2_hnsw',
    jsonb_build_object(
        'representation', 'dense', 'dimensions', 4, 'normalization', 'none',
        'metric', 'cosine', 'provider', 'example', 'model', 'embedder',
        'revision', 'v2', 'input_template', '{text}', 'output_template', '{vector}',
        'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
        'configuration_hash', 'fedcba9876543210'
    )
);
SELECT pgcontext.create_collection_alias('example_model_docs_live', 'example_model_docs_v1');
SELECT pgcontext.create_collection_alias('example_model_docs_live', 'example_model_docs_v2');

SELECT pgcontext.binary_quantize('[0.1,0.2,0.3,0.4]'::pgcontext.vector) AS binary_codes;
SELECT pgcontext.scalar_quantize('[0.1,0.2,0.3,0.4]'::pgcontext.vector, 0.0, 1.0, 256) AS sq8_codes;

SELECT source_key, score
FROM pgcontext.search('example_model_docs_live', '[0.1,0.2,0.3,0.4]'::pgcontext.vector, 2);
