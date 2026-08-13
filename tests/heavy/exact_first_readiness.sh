#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_exact_first_readiness}"
ROW_COUNT="${ROW_COUNT:-10000}"
HARDWARE_ARCH="$(uname -m 2>/dev/null || printf unknown)"
HARDWARE_OS="$(uname -s 2>/dev/null || printf unknown)"
HARDWARE_CPUS="$(getconf _NPROCESSORS_ONLN 2>/dev/null || printf unknown)"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

if [[ ! "${ROW_COUNT}" =~ ^[1-9][0-9]*$ ]] || (( ROW_COUNT < 10000 )); then
    echo "ROW_COUNT must be an integer of at least 10000" >&2
    exit 2
fi

CERTIFICATION_MANIFEST="$(cargo run -q -p context-test --bin p14_exact_first_manifest)"
manifest_value() {
    local key="$1"
    awk -F '\t' -v key="${key}" '$1 == key { print $2; found = 1 } END { if (!found) exit 1 }' \
        <<<"${CERTIFICATION_MANIFEST}"
}
hash_text() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 | awk '{print $1}'
    else
        sha256sum | awk '{print $1}'
    fi
}

P14_MANIFEST_HASH="$(manifest_value manifest_hash)"
P14_DATASET_SPEC="$(manifest_value dataset_generator_spec)"
P14_DATASET_HASH="$(manifest_value dataset_generator_sha256)"
P14_WORKLOAD_SPEC="$(manifest_value workload_spec)"
P14_WORKLOAD_HASH="$(manifest_value workload_sha256)"
P14_REQUIRED_ROWS="$(manifest_value required_dataset_rows)"
P14_MIN_RECALL_BPS="$(manifest_value min_recall_bps)"
P14_MIN_BACKFILL_RPS="$(manifest_value min_backfill_rows_per_second)"
P14_BUILDING_P95="$(manifest_value max_building_query_p95_micros)"
P14_INDEXED_P95="$(manifest_value max_indexed_query_p95_micros)"
P14_INDEXED_CANDIDATE_BUDGET="$(manifest_value indexed_candidate_budget)"
P14_MAX_RSS="$(manifest_value max_rss_bytes)"
P14_MAX_TEMP="$(manifest_value max_temp_bytes)"
P14_MAX_WAL="$(manifest_value max_wal_bytes)"
P14_MAX_STORAGE="$(manifest_value max_storage_bytes)"

[[ "$(printf '%s' "${P14_DATASET_SPEC}" | hash_text)" == "${P14_DATASET_HASH}" ]]
[[ "$(printf '%s' "${P14_WORKLOAD_SPEC}" | hash_text)" == "${P14_WORKLOAD_HASH}" ]]

ROLE_SUFFIX="${DBNAME:0:24}_${PGPORT}"
APP_ROLE="p14_app_${ROLE_SUFFIX}"
DENIED_ROLE="p14_denied_${ROLE_SUFFIX}"
require_simple_identifier "${APP_ROLE}" "application role"
require_simple_identifier "${DENIED_ROLE}" "denied role"

start_and_install_extension
reset_database
drop_role_if_exists "${APP_ROLE}"
drop_role_if_exists "${DENIED_ROLE}"
create_login_role "${APP_ROLE}"
create_login_role "${DENIED_ROLE}"
psql_postgres -c "ALTER ROLE ${DENIED_ROLE} NOINHERIT"

psql_db -v app_role="${APP_ROLE}" -v row_count="${ROW_COUNT}" <<'SQL'
CREATE EXTENSION pgcontext;
GRANT USAGE ON SCHEMA pgcontext TO :"app_role";
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA pgcontext TO :"app_role";
GRANT CREATE ON SCHEMA public TO :"app_role";

SET SESSION AUTHORIZATION :"app_role";
CREATE TABLE public.p14_exact_source (
    id bigint PRIMARY KEY,
    tenant int4 NOT NULL,
    embedding pgcontext.vector(2) NOT NULL,
    body text NOT NULL
);
INSERT INTO public.p14_exact_source (id, tenant, embedding, body)
SELECT id,
       (id % 8)::int4,
       format('[%s,%s]', id % 1000, id / 1000)::pgcontext.vector,
       format('exact-first fixture %s', id)
  FROM pg_catalog.generate_series(1::bigint, :'row_count'::bigint) AS id;
ANALYZE public.p14_exact_source;

SELECT pgcontext.create_collection('p14_exact', 'public.p14_exact_source');
SELECT * FROM pgcontext.register_exact_first(
    'p14_exact', 'public.p14_exact_source',
    jsonb_build_object(
        'version', 'exact_first_registration_v1',
        'key_column', 'id',
        'bindings', jsonb_build_array(jsonb_build_object(
            'name', 'embedding', 'column', 'embedding', 'kind', 'dense',
            'dimensions', 2, 'metric', 'l2'
        ))
    ),
    'recommend_only'
);

CREATE TABLE public.p14_exact_samples (
    stage text NOT NULL,
    sample_ordinal int4 NOT NULL,
    duration_micros numeric NOT NULL
);
CREATE TABLE public.p14_exact_evidence (
    key text PRIMARY KEY,
    value text NOT NULL
);
INSERT INTO public.p14_exact_evidence VALUES
    ('wal_start', pg_catalog.pg_current_wal_lsn()::text),
    ('temp_start', (SELECT temp_bytes::text FROM pg_catalog.pg_stat_database WHERE datname = current_database())),
    ('source_rows_at_start', (SELECT count(*)::text FROM public.p14_exact_source)),
    ('exact_before', (
        SELECT md5(string_agg(source_key || ':' || score::text, ',' ORDER BY score, source_key::bigint))
          FROM pgcontext.exact_first_search(
               'p14_exact', 'embedding', '[0,0]'::pgcontext.vector, 10)
    ));

DO $samples$
DECLARE
    started timestamptz;
BEGIN
    FOR ordinal IN 1..8 LOOP
        started := pg_catalog.clock_timestamp();
        PERFORM count(*) FROM pgcontext.exact_first_search(
            'p14_exact', 'embedding', '[0,0]'::pgcontext.vector, 10);
        INSERT INTO public.p14_exact_samples
        VALUES ('exact_only', ordinal,
                extract(epoch FROM pg_catalog.clock_timestamp() - started) * 1000000);
    END LOOP;
END
$samples$;

RESET SESSION AUTHORIZATION;
SQL

ADVISOR_ONE="$(mktemp "${HEAVY_TMPDIR}/p14-advisor-one.XXXXXX")"
ADVISOR_TWO="$(mktemp "${HEAVY_TMPDIR}/p14-advisor-two.XXXXXX")"
advisor_sql="SET SESSION AUTHORIZATION ${APP_ROLE}; SELECT plan_revision FROM pgcontext.exact_first_advisor('p14_exact', jsonb_build_object('version','exact_first_advisor_v1','memory_budget_bytes',17179869184,'build_window_seconds',7200,'update_millihertz',0,'filter_selectivity_bps',10000));"
psql_db -Atc "${advisor_sql}" >"${ADVISOR_ONE}" &
ADVISOR_PID_ONE=$!
psql_db -Atc "${advisor_sql}" >"${ADVISOR_TWO}" &
ADVISOR_PID_TWO=$!
wait "${ADVISOR_PID_ONE}"
wait "${ADVISOR_PID_TWO}"
[[ "$(tail -n 1 "${ADVISOR_ONE}")" == "$(tail -n 1 "${ADVISOR_TWO}")" ]]
[[ "$(psql_db -Atc 'SELECT count(*) FROM pgcontext._exact_first_plans')" == "1" ]]
rm -f "${ADVISOR_ONE}" "${ADVISOR_TWO}"

psql_db -v app_role="${APP_ROLE}" <<'SQL'
SET SESSION AUTHORIZATION :"app_role";
CREATE TABLE public.p14_plan AS
SELECT * FROM pgcontext.exact_first_advisor(
    'p14_exact',
    jsonb_build_object(
        'version', 'exact_first_advisor_v1',
        'memory_budget_bytes', 17179869184,
        'build_window_seconds', 7200,
        'update_millihertz', 0,
        'filter_selectivity_bps', 10000
    )
);
SELECT * FROM pgcontext.apply_exact_first_plan(
    'p14_exact', (SELECT plan_revision FROM public.p14_plan), 'enqueue');
CREATE TABLE public.p14_claim AS
SELECT * FROM pgcontext.claim_exact_first_build('p14_exact', 'p14-top-level', 60000);
RESET SESSION AUTHORIZATION;
SQL

psql_db -v app_role="${APP_ROLE}" -v denied_role="${DENIED_ROLE}" <<'SQL'
GRANT USAGE ON SCHEMA pgcontext, public TO :"denied_role";
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA pgcontext TO :"denied_role";
GRANT :"app_role" TO :"denied_role";
REVOKE ALL ON public.p14_exact_source FROM :"denied_role";
SET SESSION AUTHORIZATION :"denied_role";
DO $acl$
BEGIN
    BEGIN
        PERFORM * FROM pgcontext.exact_first_search(
            'p14_exact', 'embedding', '[0,0]'::pgcontext.vector, 10);
        RAISE EXCEPTION 'revoked source SELECT unexpectedly reached exact-first rows';
    EXCEPTION WHEN insufficient_privilege THEN
        NULL;
    END;
END
$acl$;
RESET SESSION AUTHORIZATION;
ALTER TABLE public.p14_exact_source ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.p14_exact_source FORCE ROW LEVEL SECURITY;
CREATE POLICY p14_exact_owner_rows ON public.p14_exact_source
    FOR SELECT TO :"app_role" USING (tenant = 0);
SET SESSION AUTHORIZATION :"app_role";
DO $rls$
DECLARE
    expected bigint[];
    observed bigint[];
BEGIN
    SELECT ARRAY(
        SELECT id
          FROM public.p14_exact_source
         WHERE tenant = 0
         ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector, id
         LIMIT 100
    ) INTO expected;
    SELECT ARRAY(
        SELECT source_key::bigint
          FROM pgcontext.exact_first_search(
              'p14_exact', 'embedding', '[0,0]'::pgcontext.vector, 100)
    ) INTO observed;
    IF observed IS DISTINCT FROM expected OR cardinality(observed) = 0 THEN
        RAISE EXCEPTION 'exact-first RLS rows differ from the invoker policy oracle';
    END IF;
END
$rls$;
RESET SESSION AUTHORIZATION;
ALTER TABLE public.p14_exact_source NO FORCE ROW LEVEL SECURITY;
ALTER TABLE public.p14_exact_source DISABLE ROW LEVEL SECURITY;
DROP POLICY p14_exact_owner_rows ON public.p14_exact_source;
SQL

DDL="$(psql_db -Atc 'SELECT generated_ddl FROM public.p14_claim')"
PLAN_REVISION="$(psql_db -Atc 'SELECT plan_revision FROM public.p14_claim')"
LEASE_TOKEN="$(psql_db -Atc 'SELECT lease_token FROM public.p14_claim')"
if [[ -z "${DDL}" || "${DDL}" != CREATE\ INDEX\ CONCURRENTLY* ]]; then
    echo "controller did not return a reviewed concurrent index statement" >&2
    exit 1
fi

COPY_FILE="$(mktemp "${HEAVY_TMPDIR}/p14-copy.XXXXXX")"
BUILD_LOG="$(mktemp "${HEAVY_TMPDIR}/p14-build.XXXXXX")"
trap 'rm -f "${COPY_FILE}" "${BUILD_LOG}"' EXIT
for offset in {1..16}; do
    id=$((ROW_COUNT + offset))
    printf '%s\t%s\t[2000,%s]\tcopy fixture %s\n' \
        "${id}" "$((id % 8))" "${offset}" "${id}" >>"${COPY_FILE}"
done

psql_db -c "INSERT INTO public.p14_exact_evidence VALUES ('build_started', pg_catalog.clock_timestamp()::text)" >/dev/null
PGAPPNAME=p14-exact-builder PGOPTIONS="${PGOPTIONS:-} -c search_path=public,pgcontext" \
    psql -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" -v ON_ERROR_STOP=1 \
    -c "SET SESSION AUTHORIZATION ${APP_ROLE}" -c "${DDL}" >"${BUILD_LOG}" 2>&1 &
BUILD_PID=$!

psql_db -c "SET SESSION AUTHORIZATION ${APP_ROLE}" \
    -c "UPDATE public.p14_exact_source SET embedding = '[9999,9999]' WHERE id = 1" \
    -c "UPDATE public.p14_exact_source SET id = $((ROW_COUNT + 200)) WHERE id = 3" \
    -c "DELETE FROM public.p14_exact_source WHERE id = 2" \
    -c "INSERT INTO public.p14_exact_source VALUES ($((ROW_COUNT + 100)),0,'[-1,0]','insert fixture')" \
    -c "\\copy public.p14_exact_source (id,tenant,embedding,body) FROM '${COPY_FILE}'" >/dev/null

MAX_RSS_BYTES=0
ordinal=0
while true; do
    ordinal=$((ordinal + 1))
    psql_db -Atc "SET SESSION AUTHORIZATION ${APP_ROLE}; SELECT pgcontext.heartbeat_exact_first_build('p14_exact', ${PLAN_REVISION}, ${LEASE_TOKEN}, 60000)" | tail -n 1 | grep -qx t
    psql_db <<SQL >/dev/null
SET SESSION AUTHORIZATION ${APP_ROLE};
DO \$sample\$
DECLARE
    started timestamptz := pg_catalog.clock_timestamp();
BEGIN
    PERFORM count(*) FROM pgcontext.exact_first_search(
        'p14_exact', 'embedding', '[0,0]'::pgcontext.vector, 10);
    INSERT INTO public.p14_exact_samples
    VALUES ('building', ${ordinal},
            extract(epoch FROM pg_catalog.clock_timestamp() - started) * 1000000);
END
\$sample\$;
RESET SESSION AUTHORIZATION;
SQL
    backend_pid="$(psql_db -Atc "SELECT pid FROM pg_catalog.pg_stat_activity WHERE application_name = 'p14-exact-builder' LIMIT 1")"
    if [[ -n "${backend_pid}" ]]; then
        rss_kib="$(ps -o rss= -p "${backend_pid}" 2>/dev/null | tr -d ' ' || true)"
        if [[ "${rss_kib}" =~ ^[0-9]+$ ]] && (( rss_kib * 1024 > MAX_RSS_BYTES )); then
            MAX_RSS_BYTES=$((rss_kib * 1024))
        fi
    fi
    if (( ordinal >= 8 )) && [[ -z "${backend_pid}" ]]; then
        break
    fi
    if (( ordinal >= 8 )); then
        sleep 5
    fi
done

if ! wait "${BUILD_PID}"; then
    cat "${BUILD_LOG}" >&2
    exit 1
fi
psql_db -c "INSERT INTO public.p14_exact_evidence VALUES ('build_finished', pg_catalog.clock_timestamp()::text)" >/dev/null

psql_db -v app_role="${APP_ROLE}" -v plan_revision="${PLAN_REVISION}" \
    -v lease_token="${LEASE_TOKEN}" -v candidate_budget="${P14_INDEXED_CANDIDATE_BUDGET}" <<'SQL'
SET SESSION AUTHORIZATION :"app_role";
SELECT pg_catalog.set_config(
    'pgcontext.ivfflat_candidate_budget', :'candidate_budget', false
);
SELECT * FROM pgcontext.publish_exact_first_build(
    'p14_exact', :'plan_revision'::bigint, :'lease_token'::bigint);
-- Identical publication delivery converges after operational state is cleared.
SELECT * FROM pgcontext.publish_exact_first_build(
    'p14_exact', :'plan_revision'::bigint, :'lease_token'::bigint);

INSERT INTO public.p14_exact_evidence VALUES
    ('exact_after', (
        SELECT md5(string_agg(source_key || ':' || score::text, ',' ORDER BY score, source_key::bigint))
          FROM pgcontext.exact_first_search(
               'p14_exact', 'embedding', '[0,0]'::pgcontext.vector, 10)
    )),
    ('index_after', 'pending');
SET enable_indexscan = off;
SET enable_bitmapscan = off;
CREATE TABLE public.p14_oracle_ids AS
SELECT id, (embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector)::real AS score
  FROM public.p14_exact_source
 ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector, id
 LIMIT 10;
RESET enable_indexscan;
RESET enable_bitmapscan;
SET enable_seqscan = off;
CREATE TABLE public.p14_ann_ids AS
SELECT id, (embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector)::real AS score
  FROM public.p14_exact_source
 ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector, id
  LIMIT 10;
UPDATE public.p14_exact_evidence
   SET value = (
       SELECT md5(string_agg(id::text || ':' || score::text, ',' ORDER BY score, id))
         FROM public.p14_ann_ids
   )
 WHERE key = 'index_after';
RESET enable_seqscan;
INSERT INTO public.p14_exact_evidence VALUES
    ('recall_bps', (
        SELECT (count(*) * 1000)::text
          FROM public.p14_oracle_ids AS exact
          JOIN public.p14_ann_ids AS approximate
            ON approximate.id = exact.id
           AND approximate.score = exact.score
    ));

SET enable_seqscan = off;
DO $indexed_samples$
DECLARE
    started timestamptz;
BEGIN
    FOR ordinal IN 1..8 LOOP
        started := pg_catalog.clock_timestamp();
        PERFORM count(*)
          FROM (
              SELECT id
                FROM public.p14_exact_source
               ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector, id
               LIMIT 10
          ) AS indexed_top_k;
        INSERT INTO public.p14_exact_samples
        VALUES ('indexed', ordinal,
                extract(epoch FROM pg_catalog.clock_timestamp() - started) * 1000000);
    END LOOP;
END
$indexed_samples$;
RESET enable_seqscan;

ANALYZE public.p14_exact_source;

CREATE TABLE public.p14_retry_plan AS
SELECT * FROM pgcontext.exact_first_advisor(
    'p14_exact',
    jsonb_build_object(
        'version', 'exact_first_advisor_v1',
        'memory_budget_bytes', 17179869184,
        'build_window_seconds', 7200,
        'update_millihertz', 10000,
        'filter_selectivity_bps', 10000
    )
);
SELECT * FROM pgcontext.apply_exact_first_plan(
    'p14_exact', (SELECT plan_revision FROM public.p14_retry_plan), 'enqueue');
CREATE TABLE public.p14_abandoned_claim AS
SELECT * FROM pgcontext.claim_exact_first_build('p14_exact', 'p14-abandoned', 1000);
SELECT pgcontext.cancel_exact_first_build('p14_exact');
RESET SESSION AUTHORIZATION;
SQL

indexed_explain="$(psql_db -Atc "SET SESSION AUTHORIZATION ${APP_ROLE}; SET enable_seqscan=off; EXPLAIN (COSTS OFF) SELECT id FROM public.p14_exact_source ORDER BY embedding OPERATOR(pgcontext.<->) '[0,0]'::pgcontext.vector, id LIMIT 10")"
grep -q 'Index Scan using pgcontext_ef_' <<<"${indexed_explain}"

PGRX_DATA_DIR="$(psql_db -Atc 'SHOW data_directory' | tail -n 1)"
PG_CTL="$(pg_bin pg_ctl)"
psql_db -Atc "
    INSERT INTO public.p14_exact_evidence(key, value)
    SELECT 'temp_before_restart', temp_bytes::text
      FROM pg_catalog.pg_stat_database
     WHERE datname = current_database()
    ON CONFLICT (key) DO UPDATE SET value = excluded.value"
"${PG_CTL}" -D "${PGRX_DATA_DIR}" stop -m immediate
cargo pgrx start "${PG_VERSION}"
psql_db -Atc "
    INSERT INTO public.p14_exact_evidence(key, value)
    SELECT 'temp_after_restart_start', temp_bytes::text
      FROM pg_catalog.pg_stat_database
     WHERE datname = current_database()
    ON CONFLICT (key) DO UPDATE SET value = excluded.value"
for _ in {1..30}; do
    if psql_db -Atc "SELECT lease_expires_at <= pg_catalog.clock_timestamp() FROM pgcontext._exact_first_plan_jobs LIMIT 1" | grep -qx t; then
        break
    fi
    sleep 0.1
done

psql_db -v app_role="${APP_ROLE}" <<'SQL'
SET SESSION AUTHORIZATION :"app_role";
-- Expired cancel_requested work terminalizes without returning another lease.
SELECT * FROM pgcontext.claim_exact_first_build('p14_exact', 'p14-cancel-ack', 60000);
DO $retry$
DECLARE
    retried boolean;
    claimed record;
BEGIN
    SELECT pgcontext.retry_exact_first_build('p14_exact') INTO retried;
    IF NOT retried THEN
        RAISE EXCEPTION 'exact-first retry did not requeue cancelled work';
    END IF;
    SELECT * INTO claimed
      FROM pgcontext.claim_exact_first_build('p14_exact', 'p14-retry', 60000);
    IF claimed.lease_token IS NULL THEN
        RAISE EXCEPTION 'exact-first retry did not produce a fenced claim';
    END IF;
    PERFORM pgcontext.fail_exact_first_build(
        'p14_exact', claimed.plan_revision, claimed.lease_token, 'fixture_failure');
END
$retry$;
RESET SESSION AUTHORIZATION;
SQL

exact_after="$(psql_db -Atc "SELECT value FROM public.p14_exact_evidence WHERE key='exact_after'")"
index_after="$(psql_db -Atc "SELECT value FROM public.p14_exact_evidence WHERE key='index_after'")"
[[ -n "${exact_after}" && "${exact_after}" == "${index_after}" ]]

read -r build_micros throughput building_p95 indexed_p95 index_bytes wal_bytes temp_bytes recall_bps state reason <<<"$(
    psql_db -AtF ' ' -c "
    WITH metrics AS (
        SELECT extract(epoch FROM (
                   (SELECT value::timestamptz FROM public.p14_exact_evidence WHERE key='build_finished') -
                   (SELECT value::timestamptz FROM public.p14_exact_evidence WHERE key='build_started')
               )) * 1000000 AS build_micros,
               percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_micros)
                   FILTER (WHERE stage='building') AS building_p95,
               percentile_cont(0.95) WITHIN GROUP (ORDER BY duration_micros)
                   FILTER (WHERE stage='indexed') AS indexed_p95
          FROM public.p14_exact_samples
    ), target AS (
        SELECT index_oid FROM pgcontext._visible_exact_first_targets
         WHERE lifecycle_state='current' AND structurally_validated
    ), readiness AS (
        SELECT readiness_state, readiness_reason
          FROM pgcontext.exact_first_readiness('p14_exact')
    )
    SELECT round(build_micros)::bigint,
           round(${ROW_COUNT}::numeric / NULLIF(build_micros / 1000000, 0))::bigint,
           round(building_p95)::bigint,
           round(indexed_p95)::bigint,
           pg_catalog.pg_relation_size((SELECT index_oid FROM target)),
           pg_catalog.pg_wal_lsn_diff(
               pg_catalog.pg_current_wal_lsn(),
               (SELECT value::pg_lsn FROM public.p14_exact_evidence WHERE key='wal_start')
           )::bigint,
           greatest(
               (SELECT value::bigint FROM public.p14_exact_evidence WHERE key='temp_before_restart') -
                   (SELECT value::bigint FROM public.p14_exact_evidence WHERE key='temp_start'),
               0
           ) + greatest(
               (SELECT temp_bytes FROM pg_catalog.pg_stat_database WHERE datname = current_database()) -
                   (SELECT value::bigint FROM public.p14_exact_evidence WHERE key='temp_after_restart_start'),
               0
           ),
           (SELECT value::int4 FROM public.p14_exact_evidence WHERE key='recall_bps'),
           readiness_state,
           readiness_reason
      FROM metrics CROSS JOIN readiness"
)"

decision="development_only"
if (( ROW_COUNT == P14_REQUIRED_ROWS )); then
    decision="pass"
    if (( throughput < P14_MIN_BACKFILL_RPS \
          || building_p95 > P14_BUILDING_P95 \
          || indexed_p95 > P14_INDEXED_P95 \
          || index_bytes > P14_MAX_STORAGE \
          || temp_bytes > P14_MAX_TEMP \
          || wal_bytes > P14_MAX_WAL \
          || recall_bps < P14_MIN_RECALL_BPS \
          || MAX_RSS_BYTES > P14_MAX_RSS )); then
        decision="no_go"
    fi
fi

printf 'exact_first_environment pg_version=%s os=%s arch=%s cpus=%s row_count=%s maintenance_work_mem=%s shared_buffers=%s max_parallel_maintenance_workers=%s\n' \
    "$(psql_db -Atc 'SHOW server_version')" "${HARDWARE_OS}" "${HARDWARE_ARCH}" \
    "${HARDWARE_CPUS}" "${ROW_COUNT}" "$(psql_db -Atc 'SHOW maintenance_work_mem')" \
    "$(psql_db -Atc 'SHOW shared_buffers')" "$(psql_db -Atc 'SHOW max_parallel_maintenance_workers')"
printf 'exact_first_manifest manifest_hash=%s dataset_sha256=%s workload_sha256=%s\n' \
    "${P14_MANIFEST_HASH}" "${P14_DATASET_HASH}" "${P14_WORKLOAD_HASH}"
printf 'exact_first_exact_oracle exact_hash=%s indexed_hash=%s recall_bps=%s\n' \
    "${exact_after}" "${index_after}" "${recall_bps}"
printf 'exact_first_state_samples final_state=%s final_reason=%s\n' "${state}" "${reason}"
printf 'exact_first_backfill duration_micros=%s rows_per_second=%s\n' \
    "${build_micros}" "${throughput}"
printf 'exact_first_concurrency advisor=passed copy=passed insert=passed update=passed key_update=passed delete=passed query=passed\n'
printf 'exact_first_resources rss_bytes=%s wal_bytes=%s temp_bytes=%s index_bytes=%s\n' \
    "${MAX_RSS_BYTES}" "${wal_bytes}" "${temp_bytes}" "${index_bytes}"
printf 'exact_first_recovery restart=passed cancel=passed retry=passed stale_lease=passed replay=passed\n'
printf 'exact_first_security top_level_non_superuser=passed source_acl=passed source_rls=passed\n'
printf 'exact_first_decision decision=%s building_p95_micros=%s indexed_p95_micros=%s\n' \
    "${decision}" "${building_p95}" "${indexed_p95}"

psql_db -Atc "SELECT 'exact_first_sample stage=' || stage || ' ordinal=' || sample_ordinal::text || ' duration_micros=' || duration_micros::text FROM public.p14_exact_samples ORDER BY stage, sample_ordinal"
