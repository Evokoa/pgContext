#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_automatic_observability}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

wait_for_stat() {
    local predicate="$1"
    local label="$2"
    local observed="0"
    for _ in $(seq 1 100); do
        observed="$(psql_db -Atc "
            SELECT count(*)
              FROM pgcontext.query_execution_stats()
             WHERE collection_name = 'automatic_observability_docs'
               AND ${predicate}
        ")"
        if [[ "${observed}" -gt 0 ]]; then
            printf 'automatic_observability_%s: %s\n' "${label}" "${observed}"
            return 0
        fi
        sleep 0.05
    done
    echo "timed out waiting for automatic telemetry: ${label}" >&2
    psql_db -c "TABLE pgcontext._query_stats" >&2 || true
    psql_db -c "SELECT * FROM pgcontext.query_telemetry_queue_stats()" >&2 || true
    exit 1
}

expect_query_failure() {
    local sql="$1"
    local label="$2"
    if psql_db -c "${sql}" >/dev/null 2>&1; then
        echo "expected query failure: ${label}" >&2
        exit 1
    fi
}

automatic_complete_count() {
    psql_db -Atc "
        SELECT COALESCE(sum(query_count), 0)
          FROM pgcontext.query_execution_stats()
         WHERE collection_name = 'automatic_observability_docs'
           AND completion = 'complete'
    "
}

run_success_query() {
    psql_db -c "
        SELECT count(*)
          FROM pgcontext.execute_query(
               'automatic_observability_docs',
               pgcontext.query_nearest('[1,511]'::vector, 5)
          )
    " >/dev/null
}

wait_for_complete_count() {
    local expected="$1"
    local observed="0"
    for _ in $(seq 1 100); do
        observed="$(automatic_complete_count)"
        [[ "${observed}" -ge "${expected}" ]] && return 0
        sleep 0.05
    done
    echo "timed out waiting for ${expected} completed observations; saw ${observed}" >&2
    exit 1
}

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.automatic_observability_docs (
    id bigint PRIMARY KEY,
    embedding vector(2) NOT NULL,
    tenant text NOT NULL
);
INSERT INTO public.automatic_observability_docs
SELECT value,
       format('[%s,%s]', value, 512 - value)::vector,
       CASE WHEN value % 2 = 0 THEN 'even' ELSE 'odd' END
  FROM generate_series(1, 512) AS value;

SELECT pgcontext.create_collection(
    'automatic_observability_docs',
    'public.automatic_observability_docs'
);
SELECT pgcontext.register_vector(
    'automatic_observability_docs', 'embedding', 'embedding', 2, 'l2'
);
SELECT pgcontext.register_filter_column(
    'automatic_observability_docs', 'tenant', 'tenant'
);
SELECT pgcontext.backfill_points('automatic_observability_docs', 1024);
CREATE INDEX automatic_observability_docs_hnsw
    ON public.automatic_observability_docs
    USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);
SELECT pgcontext.attach_hnsw_index(
    'automatic_observability_docs',
    'embedding',
    'public.automatic_observability_docs_hnsw'
);
SQL

psql_db -c "
    SELECT count(*)
      FROM pgcontext.execute_query(
           'automatic_observability_docs',
           pgcontext.query_nearest('[1,511]'::vector, 5)
      )
" >/dev/null
wait_for_stat "strategy = 'dense_hnsw' AND completion = 'complete' AND total_visits >= total_candidates AND total_rechecks > 0" "success"

expect_query_failure "
    SET log_min_messages = panic;
    SELECT *
      FROM pgcontext.execute_query(
           'automatic_observability_docs',
           pgcontext.query_nearest('missing', '[1,511]'::vector, NULL, 5)
      )
" "typed error"
wait_for_stat "completion = 'error'" "typed_error"

psql_db -c "
    SELECT * FROM pgcontext.configure_collection_limits(
        'automatic_observability_docs', true,
        NULL, NULL, NULL, NULL, 1, NULL, NULL, NULL
    )
" >/dev/null
expect_query_failure "
    SELECT *
      FROM pgcontext.execute_query(
           'automatic_observability_docs',
           pgcontext.query_nearest('[1,511]'::vector, 2)
      )
" "budget exhaustion"
wait_for_stat "completion = 'budget_exhausted'" "budget_exhausted"
psql_db -c "
    SELECT * FROM pgcontext.configure_collection_limits(
        'automatic_observability_docs', false,
        NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL
    )
" >/dev/null

psql_db -c "
    SET pgcontext.hnsw_mask_candidate_limit = 0;
    SELECT count(*)
      FROM pgcontext.execute_query(
           'automatic_observability_docs',
           pgcontext.query_nearest(
               NULL,
               '[1,511]'::vector,
               '{\"must\":[{\"key\":\"tenant\",\"match\":\"even\"}]}'::jsonb,
               5
           )
      )
" >/dev/null
wait_for_stat "strategy = 'dense_exact_fallback' AND lifecycle_state = 'Fallback'" "fallback"

# Hold a conflicting table lock after telemetry has begun but before candidate
# SQL can read the source, then let PostgreSQL's typed cancellation abort the
# caller transaction. Suppress server-log routing to prove the retrieval error
# boundary, rather than emit_log_hook, preserves the event independently.
psql_db -c "BEGIN; LOCK TABLE public.automatic_observability_docs IN ACCESS EXCLUSIVE MODE; SELECT pg_sleep(2); COMMIT" >/dev/null &
locker_pid=$!
sleep 0.2
expect_query_failure "
    SET log_min_messages = panic;
    SET statement_timeout = '100ms';
    SELECT *
      FROM pgcontext.execute_query(
           'automatic_observability_docs',
           pgcontext.query_nearest('[1,511]'::vector, 5)
      )
" "statement cancellation"
wait "${locker_pid}"
wait_for_stat "completion = 'cancelled'" "cancelled"

complete_before="$(psql_db -Atc "
    SELECT COALESCE(sum(query_count), 0)
      FROM pgcontext.query_execution_stats()
     WHERE collection_name = 'automatic_observability_docs'
       AND completion = 'complete'
")"
psql_db -c "
    BEGIN;
    UPDATE public.automatic_observability_docs
       SET embedding = '[0,512]'::vector
     WHERE id = 1;
    SELECT pg_sleep(1);
    COMMIT;
" >/dev/null &
updater_pid=$!
sleep 0.2
psql_db -c "
    SELECT count(*)
      FROM pgcontext.execute_query(
           'automatic_observability_docs',
           pgcontext.query_nearest('[1,511]'::vector, 5)
      )
" >/dev/null
wait "${updater_pid}"
psql_db -c "
    SELECT count(*)
      FROM pgcontext.execute_query(
           'automatic_observability_docs',
           pgcontext.query_nearest('[0,512]'::vector, 5)
      )
" >/dev/null
for _ in $(seq 1 100); do
    complete_after="$(psql_db -Atc "
        SELECT COALESCE(sum(query_count), 0)
          FROM pgcontext.query_execution_stats()
         WHERE collection_name = 'automatic_observability_docs'
           AND completion = 'complete'
    ")"
    if [[ "${complete_after}" -ge $((complete_before + 2)) ]]; then
        printf 'automatic_observability_concurrent_update\n'
        break
    fi
    sleep 0.05
done
if [[ "${complete_after:-0}" -lt $((complete_before + 2)) ]]; then
    echo "concurrent-update observations did not persist" >&2
    exit 1
fi

# A quantization policy without a published mapped generation is a real
# source-readiness failure. It must survive the caller's rollback with the
# typed ArtifactMissing lifecycle verdict.
psql_db -c "
    SELECT pgcontext.configure_vector(
        'automatic_observability_docs',
        'embedding',
        '{}'::jsonb,
        '{\"mode\":\"scalar\",\"levels\":8}'::jsonb,
        'ready'
    )
" >/dev/null
expect_query_failure "
    SELECT *
      FROM pgcontext.execute_query(
           'automatic_observability_docs',
           pgcontext.query_nearest('[1,511]'::vector, 5)
      )
" "missing quantized generation"
wait_for_stat "lifecycle_state = 'ArtifactMissing'" "artifact_missing"

relative_path_q8="$(psql_db -At <<'SQL' | tail -n 1
SELECT build_job_id AS job_id
  FROM pgcontext.start_build_job(
       'automatic_observability_docs', 'mmap', 'automatic-observability-q8',
       'public.automatic_observability_docs', 0
  ) \gset
SELECT pgcontext.run_build_job(:job_id, 1);
SELECT relative_path
  FROM pgcontext.publish_artifact_segment_file(
       :job_id, pgcontext.build_mmap_hnsw_artifact(:job_id)
  );
SQL
)"
if [[ -z "${relative_path_q8}" ]]; then
    echo "quantized artifact publication returned no path" >&2
    exit 1
fi
psql_db -c "
    SELECT count(*)
      FROM pgcontext.execute_query(
           'automatic_observability_docs',
           pgcontext.query_nearest('[1,511]'::vector, 5)
      )
" >/dev/null
wait_for_stat "strategy = 'quantized_mmap_hnsw' AND lifecycle_state = 'Indexed'" "quantized"

psql_db -c "
    SELECT pgcontext.configure_vector(
        'automatic_observability_docs',
        'embedding',
        '{}'::jsonb,
        '{\"mode\":\"scalar\",\"levels\":16}'::jsonb,
        'ready'
    )
" >/dev/null
expect_query_failure "
    SELECT *
      FROM pgcontext.execute_query(
           'automatic_observability_docs',
           pgcontext.query_nearest('[1,511]'::vector, 5)
      )
" "quantized rebuild required"
wait_for_stat "lifecycle_state = 'IndexNotReady'" "rebuild_required"

relative_path_q16="$(psql_db -At <<'SQL' | tail -n 1
SELECT build_job_id AS job_id
  FROM pgcontext.start_build_job(
       'automatic_observability_docs', 'mmap', 'automatic-observability-q16',
       'public.automatic_observability_docs', 0
  ) \gset
SELECT pgcontext.run_build_job(:job_id, 1);
SELECT relative_path
  FROM pgcontext.publish_artifact_segment_file(
       :job_id, pgcontext.build_mmap_hnsw_artifact(:job_id)
  );
SQL
)"
data_directory="$(psql_db -Atc "SHOW data_directory")"
artifact_file="${data_directory}/${relative_path_q16}"
case "${artifact_file}" in
    "${data_directory}/pgcontext_artifacts/"*.pgctxseg) ;;
    *)
        echo "refusing to corrupt unexpected artifact path: ${artifact_file}" >&2
        exit 1
        ;;
esac
if [[ ! -f "${artifact_file}" ]]; then
    echo "quantized artifact file does not exist: ${artifact_file}" >&2
    exit 1
fi
printf '\000' | dd of="${artifact_file}" bs=1 seek=0 conv=notrunc 2>/dev/null
expect_query_failure "
    SELECT *
      FROM pgcontext.execute_query(
           'automatic_observability_docs',
           pgcontext.query_nearest('[1,511]'::vector, 5)
      )
" "corrupt quantized generation"
wait_for_stat "lifecycle_state = 'IndexCorrupt'" "corrupt"

# The superuser-only switch exists for controlled baselines. Measure warmed
# batches in one backend; enqueueing may not exceed a 20% + 0.05ms/query guard.
psql_db <<'SQL'
DO $$
DECLARE
    started timestamptz;
    disabled_ms double precision;
    enabled_ms double precision;
    disabled_samples double precision[] := '{}';
    enabled_samples double precision[] := '{}';
    iteration integer;
    round integer;
BEGIN
    PERFORM pgcontext.configure_vector(
        'automatic_observability_docs', 'embedding', '{}'::jsonb, '{}'::jsonb, 'ready'
    );
    FOR round IN 1..6 LOOP
        IF round % 2 = 1 THEN
            SET LOCAL pgcontext.query_telemetry_enabled = on;
        ELSE
            SET LOCAL pgcontext.query_telemetry_enabled = off;
        END IF;
        started := clock_timestamp();
        FOR iteration IN 1..50 LOOP
            PERFORM * FROM pgcontext.execute_query(
                'automatic_observability_docs',
                pgcontext.query_nearest('[1,511]'::vector, 5)
            );
        END LOOP;
        IF round % 2 = 1 THEN
            enabled_samples := array_append(
                enabled_samples,
                extract(epoch FROM clock_timestamp() - started) * 1000.0
            );
            SET LOCAL pgcontext.query_telemetry_enabled = off;
        ELSE
            disabled_samples := array_append(
                disabled_samples,
                extract(epoch FROM clock_timestamp() - started) * 1000.0
            );
            SET LOCAL pgcontext.query_telemetry_enabled = on;
        END IF;
        started := clock_timestamp();
        FOR iteration IN 1..50 LOOP
            PERFORM * FROM pgcontext.execute_query(
                'automatic_observability_docs',
                pgcontext.query_nearest('[1,511]'::vector, 5)
            );
        END LOOP;
        IF round % 2 = 1 THEN
            disabled_samples := array_append(
                disabled_samples,
                extract(epoch FROM clock_timestamp() - started) * 1000.0
            );
        ELSE
            enabled_samples := array_append(
                enabled_samples,
                extract(epoch FROM clock_timestamp() - started) * 1000.0
            );
        END IF;
    END LOOP;

    SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY sample)
      INTO disabled_ms FROM unnest(disabled_samples) AS sample;
    SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY sample)
      INTO enabled_ms FROM unnest(enabled_samples) AS sample;

    IF enabled_ms > disabled_ms * 1.20 + 2.5 THEN
        RAISE EXCEPTION 'automatic telemetry latency regression: enabled % ms, disabled % ms',
            enabled_ms, disabled_ms;
    END IF;
    RAISE NOTICE 'automatic telemetry latency: enabled % ms, disabled % ms',
        enabled_ms, disabled_ms;
END
$$;
SQL
printf 'automatic_observability_latency_gate\n'

psql_db <<'SQL'
DO $$
DECLARE
    queue record;
BEGIN
    SELECT * INTO queue FROM pgcontext.query_telemetry_queue_stats();
    IF queue.transport <> 'named_dsm_background_worker'
       OR queue.delivery <> 'best_effort_may_duplicate'
       OR queue.enqueued <= 0
       OR queue.persisted <= 0
       OR queue.dropped_contention * 20 > queue.enqueued + queue.dropped_contention
       OR queue.dropped_full <> 0
       OR queue.dropped_orphaned <> 0
       OR queue.database_slot_exhausted <> 0
       OR queue.worker_launch_failures <> 0
       OR queue.worker_pid IS NULL THEN
        RAISE EXCEPTION 'invalid automatic telemetry queue health: %', row_to_json(queue);
    END IF;
END
$$;
SQL
printf 'automatic_observability_queue_health\n'

sleep 6
assert_sql_equals \
    "SELECT (worker_pid IS NULL)::text FROM pgcontext.query_telemetry_queue_stats()" \
    "true"
printf 'automatic_observability_worker_idled\n'

# A producer that disappears after observation begin must not pin the worker.
# Start one worker, block a second query after begin, terminate that backend,
# and prove the worker still reaches its bounded idle exit.
psql_db -c "
    SELECT count(*)
      FROM pgcontext.execute_query(
           'automatic_observability_docs',
           pgcontext.query_nearest('[1,511]'::vector, 5)
      )
" >/dev/null
for _ in $(seq 1 100); do
    worker_present="$(psql_db -Atc \
        "SELECT (worker_pid IS NOT NULL)::text FROM pgcontext.query_telemetry_queue_stats()")"
    [[ "${worker_present}" == "true" ]] && break
    sleep 0.05
done
[[ "${worker_present}" == "true" ]] || {
    echo "telemetry worker did not start for termination probe" >&2
    exit 1
}

psql_db -c "BEGIN; LOCK TABLE public.automatic_observability_docs IN ACCESS EXCLUSIVE MODE; SELECT pg_sleep(3); COMMIT" >/dev/null &
termination_locker_pid=$!
sleep 0.2
PGAPPNAME=pgcontext_telemetry_termination_probe psql_db -c "
    SELECT count(*)
      FROM pgcontext.execute_query(
           'automatic_observability_docs',
           pgcontext.query_nearest('[1,511]'::vector, 5)
      )
" >/dev/null 2>&1 &
termination_client_pid=$!

termination_backend_pid=""
for _ in $(seq 1 100); do
    termination_backend_pid="$(psql_db -Atc "
        SELECT pid
          FROM pg_catalog.pg_stat_activity
         WHERE application_name = 'pgcontext_telemetry_termination_probe'
           AND datname = current_database()
         LIMIT 1
    ")"
    [[ "${termination_backend_pid}" =~ ^[1-9][0-9]*$ ]] && break
    sleep 0.05
done
[[ "${termination_backend_pid}" =~ ^[1-9][0-9]*$ ]] || {
    echo "could not locate termination-probe backend" >&2
    exit 1
}
assert_sql_equals "SELECT pg_catalog.pg_terminate_backend(${termination_backend_pid})::text" "true"
if wait "${termination_client_pid}"; then
    echo "terminated telemetry producer unexpectedly completed" >&2
    exit 1
fi
wait "${termination_locker_pid}"
sleep 6
assert_sql_equals \
    "SELECT (worker_pid IS NULL)::text FROM pgcontext.query_telemetry_queue_stats()" \
    "true"
printf 'automatic_observability_terminated_producer_reclaimed\n'

worker_crash_before="$(automatic_complete_count)"
run_success_query
wait_for_complete_count "$((worker_crash_before + 1))"
crashed_worker_pid="$(psql_db -Atc \
    "SELECT COALESCE(worker_pid, 0) FROM pgcontext.query_telemetry_queue_stats()")"
[[ "${crashed_worker_pid}" =~ ^[1-9][0-9]*$ ]] || {
    echo "telemetry worker did not start for crash recovery probe" >&2
    exit 1
}
assert_sql_equals \
    "SELECT pg_catalog.pg_terminate_backend(${crashed_worker_pid})::text" \
    "true"
worker_cleared="false"
for _ in $(seq 1 100); do
    worker_cleared="$(psql_db -Atc \
        "SELECT (worker_pid IS NULL)::text FROM pgcontext.query_telemetry_queue_stats()")"
    [[ "${worker_cleared}" == "true" ]] && break
    sleep 0.05
done
[[ "${worker_cleared}" == "true" ]] || {
    echo "abnormally exited telemetry worker retained queue ownership" >&2
    exit 1
}

run_success_query
wait_for_complete_count "$((worker_crash_before + 2))"
replacement_worker_pid="$(psql_db -Atc \
    "SELECT COALESCE(worker_pid, 0) FROM pgcontext.query_telemetry_queue_stats()")"
if [[ ! "${replacement_worker_pid}" =~ ^[1-9][0-9]*$ \
      || "${replacement_worker_pid}" == "${crashed_worker_pid}" ]]; then
    echo "telemetry worker was not replaced after abnormal exit: old=${crashed_worker_pid}, new=${replacement_worker_pid}" >&2
    exit 1
fi
printf 'automatic_observability_worker_crash_recovered\n'
