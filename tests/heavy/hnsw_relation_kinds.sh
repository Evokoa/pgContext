#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_hnsw_relation_kinds}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.logged_hnsw_docs (id bigint PRIMARY KEY, embedding vector NOT NULL);
CREATE UNLOGGED TABLE public.unlogged_hnsw_docs (id bigint PRIMARY KEY, embedding vector NOT NULL);
INSERT INTO public.logged_hnsw_docs VALUES (1, '[1,0]'::vector), (9, '[9,0]'::vector);
INSERT INTO public.unlogged_hnsw_docs VALUES (1, '[1,0]'::vector), (9, '[9,0]'::vector);
CREATE INDEX logged_hnsw_docs_embedding_idx ON public.logged_hnsw_docs USING pgcontext_hnsw (embedding);
CREATE INDEX unlogged_hnsw_docs_embedding_idx ON public.unlogged_hnsw_docs USING pgcontext_hnsw (embedding);

DO $$
DECLARE
    persistence "char";
    nearest bigint;
BEGIN
    SELECT relpersistence INTO persistence FROM pg_class WHERE oid = 'public.logged_hnsw_docs_embedding_idx'::regclass;
    IF persistence <> 'p' THEN RAISE EXCEPTION 'logged HNSW index persistence was %, expected p', persistence; END IF;
    SELECT relpersistence INTO persistence FROM pg_class WHERE oid = 'public.unlogged_hnsw_docs_embedding_idx'::regclass;
    IF persistence <> 'u' THEN RAISE EXCEPTION 'unlogged HNSW index persistence was %, expected u', persistence; END IF;
    SET LOCAL enable_seqscan = off;
    SELECT id INTO nearest FROM public.logged_hnsw_docs ORDER BY embedding OPERATOR(pgcontext.<->) '[9,0]'::vector LIMIT 1;
    IF nearest <> 9 THEN RAISE EXCEPTION 'logged HNSW nearest was %', nearest; END IF;
    SELECT id INTO nearest FROM public.unlogged_hnsw_docs ORDER BY embedding OPERATOR(pgcontext.<->) '[9,0]'::vector LIMIT 1;
    IF nearest <> 9 THEN RAISE EXCEPTION 'unlogged HNSW nearest was %', nearest; END IF;
END
$$;

CREATE TEMPORARY TABLE temp_hnsw_docs (id bigint PRIMARY KEY, embedding vector NOT NULL) ON COMMIT PRESERVE ROWS;
INSERT INTO temp_hnsw_docs VALUES (1, '[1,0]'::vector), (9, '[9,0]'::vector);
CREATE INDEX temp_hnsw_docs_embedding_idx ON temp_hnsw_docs USING pgcontext_hnsw (embedding);
DO $$
DECLARE
    persistence "char";
    nearest bigint;
BEGIN
    SELECT relpersistence INTO persistence FROM pg_class WHERE oid = 'temp_hnsw_docs_embedding_idx'::regclass;
    IF persistence <> 't' THEN RAISE EXCEPTION 'temporary HNSW index persistence was %, expected t', persistence; END IF;
    SET LOCAL enable_seqscan = off;
    SELECT id INTO nearest FROM temp_hnsw_docs ORDER BY embedding OPERATOR(pgcontext.<->) '[9,0]'::vector LIMIT 1;
    IF nearest <> 9 THEN RAISE EXCEPTION 'temporary HNSW nearest was %', nearest; END IF;
END
$$;
SQL

printf 'hnsw_relation_kinds_logged_unlogged_temporary: passed\n'
