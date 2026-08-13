#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_exact_first_lock_order}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;
CREATE TABLE public.p14_lock_source (
    id bigint PRIMARY KEY,
    embedding pgcontext.vector(3) NOT NULL
);
INSERT INTO public.p14_lock_source
SELECT value, ARRAY[value::real, 1::real, 0::real]::pgcontext.vector
  FROM pg_catalog.generate_series(1, 10000) AS value;
ANALYZE public.p14_lock_source;

SELECT * FROM pgcontext.register_exact_first(
    'p14_lock_order', 'public.p14_lock_source',
    jsonb_build_object(
        'version', 'exact_first_registration_v1',
        'key_column', 'id',
        'bindings', jsonb_build_array(jsonb_build_object(
            'name', 'embedding', 'column', 'embedding', 'kind', 'dense',
            'dimensions', 3, 'metric', 'l2'
        ))
    )
);
SELECT * FROM pgcontext.exact_first_advisor(
    'p14_lock_order',
    jsonb_build_object(
        'version', 'exact_first_advisor_v1',
        'memory_budget_bytes', 1000000000,
        'build_window_seconds', 3600,
        'update_millihertz', 10000,
        'filter_selectivity_bps', 10000
    )
);
SQL

LOCKER_OUT="$(mktemp "${HEAVY_TMPDIR}/p14-lock-order-a-out.XXXXXX")"
LOCKER_ERR="$(mktemp "${HEAVY_TMPDIR}/p14-lock-order-a-err.XXXXXX")"
APPLY_OUT="$(mktemp "${HEAVY_TMPDIR}/p14-lock-order-b-out.XXXXXX")"
APPLY_ERR="$(mktemp "${HEAVY_TMPDIR}/p14-lock-order-b-err.XXXXXX")"

cleanup() {
    if [[ -n "${LOCKER_PID:-}" ]]; then
        kill "${LOCKER_PID}" 2>/dev/null || true
    fi
    if [[ -n "${APPLY_PID:-}" ]]; then
        kill "${APPLY_PID}" 2>/dev/null || true
    fi
    rm -f "${LOCKER_OUT}" "${LOCKER_ERR}" "${APPLY_OUT}" "${APPLY_ERR}"
}
trap cleanup EXIT

psql_db >"${LOCKER_OUT}" 2>"${LOCKER_ERR}" <<'SQL' &
\set VERBOSITY verbose
SET deadlock_timeout = '200ms';
SET statement_timeout = '30s';
BEGIN;
LOCK TABLE public.p14_lock_source IN ACCESS EXCLUSIVE MODE;
SELECT pg_catalog.pg_sleep(5);
SELECT * FROM pgcontext.exact_first_advisor(
    'p14_lock_order',
    jsonb_build_object(
        'version', 'exact_first_advisor_v1',
        'memory_budget_bytes', 1000000000,
        'build_window_seconds', 3600,
        'update_millihertz', 10000,
        'filter_selectivity_bps', 400
    )
);
COMMIT;
SQL
LOCKER_PID=$!

lock_observed=false
for _ in $(seq 1 100); do
    if [[ "$(psql_db -Atc "SELECT count(*) FROM pg_catalog.pg_locks WHERE relation = 'public.p14_lock_source'::regclass AND mode = 'AccessExclusiveLock' AND granted")" == "1" ]]; then
        lock_observed=true
        break
    fi
    sleep 0.05
done
if [[ "${lock_observed}" != true ]]; then
    echo "source relation lock was not observed" >&2
    exit 1
fi

psql_db >"${APPLY_OUT}" 2>"${APPLY_ERR}" <<'SQL' &
\set VERBOSITY verbose
SET deadlock_timeout = '200ms';
SET statement_timeout = '30s';
SELECT * FROM pgcontext.apply_exact_first_plan(
    'p14_lock_order', 1, 'apply_foreground'
);
SQL
APPLY_PID=$!

apply_wait_observed=false
for _ in $(seq 1 100); do
    if [[ "$(psql_db -Atc "SELECT count(*) FROM pg_catalog.pg_stat_activity WHERE datname = current_database() AND pid <> pg_backend_pid() AND query LIKE '%apply_exact_first_plan%' AND wait_event_type = 'Lock'")" == "1" ]]; then
        apply_wait_observed=true
        break
    fi
    sleep 0.05
done
if [[ "${apply_wait_observed}" != true ]]; then
    echo "foreground apply did not wait on the source relation" >&2
    exit 1
fi

wait "${LOCKER_PID}"
LOCKER_PID=""
if wait "${APPLY_PID}"; then
    echo "stale foreground apply unexpectedly succeeded" >&2
    exit 1
fi
APPLY_PID=""

if ! grep -q 'exact-first plan changed before foreground application' "${APPLY_ERR}"; then
    echo "foreground apply did not report the expected plan fence" >&2
    cat "${APPLY_ERR}" >&2
    exit 1
fi
if grep -q '40P01' "${APPLY_ERR}" "${LOCKER_ERR}"; then
    echo "foreground apply lock order deadlocked" >&2
    exit 1
fi

assert_sql_equals \
    "SELECT current_plan_revision FROM pgcontext._exact_first_registrations WHERE collection_id = (SELECT collection_id FROM pgcontext._collections WHERE collection_name = 'p14_lock_order')" \
    "2"
assert_sql_equals \
    "SELECT pg_catalog.to_regclass(plans.evidence->>'index_name') IS NULL FROM pgcontext._exact_first_plans AS plans JOIN pgcontext._exact_first_registrations AS registrations USING (exact_first_registration_id) WHERE registrations.collection_id = (SELECT collection_id FROM pgcontext._collections WHERE collection_name = 'p14_lock_order') AND plans.plan_revision = 1" \
    "t"

echo "exact_first_foreground_lock_order=passed"
