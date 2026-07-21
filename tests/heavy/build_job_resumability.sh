#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_build_job_resumability}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

build_hnsw_payload_hex() {
    local build_job_id="$1"
    psql_db -Atc "SELECT encode(pgcontext.build_mmap_hnsw_artifact(${build_job_id}), 'hex')"
}

validate_mmap_search_order() {
    local artifact_name="$1"
    local expected="$2"
    local label="$3"
    local ordered_keys

    ordered_keys="$(psql_db -At <<SQL | tail -n 1
SELECT string_agg(source_key, ',' ORDER BY score, point_id)
  FROM pgcontext.search_mmap_hnsw_artifact(
       'build_resume_docs',
       '${artifact_name}',
       '[0,0]'::vector,
       4096,
       3,
       3
  );
SQL
)"
    if [[ "${ordered_keys}" != "${expected}" ]]; then
        echo "unexpected mmap source-table recheck order for ${artifact_name}: ${ordered_keys}" >&2
        exit 1
    fi
    printf '%s\n' "${label}"
}

validate_serving_ready() {
    local artifact_name="$1"
    local label="$2"
    local readiness

    readiness="$(psql_db -At <<SQL | tail -n 1
SELECT status || ':' || serving_ready::text
  FROM pgcontext.artifact_segment_serving_readiness('build_resume_docs', 4096)
 WHERE artifact_kind = 'mmap'
   AND artifact_name = '${artifact_name}';
SQL
)"
    if [[ "${readiness}" != "ready:true" ]]; then
        echo "expected serving-ready artifact ${artifact_name}, got: ${readiness}" >&2
        exit 1
    fi
    printf '%s\n' "${label}"
}

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.build_resume_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    body text NOT NULL
);

INSERT INTO public.build_resume_docs (id, embedding, body)
VALUES
    (10, '[3,0]'::vector, 'resume candidate ten'),
    (20, '[1,0]'::vector, 'resume candidate twenty'),
    (30, '[2,0]'::vector, 'resume candidate thirty');

SELECT pgcontext.create_collection('build_resume_docs', 'public.build_resume_docs');
SELECT pgcontext.register_vector(
    'build_resume_docs',
    'embedding',
    'embedding',
    2,
    'l2'
);
SELECT pgcontext.upsert_points('build_resume_docs', ARRAY['10', '20', '30']);
SQL

psql_db <<SQL
SELECT build_job_id AS view_a_job_id
  FROM pgcontext.start_build_job(
       'build_resume_docs',
       'mmap',
       'view-a',
       'public.build_resume_docs',
       3
  ) \gset
SELECT pgcontext.run_build_job(:view_a_job_id, 1);
SELECT pgcontext.delete_points('build_resume_docs', ARRAY['20']);
SELECT pgcontext.upsert_points('build_resume_docs', ARRAY['20']);
DO \$\$
DECLARE
    operations text[];
BEGIN
    SELECT array_agg(operation ORDER BY delta_sequence)
      INTO operations
      FROM pgcontext._build_deltas AS deltas
      JOIN pgcontext._build_jobs AS jobs USING (build_job_id)
     WHERE jobs.artifact_name = 'view-a';
    IF operations <> ARRAY['delete', 'upsert'] THEN
        RAISE EXCEPTION 'unexpected build delta operations: %', operations;
    END IF;
END
\$\$;
SELECT pgcontext.update_build_job(
       :view_a_job_id,
       1,
       'failed',
       'interrupted after first source batch'
);
SELECT pgcontext.retry_build_job(:view_a_job_id);

DO \$\$
DECLARE
    job record;
BEGIN
    SELECT status::text AS status,
           attempt,
           processed_units,
           total_units,
           cancel_requested,
           error_message
      INTO job
      FROM pgcontext.build_jobs('build_resume_docs')
     WHERE artifact_name = 'view-a';

    IF job.status <> 'Running'
       OR job.attempt <> 2
       OR job.processed_units <> 1
       OR job.total_units <> 3
       OR job.cancel_requested
       OR job.error_message IS NOT NULL THEN
        RAISE EXCEPTION 'unexpected retried build job state: %', job;
    END IF;
    IF (
        SELECT last_source_point_id
          FROM pgcontext._build_jobs
         WHERE artifact_name = 'view-a'
    ) <> 1 THEN
        RAISE EXCEPTION 'retry did not preserve the logical source checkpoint';
    END IF;
END
\$\$;
SELECT pgcontext.run_build_job(:view_a_job_id, 5);
DO \$\$
BEGIN
    IF EXISTS (
        SELECT 1
          FROM pgcontext._build_deltas AS deltas
          JOIN pgcontext._build_jobs AS jobs USING (build_job_id)
         WHERE jobs.artifact_name = 'view-a'
    ) THEN
        RAISE EXCEPTION 'completed build retained unreplayed deltas';
    END IF;
END
\$\$;
SELECT pgcontext.publish_artifact_segment_file(
       :view_a_job_id,
       pgcontext.build_mmap_hnsw_artifact(:view_a_job_id)
);
SQL
printf 'build_job_progress_preserved\n'
printf 'build_job_logical_source_checkpoint_preserved\n'
printf 'build_job_delta_log_replayed\n'

psql_db <<'SQL'
UPDATE public.build_resume_docs
   SET embedding = '[0,0]'::vector
 WHERE id = 30;
SQL
validate_mmap_search_order "view-a" "30,20,10" "build_job_source_recheck_after_update"

psql_db <<SQL
SELECT build_job_id AS view_b_job_id
  FROM pgcontext.start_build_job(
       'build_resume_docs',
       'mmap',
       'view-b',
       'public.build_resume_docs',
       3
  ) \gset
SELECT pgcontext.run_build_job(:view_b_job_id, 1);
CHECKPOINT;
SQL

cargo pgrx stop "${PG_VERSION}"
cargo pgrx start "${PG_VERSION}"

psql_db <<SQL
DO \$\$
DECLARE
    job record;
BEGIN
    SELECT status::text AS status,
           attempt,
           processed_units,
           total_units
      INTO job
      FROM pgcontext.build_jobs('build_resume_docs')
     WHERE artifact_name = 'view-b';

    IF job.status <> 'Abandoned'
       OR job.attempt <> 1
       OR job.processed_units <> 1
       OR job.total_units <> 3 THEN
        RAISE EXCEPTION 'unexpected abandoned build job state: %', job;
    END IF;
END
\$\$;
SELECT pgcontext.retry_build_job(
       (
         SELECT build_job_id
           FROM pgcontext.build_jobs('build_resume_docs')
          WHERE artifact_name = 'view-b'
       )
);
DO \$\$
DECLARE
    job record;
BEGIN
    SELECT status::text AS status,
           attempt,
           processed_units
      INTO job
      FROM pgcontext.build_jobs('build_resume_docs')
     WHERE artifact_name = 'view-b';

    IF job.status <> 'Running'
       OR job.attempt <> 2
       OR job.processed_units <> 1 THEN
        RAISE EXCEPTION 'unexpected resumed build job state: %', job;
    END IF;
END
\$\$;
SELECT pgcontext.run_build_job(
       (
         SELECT build_job_id
           FROM pgcontext.build_jobs('build_resume_docs')
          WHERE artifact_name = 'view-b'
       ),
       5
);
SELECT pgcontext.publish_artifact_segment_file(
       (
         SELECT build_job_id
           FROM pgcontext.build_jobs('build_resume_docs')
          WHERE artifact_name = 'view-b'
       ),
       pgcontext.build_mmap_hnsw_artifact(
           (SELECT build_job_id FROM pgcontext.build_jobs('build_resume_docs') WHERE artifact_name = 'view-b')
       )
);
SQL
printf 'build_job_abandoned_owner_recovered\n'

validate_serving_ready "view-a" "build_job_final_serving_ready_view_a"
validate_serving_ready "view-b" "build_job_final_serving_ready_view_b"
validate_mmap_search_order "view-b" "30,20,10" "build_job_source_recheck_after_restart"

psql_db <<'SQL'
DELETE FROM public.build_resume_docs
 WHERE id = 20;
VACUUM (ANALYZE) public.build_resume_docs;
SQL
validate_mmap_search_order "view-a" "30,10" "build_job_vacuum_source_recheck"
