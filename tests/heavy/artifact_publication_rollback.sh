#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_artifact_publication_rollback}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;
CREATE TABLE public.artifact_rollback_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL
);
INSERT INTO public.artifact_rollback_docs VALUES
    (10, '[1,0]'::vector),
    (20, '[0,1]'::vector);
SELECT pgcontext.create_collection('artifact_rollback_docs', 'public.artifact_rollback_docs');
SELECT pgcontext.register_vector('artifact_rollback_docs', 'embedding', 'embedding', 2, 'l2');
SELECT pgcontext.upsert_points('artifact_rollback_docs', ARRAY['10', '20']);
SELECT build_job_id AS build_job_id
  FROM pgcontext.start_build_job(
       'artifact_rollback_docs', 'mmap', 'view-a',
       'public.artifact_rollback_docs', 2
  ) \gset
SELECT pgcontext.run_build_job(:build_job_id, 2);
BEGIN;
SELECT * FROM pgcontext.publish_artifact_segment_file(
    :build_job_id,
    pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
);
ROLLBACK;
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM pgcontext.artifact_segments('artifact_rollback_docs')
    ) THEN
        RAISE EXCEPTION 'rollback left a visible artifact generation';
    END IF;
END
$$;
SELECT * FROM pgcontext.cleanup_artifact_segments('artifact_rollback_docs', false);
SELECT * FROM pgcontext.publish_artifact_segment_file(
    :build_job_id,
    pgcontext.encode_artifact_segment('hnsw_graph', decode('0102', 'hex'))
);
DO $$
BEGIN
    IF (SELECT count(*) FROM pgcontext.artifact_segments('artifact_rollback_docs')) <> 1 THEN
        RAISE EXCEPTION 'committed publication did not produce exactly one visible artifact';
    END IF;
    IF NOT EXISTS (
        SELECT 1
          FROM pgcontext.artifact_segment_mmap_payload(
               'artifact_rollback_docs', 'view-a', 4096
          )
    ) THEN
        RAISE EXCEPTION 'committed publication is not mmap-serving-ready';
    END IF;
END
$$;
SQL

printf 'artifact_publication_rollback_invisible\n'
printf 'artifact_publication_orphan_reconciled\n'
printf 'artifact_publication_commit_serving_ready\n'
