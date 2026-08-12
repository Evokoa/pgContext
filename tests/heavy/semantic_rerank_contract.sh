#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_semantic_rerank_contract}"
ROW_COUNT="${ROW_COUNT:-10000}"
HARDWARE_ARCH="$(uname -m 2>/dev/null || printf unknown)"
HARDWARE_OS="$(uname -s 2>/dev/null || printf unknown)"
HARDWARE_CPUS="$(getconf _NPROCESSORS_ONLN 2>/dev/null || printf unknown)"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

CERTIFICATION_MANIFEST="$(cargo run -q -p context-test --bin p12_semantic_rerank_manifest)"
manifest_value() {
    local key="$1"
    awk -F '\t' -v key="${key}" '$1 == key { print $2; found = 1 } END { if (!found) exit 1 }' \
        <<<"${CERTIFICATION_MANIFEST}"
}
P12_MANIFEST_HASH="$(manifest_value manifest_hash)"
P12_HELD_OUT_QUERIES="$(manifest_value held_out_queries)"
P12_CANDIDATES="$(manifest_value candidate_count)"
P12_TOP_K="$(manifest_value top_k)"
P12_MIN_NDCG_LIFT="$(manifest_value minimum_ndcg_lift)"
P12_P50_MICROS="$(manifest_value p50_micros)"
P12_P95_MICROS="$(manifest_value p95_micros)"
P12_COLD_MICROS="$(manifest_value cold_start_micros)"
P12_MAX_RSS_BYTES="$(manifest_value max_rss_bytes)"
P12_MAX_COST_MICRODOLLARS_PER_1K="$(manifest_value max_cost_microdollars_per_1k)"
P12_ADAPTER="$(manifest_value adapter)"
P12_MODEL="$(manifest_value model_name)"
P12_MODEL_REVISION="$(manifest_value model_number)"
P12_TOKENIZER="$(manifest_value tokenizer_revision)"
P12_INPUT_CONTRACT="$(manifest_value input_contract)"
P12_OUTPUT_CONTRACT="$(manifest_value output_contract)"
P12_FAILURE_CONTRACT="$(manifest_value failure_contract)"
P12_SCORE_CONTRACT="$(manifest_value score_contract)"
P12_MAX_REQUEST_BYTES="$(manifest_value max_request_bytes)"
P12_MAX_QUERY_TOKENS="$(manifest_value max_query_tokens)"
P12_MAX_DOCUMENT_TOKENS="$(manifest_value max_document_tokens)"
P12_MAX_ELAPSED_MICROS="$(manifest_value max_elapsed_micros)"
P12_MAX_RETRIES="$(manifest_value max_retries)"
P12_BREAKER_FAILURES="$(manifest_value breaker_failures)"
P12_BREAKER_COOLDOWN_MICROS="$(manifest_value breaker_cooldown_micros)"
P12_ARTIFACT_BYTES="$(manifest_value artifact_bytes)"
P12_ARTIFACT_SHA256="$(manifest_value artifact_sha256)"
P12_LICENSE_SPDX="$(manifest_value license_spdx)"
P12_LICENSE_URL="$(manifest_value license_url)"
P12_DISTRIBUTION="$(manifest_value distribution)"
P12_REQUIRED_DATASET_ROWS="$(manifest_value required_dataset_rows)"
P12_REQUIRED_DATASET_SHA256="$(manifest_value required_dataset_sha256)"
P12_WORKLOAD_SHA256="$(manifest_value workload_sha256)"

if [[ ! "${ROW_COUNT}" =~ ^[1-9][0-9]*$ ]] || (( ROW_COUNT < 32 )); then
    echo "ROW_COUNT must be an integer of at least 32" >&2
    exit 2
fi

ROLE_SUFFIX="${DBNAME:0:20}_${PGPORT}"
TABLE_ROLE="pgctx_sr_table_${ROLE_SUFFIX}"
OWNER_ROLE="pgctx_sr_owner_${ROLE_SUFFIX}"
for role in "${TABLE_ROLE}" "${OWNER_ROLE}"; do
    require_simple_identifier "${role}" "role"
done

case "${HARDWARE_OS}:${HARDWARE_ARCH}" in
    Darwin:arm64) WORKER_PLATFORM="darwin-aarch64" ;;
    Darwin:x86_64) WORKER_PLATFORM="darwin-x86_64" ;;
    Linux:aarch64) WORKER_PLATFORM="linux-aarch64" ;;
    Linux:x86_64) WORKER_PLATFORM="linux-x86_64" ;;
    *) echo "unsupported worker platform" >&2; exit 2 ;;
esac

start_and_install_extension
reset_database
drop_role_if_exists "${OWNER_ROLE}"
drop_role_if_exists "${TABLE_ROLE}"
create_login_role "${TABLE_ROLE}"
create_login_role "${OWNER_ROLE}"

WORKER_DIR="$(mktemp -d "${HEAVY_TMPDIR}/p12-worker.XXXXXX")"
trap 'rm -rf "${WORKER_DIR}"' EXIT
printf '%s' 'UEdMUEFJUjEAAAAAAAAAAAAAAAAAACBAAAAAAAAAEECamZmZmZm5Pw==' \
    | openssl base64 -d -A >"${WORKER_DIR}/linear_pair_v1.bin"
cat >"${WORKER_DIR}/manifest.json" <<JSON
{
  "schema_version": 1,
  "adapter": "${P12_ADAPTER}",
  "model": "${P12_MODEL}",
  "model_revision": ${P12_MODEL_REVISION},
  "artifact_path": "linear_pair_v1.bin",
  "artifact_bytes": ${P12_ARTIFACT_BYTES},
  "artifact_sha256": "${P12_ARTIFACT_SHA256}",
  "tokenizer_revision": "${P12_TOKENIZER}",
  "input_contract": "${P12_INPUT_CONTRACT}",
  "output_contract": "${P12_OUTPUT_CONTRACT}",
  "failure_contract": "${P12_FAILURE_CONTRACT}",
  "score_contract": "${P12_SCORE_CONTRACT}",
  "max_request_bytes": ${P12_MAX_REQUEST_BYTES},
  "max_candidates": ${P12_CANDIDATES},
  "max_query_tokens": ${P12_MAX_QUERY_TOKENS},
  "max_document_tokens": ${P12_MAX_DOCUMENT_TOKENS},
  "max_elapsed_micros": ${P12_MAX_ELAPSED_MICROS},
  "max_retries": ${P12_MAX_RETRIES},
  "breaker_failure_threshold": ${P12_BREAKER_FAILURES},
  "breaker_cooldown_micros": ${P12_BREAKER_COOLDOWN_MICROS},
  "supported_platforms": ["${WORKER_PLATFORM}"],
  "license_spdx": "${P12_LICENSE_SPDX}",
  "license_url": "${P12_LICENSE_URL}",
  "distribution": "${P12_DISTRIBUTION}"
}
JSON
cargo build -q --release -p pgcontext-worker --bin pgcontext-worker
WORKER_BIN="${REPO_ROOT}/target/release/pgcontext-worker"

psql_db <<SQL
CREATE EXTENSION pgcontext;
GRANT CREATE ON SCHEMA public TO ${TABLE_ROLE};
GRANT USAGE ON SCHEMA public, pgcontext TO ${OWNER_ROLE};
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA pgcontext TO ${OWNER_ROLE};

SET SESSION AUTHORIZATION ${TABLE_ROLE};
CREATE TABLE public.semantic_rerank_docs (
    id bigint PRIMARY KEY,
    tenant text NOT NULL,
    body text NOT NULL,
    source_version bigint NOT NULL DEFAULT 1
);
INSERT INTO public.semantic_rerank_docs (id, tenant, body)
SELECT id,
       CASE WHEN id <= ${P12_CANDIDATES} THEN 'quality' ELSE 'bulk' END,
       CASE WHEN id <= ${P12_HELD_OUT_QUERIES}
            THEN 'topic' || id::text || ' authoritative answer'
            ELSE 'bounded distractor document ' || id::text
       END
  FROM pg_catalog.generate_series(1::bigint, ${ROW_COUNT}::bigint) AS id;
ALTER TABLE public.semantic_rerank_docs ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.semantic_rerank_docs FORCE ROW LEVEL SECURITY;
CREATE POLICY semantic_rerank_tenant ON public.semantic_rerank_docs
    USING (tenant = pg_catalog.current_setting('pgcontext_heavy.tenant', true));
GRANT SELECT ON public.semantic_rerank_docs TO ${OWNER_ROLE};
RESET SESSION AUTHORIZATION;

SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'quality';
SELECT pgcontext.create_collection('semantic_rerank_docs', 'public.semantic_rerank_docs');
SELECT pgcontext.register_filter_column('semantic_rerank_docs', 'tenant', 'tenant');
SELECT pgcontext.register_semantic_rerank_source(
    'semantic_rerank_docs', 'body', 'body', 'source_version'
);
RESET SESSION AUTHORIZATION;

BEGIN;
ALTER TABLE pgcontext._collection_points
    DISABLE TRIGGER pgcontext_capture_build_point_delta;
INSERT INTO pgcontext._collection_points (collection_id, source_key)
SELECT collections.collection_id, source.id::text
  FROM pgcontext._collections AS collections
  CROSS JOIN public.semantic_rerank_docs AS source
 WHERE collections.collection_name = 'semantic_rerank_docs'
 ORDER BY source.id;
UPDATE pgcontext._collection_source_revisions AS revisions
   SET source_version = ${ROW_COUNT}::bigint + 1,
       updated_at = pg_catalog.now()
  FROM pgcontext._collections AS collections
 WHERE collections.collection_id = revisions.collection_id
   AND collections.collection_name = 'semantic_rerank_docs';
ALTER TABLE pgcontext._collection_points
    ENABLE TRIGGER pgcontext_capture_build_point_delta;
COMMIT;
SQL

hash_stream() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 | awk '{print $1}'
    else
        sha256sum | awk '{print $1}'
    fi
}

dataset_hash="$(
    psql_db -qAt -F '|' -c \
        "SELECT id, tenant, body, source_version FROM public.semantic_rerank_docs ORDER BY id" \
        | hash_stream
)"
workload_hash="$(
    psql_db -qAt -F '|' -c \
        "SELECT query_id,
                'topic' || query_id::text AS query_text,
                query_id AS relevant_id,
                pg_catalog.string_agg(candidate_id::text, ',' ORDER BY
                    (candidate_id = query_id), candidate_id)
           FROM pg_catalog.generate_series(1, ${P12_HELD_OUT_QUERIES}) AS query_id
          CROSS JOIN pg_catalog.generate_series(1, ${P12_CANDIDATES}) AS candidate_id
          GROUP BY query_id
          ORDER BY query_id" \
        | hash_stream
)"
if [[ "${workload_hash}" != "${P12_WORKLOAD_SHA256}" ]]; then
    echo "semantic rerank workload hash drifted from the frozen manifest" >&2
    exit 1
fi
if (( ROW_COUNT == P12_REQUIRED_DATASET_ROWS )) \
   && [[ "${dataset_hash}" != "${P12_REQUIRED_DATASET_SHA256}" ]]; then
    echo "semantic rerank required dataset hash drifted from the frozen manifest" >&2
    exit 1
fi

prepare_single_request() {
    local tenant="$1"
    psql_db -qAt -v tenant="${tenant}" <<SQL | tail -n 1
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = :'tenant';
SELECT pgcontext.prepare_semantic_rerank(
           'semantic_rerank_docs', 'body', 'topic1',
           pg_catalog.jsonb_build_array(
               pg_catalog.jsonb_build_object(
                   'occurrence_id', 1,
                   'point_id', (
                       SELECT point_id
                         FROM pgcontext._visible_collection_points
                        WHERE collection_id = (
                                  SELECT collection_id
                                    FROM pgcontext._visible_collections
                                   WHERE collection_name = 'semantic_rerank_docs'
                              )
                          AND source_key = '1'
                   ),
                   'fused_rank', 1,
                   'fused_score', 1.0,
                   'contributions', pg_catalog.jsonb_build_array(
                       pg_catalog.jsonb_build_object(
                           'profile', 'fused', 'rank', 1,
                           'native_score', 1.0, 'weight', 1.0,
                           'contribution', 1.0
                       )
                   )
               )
           ),
           '${P12_MODEL}', ${P12_MODEL_REVISION}, 5000, 'require_reranker', NULL, false
       )->>'request_id';
RESET SESSION AUTHORIZATION;
SQL
}

worker_latencies=()
ndcg_samples=()
max_rss_bytes=0
correct=0
worker_pid=""
baseline_ndcg_total="0"
reranked_ndcg_total="0"
for query_id in $(seq 1 "${P12_HELD_OUT_QUERIES}"); do
    prepared="$({ psql_db -qAt -F $'\t' -v query_id="${query_id}" <<SQL
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'quality';
WITH candidates AS (
    SELECT source.id,
           points.point_id,
           pg_catalog.row_number() OVER (
               ORDER BY (source.id = :query_id::bigint), source.id
           )::int AS fused_rank
      FROM public.semantic_rerank_docs AS source
      JOIN pgcontext._visible_collection_points AS points
        ON points.source_key = source.id::text
       AND points.collection_id = (
               SELECT collection_id FROM pgcontext._visible_collections
                WHERE collection_name = 'semantic_rerank_docs'
           )
     WHERE source.id BETWEEN 1 AND ${P12_CANDIDATES}
), envelope AS (
    SELECT pgcontext.prepare_semantic_rerank(
               'semantic_rerank_docs',
               'body',
               'topic' || :query_id::text,
               pg_catalog.jsonb_agg(
                   pg_catalog.jsonb_build_object(
                       'occurrence_id', id,
                       'point_id', point_id,
                       'fused_rank', fused_rank,
                       'fused_score', 1.0 / fused_rank::double precision,
                       'contributions', pg_catalog.jsonb_build_array(
                           pg_catalog.jsonb_build_object(
                               'profile', 'fused',
                               'rank', fused_rank,
                               'native_score', fused_rank::double precision,
                               'weight', 1.0,
                               'contribution', 1.0 / fused_rank::double precision
                           )
                       )
                   ) ORDER BY fused_rank
               ),
               '${P12_MODEL}', ${P12_MODEL_REVISION}, 5000, 'require_reranker', NULL, false
           ) AS value
      FROM candidates
)
SELECT value->>'request_id', value::text FROM envelope;
RESET SESSION AUTHORIZATION;
SQL
    } | tail -n 1)"
    IFS=$'\t' read -r request_id envelope <<<"${prepared}"
    if [[ -z "${request_id}" || -z "${envelope}" ]]; then
        echo "semantic rerank preparation returned no envelope" >&2
        exit 1
    fi

    if [[ "${query_id}" == "1" ]]; then
        metrics_file="${WORKER_DIR}/worker-metrics.txt"
        worker_input_fifo="${WORKER_DIR}/worker-input.fifo"
        worker_output_fifo="${WORKER_DIR}/worker-output.fifo"
        mkfifo "${worker_input_fifo}" "${worker_output_fifo}"
        cold_started="$(perl -MTime::HiRes=time -e 'printf "%.0f", time * 1000000')"
        if [[ "${HARDWARE_OS}" == "Darwin" ]]; then
            /usr/bin/time -l "${WORKER_BIN}" score \
                --manifest "${WORKER_DIR}/manifest.json" \
                <"${worker_input_fifo}" >"${worker_output_fifo}" 2>"${metrics_file}" &
        else
            /usr/bin/time -f '%M' "${WORKER_BIN}" score \
                --manifest "${WORKER_DIR}/manifest.json" \
                <"${worker_input_fifo}" >"${worker_output_fifo}" 2>"${metrics_file}" &
        fi
        worker_pid="$!"
        exec 8>"${worker_input_fifo}"
        exec 9<"${worker_output_fifo}"
    fi
    if [[ "${query_id}" == "1" ]]; then
        started="${cold_started}"
    else
        started="$(perl -MTime::HiRes=time -e 'printf "%.0f", time * 1000000')"
    fi
    printf '%s\n' "${envelope}" >&8
    IFS= read -r response <&9
    finished="$(perl -MTime::HiRes=time -e 'printf "%.0f", time * 1000000')"
    latency="$((finished - started))"
    worker_latencies+=("${latency}")
    finalized="$(psql_db -qAt -F '|' -v request_id="${request_id}" -v response="${response}" -v query_id="${query_id}" <<SQL | tail -n 1
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'quality';
WITH result AS (
    SELECT pgcontext.finalize_semantic_rerank(
               :request_id::bigint, :'response'::jsonb, NULL
           ) AS value
)
SELECT value->>'status',
       coalesce((
           SELECT ordinality::text
             FROM pg_catalog.jsonb_array_elements(value->'results')
                  WITH ORDINALITY AS ranked(row, ordinality)
            WHERE (row->>'occurrence_id')::bigint = :query_id::bigint
       ), '0')
  FROM result;
RESET SESSION AUTHORIZATION;
SQL
    )"
    IFS='|' read -r status relevant_rank <<<"${finalized}"
    if [[ "${status}" != "reranked" || "${relevant_rank}" != "1" ]]; then
        echo "semantic rerank quality failure for query ${query_id}: ${finalized}" >&2
        exit 1
    fi
    baseline_rank="${P12_CANDIDATES}"
    baseline_ndcg="$(awk -v rank="${baseline_rank}" -v top_k="${P12_TOP_K}" 'BEGIN {
        if (rank > top_k) print 0; else printf "%.12f", log(2) / log(rank + 1)
    }')"
    reranked_ndcg="$(awk -v rank="${relevant_rank}" -v top_k="${P12_TOP_K}" 'BEGIN {
        if (rank == 0 || rank > top_k) print 0; else printf "%.12f", log(2) / log(rank + 1)
    }')"
    baseline_ndcg_total="$(awk -v total="${baseline_ndcg_total}" -v sample="${baseline_ndcg}" 'BEGIN { printf "%.12f", total + sample }')"
    reranked_ndcg_total="$(awk -v total="${reranked_ndcg_total}" -v sample="${reranked_ndcg}" 'BEGIN { printf "%.12f", total + sample }')"
    ndcg_samples+=("${baseline_ndcg}|${reranked_ndcg}")
    correct=$((correct + 1))
done

exec 8>&-
exec 9<&-
wait "${worker_pid}"
if [[ "${HARDWARE_OS}" == "Darwin" ]]; then
    max_rss_bytes="$(awk '/maximum resident set size/ {print $1}' "${metrics_file}" | tail -n 1)"
else
    rss_kib="$(tail -n 1 "${metrics_file}")"
    max_rss_bytes="$((rss_kib * 1024))"
fi
for query_index in "${!worker_latencies[@]}"; do
    IFS='|' read -r baseline_ndcg reranked_ndcg <<<"${ndcg_samples[query_index]}"
    echo "semantic_rerank_sample query=$((query_index + 1)) top_k=${P12_TOP_K} baseline_ndcg=${baseline_ndcg} reranked_ndcg=${reranked_ndcg} latency_micros=${worker_latencies[query_index]} rss_bytes=${max_rss_bytes}"
done

sorted_latencies="$(printf '%s\n' "${worker_latencies[@]:1}" | sort -n)"
warm_count="$((P12_HELD_OUT_QUERIES - 1))"
p50_index="$(((warm_count * 50 + 99) / 100))"
p95_index="$(((warm_count * 95 + 99) / 100))"
p50="$(printf '%s\n' "${sorted_latencies}" | sed -n "${p50_index}p")"
p95="$(printf '%s\n' "${sorted_latencies}" | sed -n "${p95_index}p")"
cold="${worker_latencies[0]}"
baseline_ndcg="$(awk -v total="${baseline_ndcg_total}" -v count="${P12_HELD_OUT_QUERIES}" 'BEGIN { printf "%.12f", total / count }')"
reranked_ndcg="$(awk -v total="${reranked_ndcg_total}" -v count="${P12_HELD_OUT_QUERIES}" 'BEGIN { printf "%.12f", total / count }')"
ndcg_lift="$(awk -v reranked="${reranked_ndcg}" -v baseline="${baseline_ndcg}" 'BEGIN { printf "%.12f", reranked - baseline }')"
observed_cost_microdollars_per_1k=0
quality_pass="$(awk -v lift="${ndcg_lift}" -v minimum="${P12_MIN_NDCG_LIFT}" 'BEGIN { print (lift >= minimum) ? 1 : 0 }')"
cost_pass="$(awk -v cost="${observed_cost_microdollars_per_1k}" -v maximum="${P12_MAX_COST_MICRODOLLARS_PER_1K}" 'BEGIN { print (cost <= maximum) ? 1 : 0 }')"
if (( correct != P12_HELD_OUT_QUERIES || quality_pass != 1 || cost_pass != 1 || p50 > P12_P50_MICROS || p95 > P12_P95_MICROS || cold > P12_COLD_MICROS || max_rss_bytes > P12_MAX_RSS_BYTES )); then
    decision="no_go"
else
    decision="pass"
fi

psql_db <<SQL
SET SESSION AUTHORIZATION ${TABLE_ROLE};
REVOKE SELECT ON public.semantic_rerank_docs FROM ${OWNER_ROLE};
RESET SESSION AUTHORIZATION;
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'quality';
DO \$security\$
BEGIN
    BEGIN
        PERFORM pgcontext.prepare_semantic_rerank(
            'semantic_rerank_docs', 'body', 'topic1',
            pg_catalog.jsonb_build_array(
                pg_catalog.jsonb_build_object(
                    'occurrence_id', 1,
                    'point_id', 1,
                    'fused_rank', 1,
                    'fused_score', 1.0,
                    'contributions', pg_catalog.jsonb_build_array(
                        pg_catalog.jsonb_build_object(
                            'profile', 'fused', 'rank', 1,
                            'native_score', 1.0, 'weight', 1.0,
                            'contribution', 1.0
                        )
                    )
                )
            ),
            '${P12_MODEL}', ${P12_MODEL_REVISION}
        );
        RAISE EXCEPTION 'source ACL denial was not enforced';
    EXCEPTION WHEN insufficient_privilege THEN
        NULL;
    END;
END
\$security\$;
RESET SESSION AUTHORIZATION;
SET SESSION AUTHORIZATION ${TABLE_ROLE};
GRANT SELECT ON public.semantic_rerank_docs TO ${OWNER_ROLE};
RESET SESSION AUTHORIZATION;
SQL

rls_denied="$(prepare_single_request bulk 2>&1 || true)"
if [[ "${rls_denied}" != *"one or more semantic rerank candidates are not visible"* ]]; then
    echo "semantic rerank RLS denial was not enforced" >&2
    exit 1
fi

churn_request_id="$(prepare_single_request quality)"
psql_db <<SQL
SET SESSION AUTHORIZATION ${TABLE_ROLE};
SET pgcontext_heavy.tenant = 'quality';
UPDATE public.semantic_rerank_docs
   SET body = 'source changed after release', source_version = source_version + 1
 WHERE id = 1;
RESET SESSION AUTHORIZATION;
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'quality';
DO \$churn\$
BEGIN
    BEGIN
        PERFORM pgcontext.finalize_semantic_rerank(
            ${churn_request_id},
            pg_catalog.jsonb_build_object(
                'version', 3,
                'request_id', ${churn_request_id},
                'model', '${P12_MODEL}',
                'model_revision', ${P12_MODEL_REVISION},
                'scores', pg_catalog.jsonb_build_array(
                    pg_catalog.jsonb_build_object('occurrence_id', 1, 'score', 1.0)
                )
            ),
            NULL
        );
        RAISE EXCEPTION 'source churn was not rejected';
    EXCEPTION WHEN object_not_in_prerequisite_state THEN
        NULL;
    END;
END
\$churn\$;
RESET SESSION AUTHORIZATION;
SQL

race_request_id="$(prepare_single_request quality)"
psql_db <<SQL
CREATE FUNCTION public.semantic_rerank_finalize_delay()
RETURNS trigger
LANGUAGE plpgsql
AS \$\$
BEGIN
    IF pg_catalog.current_setting('pgcontext_heavy.finalize_delay', true) = 'on'
       AND OLD.status = 'prepared'
       AND NEW.status = 'finalized'
    THEN
        PERFORM pg_catalog.pg_sleep(1);
    END IF;
    RETURN NEW;
END
\$\$;
CREATE TRIGGER semantic_rerank_finalize_delay
BEFORE UPDATE ON pgcontext._semantic_rerank_requests
FOR EACH ROW EXECUTE FUNCTION public.semantic_rerank_finalize_delay();
SQL

race_one_log="${WORKER_DIR}/finalize-race-one.log"
race_two_log="${WORKER_DIR}/finalize-race-two.log"
psql_db -qAt >"${race_one_log}" 2>&1 <<SQL &
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'quality';
SET pgcontext_heavy.finalize_delay = 'on';
SELECT pgcontext.finalize_semantic_rerank(
           ${race_request_id},
           pg_catalog.jsonb_build_object(
               'version', 3, 'request_id', ${race_request_id},
               'model', '${P12_MODEL}', 'model_revision', ${P12_MODEL_REVISION},
               'scores', pg_catalog.jsonb_build_array(
                   pg_catalog.jsonb_build_object('occurrence_id', 1, 'score', 1.0)
               )
           ), NULL
       )->>'status';
SQL
race_one_pid="$!"
sleep 0.2
psql_db -qAt >"${race_two_log}" 2>&1 <<SQL &
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'quality';
SELECT pgcontext.finalize_semantic_rerank(
           ${race_request_id},
           pg_catalog.jsonb_build_object(
               'version', 3, 'request_id', ${race_request_id},
               'model', '${P12_MODEL}', 'model_revision', ${P12_MODEL_REVISION},
               'scores', pg_catalog.jsonb_build_array(
                   pg_catalog.jsonb_build_object('occurrence_id', 1, 'score', 1.0)
               )
           ), NULL
       )->>'status';
SQL
race_two_pid="$!"
wait "${race_one_pid}"
wait "${race_two_pid}"
if ! grep -qx 'reranked' "${race_one_log}" || ! grep -qx 'reranked' "${race_two_log}"; then
    echo "identical semantic rerank finalizers did not converge" >&2
    sed -n '1,20p' "${race_one_log}" >&2
    sed -n '1,20p' "${race_two_log}" >&2
    exit 1
fi

different_request_id="$(prepare_single_request quality)"
different_one_log="${WORKER_DIR}/finalize-different-one.log"
different_two_log="${WORKER_DIR}/finalize-different-two.log"
psql_db -qAt >"${different_one_log}" 2>&1 <<SQL &
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'quality';
SET pgcontext_heavy.finalize_delay = 'on';
SELECT pgcontext.finalize_semantic_rerank(
           ${different_request_id},
           pg_catalog.jsonb_build_object(
               'version', 3, 'request_id', ${different_request_id},
               'model', '${P12_MODEL}', 'model_revision', ${P12_MODEL_REVISION},
               'scores', pg_catalog.jsonb_build_array(
                   pg_catalog.jsonb_build_object('occurrence_id', 1, 'score', 1.0)
               )
           ), NULL
       )->>'status';
SQL
different_one_pid="$!"
sleep 0.2
psql_db -qAt >"${different_two_log}" 2>&1 <<SQL &
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'quality';
SELECT pgcontext.finalize_semantic_rerank(
           ${different_request_id},
           pg_catalog.jsonb_build_object(
               'version', 3, 'request_id', ${different_request_id},
               'model', '${P12_MODEL}', 'model_revision', ${P12_MODEL_REVISION},
               'scores', pg_catalog.jsonb_build_array(
                   pg_catalog.jsonb_build_object('occurrence_id', 1, 'score', 0.5)
               )
           ), NULL
       )->>'status';
SQL
different_two_pid="$!"
if wait "${different_one_pid}"; then
    different_one_status=0
else
    different_one_status="$?"
fi
if wait "${different_two_pid}"; then
    different_two_status=0
else
    different_two_status="$?"
fi
different_successes=0
different_conflicts=0
for outcome in one two; do
    status_variable="different_${outcome}_status"
    log_variable="different_${outcome}_log"
    status="${!status_variable}"
    log="${!log_variable}"
    if [[ "${status}" -eq 0 ]] && grep -qx 'reranked' "${log}"; then
        different_successes=$((different_successes + 1))
    elif [[ "${status}" -ne 0 ]] \
        && grep -Eq \
            'already finalized by a different response|semantic rerank request changed during finalization' \
            "${log}"; then
        different_conflicts=$((different_conflicts + 1))
    fi
done
if [[ "${different_successes}" -ne 1 || "${different_conflicts}" -ne 1 ]]; then
    echo "competing semantic rerank finalizers did not produce one winner and one conflict" >&2
    sed -n '1,20p' "${different_one_log}" >&2
    sed -n '1,20p' "${different_two_log}" >&2
    exit 1
fi
stored_race_score="$(psql_db -qAt -c \
    "SELECT final_result->'results'->0->>'score'
       FROM pgcontext._semantic_rerank_requests
      WHERE request_id = ${different_request_id}")"
if [[ "${stored_race_score}" != "1.0" \
   && "${stored_race_score}" != "1" \
   && "${stored_race_score}" != "0.5" ]]; then
    echo "semantic rerank finalization race corrupted the stored winner" >&2
    exit 1
fi
psql_db <<SQL
DROP TRIGGER semantic_rerank_finalize_delay ON pgcontext._semantic_rerank_requests;
DROP FUNCTION public.semantic_rerank_finalize_delay();
SQL

echo "semantic_rerank_quality manifest=${P12_MANIFEST_HASH} dataset=${dataset_hash} workload=${workload_hash} rows=${ROW_COUNT} queries=${P12_HELD_OUT_QUERIES} top_k=${P12_TOP_K} baseline_ndcg=${baseline_ndcg} reranked_ndcg=${reranked_ndcg} lift=${ndcg_lift} minimum_lift=${P12_MIN_NDCG_LIFT} decision=${decision}"
echo "semantic_rerank_latency_cost p50_micros=${p50} p95_micros=${p95} cold_micros=${cold} max_rss_bytes=${max_rss_bytes} cost_microdollars_per_1k=${observed_cost_microdollars_per_1k} max_cost_microdollars_per_1k=${P12_MAX_COST_MICRODOLLARS_PER_1K}"
echo "semantic_rerank_security acl=pass rls=pass source_churn=pass finalize_race=pass"
echo "semantic_rerank_environment pg=${PG_VERSION} rows=${ROW_COUNT} os=${HARDWARE_OS} arch=${HARDWARE_ARCH} cpus=${HARDWARE_CPUS} model_sha256=${P12_ARTIFACT_SHA256} manifest=${P12_MANIFEST_HASH}"

if [[ "${decision}" != "pass" ]]; then
    echo "semantic rerank promotion decision is no-go" >&2
    exit 1
fi
echo "semantic_rerank_contract: ok"
