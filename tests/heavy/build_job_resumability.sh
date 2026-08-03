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

psql_db -c "SET lock_timeout = '5s'; SELECT count(*) FROM pgcontext.bulk_upsert_points('build_resume_docs', ARRAY['10', '20'], 2)" >/dev/null &
forward_overlap_pid=$!
psql_db -c "SET lock_timeout = '5s'; SELECT count(*) FROM pgcontext.bulk_upsert_points('build_resume_docs', ARRAY['20', '10'], 2)" >/dev/null &
reverse_overlap_pid=$!
wait "${forward_overlap_pid}"
wait "${reverse_overlap_pid}"
assert_sql_equals \
    "SELECT count(*)::text FROM pgcontext._collection_points WHERE collection_id = 1 AND deleted_at IS NULL" \
    "3"
printf 'set_based_point_overlap_has_stable_lock_order\n'

psql_db <<'SQL'
SET pgcontext.build_workers_enabled = off;
SELECT pgcontext.enqueue_build_job(
       'build_resume_docs',
       'certification',
       'disabled-worker-job'
);
DO $$
BEGIN
    IF (
        SELECT status FROM pgcontext._build_jobs
         WHERE artifact_name = 'disabled-worker-job'
    ) <> 'planned' THEN
        RAISE EXCEPTION 'disabled worker job did not remain planned';
    END IF;
END
$$;
SELECT pgcontext.request_build_cancel(
       (SELECT build_job_id FROM pgcontext._build_jobs
         WHERE artifact_name = 'disabled-worker-job')
);
SQL
printf 'supervised_build_worker_disabled_is_fail_open\n'

retry_job_id="$(psql_db -Atc "SELECT build_job_id FROM pgcontext._build_jobs WHERE artifact_name = 'disabled-worker-job'" | tail -n 1)"
psql_db -v ON_ERROR_STOP=1 -c "SELECT * FROM pgcontext.retry_build_job(${retry_job_id})" >/dev/null 2>&1 &
first_retry_pid=$!
psql_db -v ON_ERROR_STOP=1 -c "SELECT * FROM pgcontext.retry_build_job(${retry_job_id})" >/dev/null 2>&1 &
second_retry_pid=$!
set +e
wait "${first_retry_pid}"
first_retry_status=$?
wait "${second_retry_pid}"
second_retry_status=$?
set -e
retry_successes=0
if [[ "${first_retry_status}" -eq 0 ]]; then
    retry_successes=$((retry_successes + 1))
fi
if [[ "${second_retry_status}" -eq 0 ]]; then
    retry_successes=$((retry_successes + 1))
fi
if [[ "${retry_successes}" -ne 1 ]]; then
    echo "expected exactly one concurrent supervised retry to succeed: ${first_retry_status}, ${second_retry_status}" >&2
    exit 1
fi
assert_sql_equals \
    "SELECT attempt::text FROM pgcontext._build_jobs WHERE build_job_id = ${retry_job_id}" \
    "2"
retry_status="planned"
for _ in $(seq 1 100); do
    retry_status="$(psql_db -Atc "SELECT status FROM pgcontext._build_jobs WHERE build_job_id = ${retry_job_id}" | tail -n 1)"
    if [[ "${retry_status}" == "completed" ]]; then
        break
    fi
    sleep 0.1
done
if [[ "${retry_status}" != "completed" ]]; then
    echo "concurrently retried supervised job did not complete: ${retry_status}" >&2
    exit 1
fi
assert_sql_equals \
    "SELECT count(*)::text FROM pgcontext._generation_build_rows WHERE build_job_id = ${retry_job_id}" \
    "3"
printf 'supervised_build_retry_is_single_atomic_transition\n'

cancel_boundary_job_id="$(psql_db -Atq <<'SQL'
INSERT INTO pgcontext._build_jobs (
    collection_id, artifact_kind, artifact_name, target_name, job_kind,
    status, total_units, processed_units
)
SELECT collection_id, 'segment', 'cancel-boundary',
       'pgcontext._collection_points', 'artifact_build',
       'planned', 0, 0
  FROM pgcontext._collections
 WHERE collection_name = 'build_resume_docs'
RETURNING build_job_id;
SQL
)"
psql_db <<SQL >/dev/null &
UPDATE pgcontext._build_jobs
   SET status = 'validating',
       backend_pid = pg_catalog.pg_backend_pid(),
       backend_identity = (
           SELECT pg_catalog.to_char(
                      activity.backend_start AT TIME ZONE 'UTC',
                      'YYYY-MM-DD"T"HH24:MI:SS.US'
                  )
             FROM pg_catalog.pg_stat_activity AS activity
            WHERE activity.pid = pg_catalog.pg_backend_pid()
       ),
       lease_expires_at = pg_catalog.clock_timestamp() + INTERVAL '30 seconds'
 WHERE build_job_id = ${cancel_boundary_job_id};
BEGIN;
UPDATE pgcontext._build_jobs
   SET status = 'publishing', validation_passed = true, validation_findings = 0
 WHERE build_job_id = ${cancel_boundary_job_id};
SELECT pg_catalog.pg_sleep(2);
COMMIT;
SQL
cancel_publisher_pid=$!
cancel_boundary_status="planned"
for _ in $(seq 1 30); do
    cancel_boundary_status="$(psql_db -Atc "SELECT status FROM pgcontext._build_jobs WHERE build_job_id = ${cancel_boundary_job_id}" | tail -n 1)"
    if [[ "${cancel_boundary_status}" == "validating" ]]; then
        break
    fi
    sleep 0.1
done
set +e
psql_db -v ON_ERROR_STOP=1 -c "SELECT * FROM pgcontext.request_build_cancel(${cancel_boundary_job_id})" >/dev/null 2>&1
cancel_request_status=$?
set -e
wait "${cancel_publisher_pid}"
if [[ "${cancel_request_status}" -eq 0 ]]; then
    echo "cancellation crossed the validating-to-publishing boundary" >&2
    exit 1
fi
assert_sql_equals \
    "SELECT status || ':' || cancel_requested::text FROM pgcontext._build_jobs WHERE build_job_id = ${cancel_boundary_job_id}" \
    "publishing:false"
psql_db -c "UPDATE pgcontext._build_jobs SET status = 'abandoned', backend_pid = NULL, backend_identity = NULL, lease_expires_at = NULL, completed_at = pg_catalog.now() WHERE build_job_id = ${cancel_boundary_job_id}" >/dev/null
printf 'build_cancel_boundary_is_serialized\n'

psql_db <<'SQL'
SELECT pgcontext.enqueue_build_job(
       'build_resume_docs',
       'certification',
       'supervised-worker-job'
);
SQL

supervised_status="planned"
for _ in $(seq 1 100); do
    supervised_status="$(psql_db -Atc "SELECT status FROM pgcontext._build_jobs WHERE artifact_name = 'supervised-worker-job'" | tail -n 1)"
    if [[ "${supervised_status}" == "completed" ]]; then
        break
    fi
    sleep 0.1
done
if [[ "${supervised_status}" != "completed" ]]; then
    echo "supervised worker did not complete durable job: ${supervised_status}" >&2
    exit 1
fi
assert_sql_equals \
    "SELECT manifests.lifecycle_state || ':' || (jobs.source_version IS NOT NULL)::text FROM pgcontext._build_jobs AS jobs JOIN pgcontext._generation_manifests AS manifests USING (build_job_id) WHERE jobs.artifact_name = 'supervised-worker-job'" \
    "published:true"
printf 'supervised_build_worker_published_generation\n'

claim_boundary_job_id="$(psql_db -Atq <<'SQL'
SET pgcontext.build_workers_enabled = off;
SELECT build_job_id
  FROM pgcontext.enqueue_build_job(
       'build_resume_docs', 'certification', 'claim-boundary-job'
  );
SQL
)"
psql_db -v ON_ERROR_STOP=1 -c "BEGIN; SELECT build_job_id FROM pgcontext._build_jobs WHERE build_job_id = ${claim_boundary_job_id} FOR UPDATE; SELECT pg_catalog.pg_sleep(2); COMMIT" >/dev/null &
claim_lock_pid=$!
sleep 0.2
psql_db -c "SELECT pgcontext.wake_build_jobs('build_resume_docs')" >/dev/null
sleep 0.2
psql_db <<'SQL' >/dev/null &
INSERT INTO public.build_resume_docs (id, embedding, body)
VALUES (40, '[4,0]'::vector, 'claim boundary candidate forty');
SELECT pgcontext.upsert_points('build_resume_docs', ARRAY['40']);
SQL
claim_mutation_pid=$!
wait "${claim_lock_pid}"
wait "${claim_mutation_pid}"
claim_boundary_status="planned"
for _ in $(seq 1 100); do
    claim_boundary_status="$(psql_db -Atc "SELECT status FROM pgcontext._build_jobs WHERE build_job_id = ${claim_boundary_job_id}" | tail -n 1)"
    if [[ "${claim_boundary_status}" == "completed" ]]; then
        break
    fi
    sleep 0.1
done
if [[ "${claim_boundary_status}" != "completed" ]]; then
    echo "claim-boundary mutation was not reconciled: ${claim_boundary_status}" >&2
    exit 1
fi
assert_sql_equals \
    "SELECT count(*)::text FROM pgcontext._generation_build_rows WHERE build_job_id = ${claim_boundary_job_id} AND source_key = '40'" \
    "1"
psql_db -c "SELECT pgcontext.delete_points('build_resume_docs', ARRAY['40']); DELETE FROM public.build_resume_docs WHERE id = 40" >/dev/null
printf 'supervised_build_claim_boundary_delta_reconciled\n'

worker_count="1"
for _ in $(seq 1 100); do
    worker_count="$(psql_db -Atc "SELECT count(*) FROM pg_catalog.pg_stat_activity WHERE backend_type = 'pgContext generation build'" | tail -n 1)"
    if [[ "${worker_count}" == "0" ]]; then
        break
    fi
    sleep 0.1
done
if [[ "${worker_count}" != "0" ]]; then
    echo "supervised generation worker did not exit after its idle bound" >&2
    exit 1
fi
printf 'supervised_build_worker_idle_shutdown\n'

psql_db <<'SQL'
BEGIN;
SELECT pgcontext.enqueue_build_job(
       'build_resume_docs',
       'certification',
       'delayed-commit-job'
);
SELECT pg_catalog.pg_sleep(6);
COMMIT;
SQL
assert_sql_equals \
    "SELECT status FROM pgcontext._build_jobs WHERE artifact_name = 'delayed-commit-job'" \
    "planned"
psql_db -c "SELECT pgcontext.wake_build_jobs('build_resume_docs')" >/dev/null
delayed_status="planned"
for _ in $(seq 1 100); do
    delayed_status="$(psql_db -Atc "SELECT status FROM pgcontext._build_jobs WHERE artifact_name = 'delayed-commit-job'" | tail -n 1)"
    if [[ "${delayed_status}" == "completed" ]]; then
        break
    fi
    sleep 0.1
done
if [[ "${delayed_status}" != "completed" ]]; then
    echo "post-commit wake did not complete planned work: ${delayed_status}" >&2
    exit 1
fi
printf 'supervised_build_worker_post_commit_wake\n'

psql_db <<'SQL'
CREATE TABLE public.build_crash_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    body text NOT NULL
);
INSERT INTO public.build_crash_docs (id, embedding, body)
SELECT value,
       ('[' || value::text || ',0]')::vector,
       'worker crash fixture ' || value::text
  FROM pg_catalog.generate_series(1, 5000) AS values(value);
SELECT pgcontext.create_collection('build_crash_docs', 'public.build_crash_docs');
SELECT pgcontext.register_vector(
    'build_crash_docs', 'embedding', 'embedding', 2, 'l2'
);
SELECT pg_catalog.count(*)
  FROM pgcontext.upsert_points(
       'build_crash_docs',
       ARRAY(
           SELECT value::text
             FROM pg_catalog.generate_series(1, 5000) AS values(value)
            ORDER BY value
       )
  );
SELECT pgcontext.enqueue_build_job(
       'build_crash_docs',
       'certification',
       'crash-takeover-job'
);
SQL

crash_worker_pid=""
for _ in $(seq 1 100); do
    crash_worker_pid="$(psql_db -Atc "SELECT backend_pid FROM pgcontext._build_jobs WHERE artifact_name = 'crash-takeover-job' AND status = 'running'" | tail -n 1)"
    if [[ -n "${crash_worker_pid}" ]]; then
        break
    fi
    sleep 0.05
done
if [[ -z "${crash_worker_pid}" ]]; then
    echo "could not observe supervised worker before crash injection" >&2
    exit 1
fi
psql_db -c "SELECT pg_catalog.pg_terminate_backend(${crash_worker_pid})" >/dev/null
psql_db -c "SELECT pgcontext.wake_build_jobs('build_crash_docs')" >/dev/null
crash_status="running"
for _ in $(seq 1 200); do
    crash_status="$(psql_db -Atc "SELECT status FROM pgcontext._build_jobs WHERE artifact_name = 'crash-takeover-job'" | tail -n 1)"
    if [[ "${crash_status}" == "completed" ]]; then
        break
    fi
    sleep 0.1
done
if [[ "${crash_status}" != "completed" ]]; then
    echo "replacement worker did not recover crashed job: ${crash_status}" >&2
    exit 1
fi
assert_sql_equals \
    "SELECT (attempt >= 2)::text FROM pgcontext._build_jobs WHERE artifact_name = 'crash-takeover-job'" \
    "true"
printf 'supervised_build_worker_crash_takeover\n'

first_generation="$(psql_db -Atq <<'SQL'
WITH job AS (
    INSERT INTO pgcontext._build_jobs (
        collection_id, artifact_kind, artifact_name, target_name, job_kind,
        status, total_units, processed_units, config_revision, source_version,
        validation_passed, validation_findings, completed_at
    )
    SELECT collection_id, 'certification', 'concurrent-first-a',
           'pgcontext._collection_points', 'certification', 'completed',
           0, 0, config_revision,
           (SELECT source_version FROM pgcontext._collection_source_revisions
             WHERE collection_id = collections.collection_id),
           true, 0, pg_catalog.now()
      FROM pgcontext._collections AS collections
     WHERE collection_name = 'build_resume_docs'
    RETURNING build_job_id, collection_id, config_revision, source_version
)
INSERT INTO pgcontext._generation_manifests (
    collection_id, build_job_id, publication_alias, source_version,
    config_revision, lifecycle_state, validation_passed, validation_findings
)
SELECT collection_id, build_job_id, 'concurrent-first', source_version,
       config_revision, 'validated', true, 0
  FROM job
RETURNING generation;
SQL
)"
second_generation="$(psql_db -Atq <<'SQL'
WITH job AS (
    INSERT INTO pgcontext._build_jobs (
        collection_id, artifact_kind, artifact_name, target_name, job_kind,
        status, total_units, processed_units, config_revision, source_version,
        validation_passed, validation_findings, completed_at
    )
    SELECT collection_id, 'certification', 'concurrent-first-b',
           'pgcontext._collection_points', 'certification', 'completed',
           0, 0, config_revision,
           (SELECT source_version FROM pgcontext._collection_source_revisions
             WHERE collection_id = collections.collection_id),
           true, 0, pg_catalog.now()
      FROM pgcontext._collections AS collections
     WHERE collection_name = 'build_resume_docs'
    RETURNING build_job_id, collection_id, config_revision, source_version
)
INSERT INTO pgcontext._generation_manifests (
    collection_id, build_job_id, publication_alias, source_version,
    config_revision, lifecycle_state, validation_passed, validation_findings
)
SELECT collection_id, build_job_id, 'concurrent-first', source_version,
       config_revision, 'validated', true, 0
  FROM job
RETURNING generation;
SQL
)"
psql_db -c "INSERT INTO pgcontext._generation_artifacts (generation, artifact_kind, artifact_name, payload_bytes, checksum, payload) VALUES (${first_generation}, 'certification_evidence', 'concurrent-first-a', 2, pg_catalog.hashtextextended(pg_catalog.encode(convert_to('{}', 'UTF8'), 'hex'), 0), convert_to('{}', 'UTF8')), (${second_generation}, 'certification_evidence', 'concurrent-first-b', 2, pg_catalog.hashtextextended(pg_catalog.encode(convert_to('{}', 'UTF8'), 'hex'), 0), convert_to('{}', 'UTF8'))" >/dev/null
psql_db -c "SELECT pgcontext._publish_generation(${first_generation})" >/dev/null &
first_publish_pid=$!
psql_db -c "SELECT pgcontext._publish_generation(${second_generation})" >/dev/null &
second_publish_pid=$!
wait "${first_publish_pid}"
wait "${second_publish_pid}"
assert_sql_equals \
    "SELECT count(*)::text FROM pgcontext._generation_manifests WHERE generation IN (${first_generation}, ${second_generation}) AND lifecycle_state = 'published'" \
    "1"
assert_sql_equals \
    "SELECT count(*)::text FROM pgcontext._generation_manifests WHERE generation IN (${first_generation}, ${second_generation}) AND lifecycle_state = 'retired'" \
    "1"
printf 'generation_first_publication_serialized\n'

pinned_generation="$(psql_db -Atc "SELECT generation FROM pgcontext._generation_aliases AS aliases JOIN pgcontext._collections AS collections USING (collection_id) WHERE collections.collection_name = 'build_resume_docs' AND aliases.publication_alias = 'concurrent-first'" | tail -n 1)"
psql_db <<SQL >/dev/null &
SET TIME ZONE 'America/New_York';
SELECT pgcontext._pin_generation(
       (SELECT collection_id FROM pgcontext._collections WHERE collection_name = 'build_resume_docs'),
       'concurrent-first'
);
SELECT pg_catalog.pg_sleep(4);
SELECT pgcontext._unpin_generation(${pinned_generation});
SQL
timezone_pin_pid=$!
pin_count="0"
for _ in $(seq 1 30); do
    pin_count="$(psql_db -Atc "SELECT count(*) FROM pgcontext._generation_reader_pins WHERE generation = ${pinned_generation}" | tail -n 1)"
    if [[ "${pin_count}" == "1" ]]; then
        break
    fi
    sleep 0.1
done
if [[ "${pin_count}" != "1" ]]; then
    echo "different-TimeZone reader did not establish generation pin" >&2
    exit 1
fi
timezone_replacement_generation="$(psql_db -Atq <<'SQL'
WITH job AS (
    INSERT INTO pgcontext._build_jobs (
        collection_id, artifact_kind, artifact_name, target_name, job_kind,
        status, total_units, processed_units, config_revision, source_version,
        validation_passed, validation_findings, completed_at
    )
    SELECT collection_id, 'certification', 'timezone-replacement',
           'pgcontext._collection_points', 'certification', 'completed',
           0, 0, config_revision,
           (SELECT source_version FROM pgcontext._collection_source_revisions
             WHERE collection_id = collections.collection_id),
           true, 0, pg_catalog.now()
      FROM pgcontext._collections AS collections
     WHERE collection_name = 'build_resume_docs'
    RETURNING build_job_id, collection_id, config_revision, source_version
)
INSERT INTO pgcontext._generation_manifests (
    collection_id, build_job_id, publication_alias, source_version,
    config_revision, lifecycle_state, validation_passed, validation_findings
)
SELECT collection_id, build_job_id, 'concurrent-first', source_version,
       config_revision, 'validated', true, 0
  FROM job
RETURNING generation;
SQL
)"
psql_db -c "INSERT INTO pgcontext._generation_artifacts (generation, artifact_kind, artifact_name, payload_bytes, checksum, payload) VALUES (${timezone_replacement_generation}, 'certification_evidence', 'timezone-replacement', 2, pg_catalog.hashtextextended(pg_catalog.encode(convert_to('{}', 'UTF8'), 'hex'), 0), convert_to('{}', 'UTF8'))" >/dev/null
psql_db -c "SET TIME ZONE 'Asia/Tokyo'; SELECT pgcontext._publish_generation(${timezone_replacement_generation})" >/dev/null
assert_sql_equals \
    "SELECT lifecycle_state FROM pgcontext._generation_manifests WHERE generation = ${pinned_generation}" \
    "retiring"
assert_sql_equals \
    "SELECT count(*)::text FROM pgcontext._generation_reader_pins WHERE generation = ${pinned_generation}" \
    "1"
wait "${timezone_pin_pid}"
assert_sql_equals \
    "SELECT lifecycle_state FROM pgcontext._generation_manifests WHERE generation = ${pinned_generation}" \
    "retired"
printf 'generation_pin_identity_is_timezone_independent\n'

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
