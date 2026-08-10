#!/usr/bin/env bash
set -euo pipefail

# Frozen Phase 10 live scale lane.
#
# At 1M rows the candidate scan reaches the 1,000,000-comparison allowance, but
# the frozen 500 ms global elapsed budget is tighter on this development
# harness. This lane proves that the live scan-based no-go path fails closed
# without prefix expansion. The pure manifest separately pins that allowance
# and the zero-prefix recheck-budget decision. The 10M command preserves the
# same live fail-closed/report contract for the larger corpus.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_adaptive_prefix_scale}"
ROW_COUNT="${ROW_COUNT:-1000000}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

if [[ "${ROW_COUNT}" != "1000000" && "${ROW_COUNT}" != "10000000" ]]; then
    echo "ROW_COUNT must be exactly 1000000 or 10000000" >&2
    exit 2
fi
scalar() {
    psql_db -tAc "$1" | tr -d '[:space:]'
}

ranked() {
    local setting="$1"
    psql_db -tAc "
        SET pgcontext.adaptive_prefix_dimensions = ${setting};
        SELECT string_agg(source_key, ',' ORDER BY score ASC, point_id ASC)
          FROM pgcontext.search(
              'adaptive_scale_docs',
              '[7,119]'::vector,
              10
          )" | tr -d '[:space:]'
}

wait_for_cancellation() {
    local value=""
    for _ in $(seq 1 100); do
        value="$(scalar "
            SELECT count(*) FROM pgcontext.query_execution_stats()
             WHERE collection_name = 'adaptive_scale_docs'
               AND completion = 'cancelled'")"
        if [[ "${value}" =~ ^[0-9]+$ ]] && (( value > 0 )); then
            return 0
        fi
        sleep 0.2
    done
    echo "timed out waiting for cancelled-query telemetry" >&2
    return 1
}

start_and_install_extension
reset_database

psql_db <<SQL
CREATE EXTENSION pgcontext;

CREATE TABLE public.adaptive_scale_docs (
    id bigint PRIMARY KEY,
    embedding vector(2) NOT NULL
);

INSERT INTO public.adaptive_scale_docs (id, embedding)
SELECT id,
       ARRAY[(id % 1000)::real, ((id * 17) % 1009)::real]::real[]::vector
  FROM pg_catalog.generate_series(1, ${ROW_COUNT}) AS id;

SELECT pgcontext.create_collection(
    'adaptive_scale_docs', 'public.adaptive_scale_docs'
);
SELECT pgcontext.register_vector(
    'adaptive_scale_docs', 'embedding', 'embedding', 2, 'l2'
);
-- Scale setup seeds the normalized point catalog set-wise. The per-row delta
-- trigger serially increments one revision row and would turn this retrieval
-- gate into a point-lifecycle benchmark; that lifecycle is certified by its
-- dedicated heavy/PG tests. Restore the trigger and equivalent final revision
-- before any retrieval work begins.
BEGIN;
ALTER TABLE pgcontext._collection_points
    DISABLE TRIGGER pgcontext_capture_build_point_delta;
INSERT INTO pgcontext._collection_points (collection_id, source_key)
SELECT collections.collection_id, documents.id::text
  FROM pgcontext._collections AS collections
 CROSS JOIN public.adaptive_scale_docs AS documents
 WHERE collections.collection_name = 'adaptive_scale_docs';
ALTER TABLE pgcontext._collection_points
    ENABLE TRIGGER pgcontext_capture_build_point_delta;
INSERT INTO pgcontext._collection_source_revisions (
    collection_id, source_version, updated_at
)
SELECT collections.collection_id, ${ROW_COUNT} + 1, pg_catalog.now()
  FROM pgcontext._collections AS collections
 WHERE collections.collection_name = 'adaptive_scale_docs'
ON CONFLICT (collection_id) DO UPDATE
      SET source_version = EXCLUDED.source_version,
          updated_at = EXCLUDED.updated_at;
COMMIT;

SET pgcontext.hnsw_m = 2;
SET pgcontext.hnsw_ef_construction = 2;
CREATE INDEX adaptive_scale_docs_hnsw ON public.adaptive_scale_docs
    USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops);

SELECT pgcontext.register_embedding_profile(
    'adaptive_scale_docs', 'mrl', 'embedding',
    'public.adaptive_scale_docs_hnsw',
    jsonb_build_object(
        'representation', 'dense', 'dimensions', 2,
        'normalization', 'none', 'metric', 'l2',
        'provider', 'pgcontext-heavy', 'model', 'mrl-2', 'revision', '1',
        'input_template', '{text}', 'output_template', '{vector}',
        'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
        'configuration_hash', '0123456789abcdef',
        'matryoshka_prefixes', jsonb_build_array(1)
    )
);

SELECT * FROM pgcontext.configure_collection_limits(
    'adaptive_scale_docs', false,
    NULL, NULL, NULL, NULL, NULL, NULL,
    120000,
    NULL
);
ANALYZE public.adaptive_scale_docs;
SQL

actual_rows="$(scalar "SELECT count(*) FROM public.adaptive_scale_docs")"
if [[ "${actual_rows}" != "${ROW_COUNT}" ]]; then
    echo "scale corpus row mismatch: expected ${ROW_COUNT}, got ${actual_rows}" >&2
    exit 1
fi
expected_source_version="$((ROW_COUNT + 1))"
source_version="$(scalar "
    SELECT revisions.source_version
      FROM pgcontext._collection_source_revisions AS revisions
      JOIN pgcontext._collections AS collections USING (collection_id)
     WHERE collections.collection_name = 'adaptive_scale_docs'")"
if [[ "${source_version}" != "${expected_source_version}" ]]; then
    echo "scale source revision mismatch: expected ${expected_source_version}, got ${source_version}" >&2
    exit 1
fi
trigger_state="$(scalar "
    SELECT point_trigger.tgenabled
      FROM pg_catalog.pg_trigger AS point_trigger
     WHERE point_trigger.tgrelid = 'pgcontext._collection_points'::regclass
       AND point_trigger.tgname = 'pgcontext_capture_build_point_delta'")"
if [[ "${trigger_state}" != "O" ]]; then
    echo "scale setup did not restore the point-delta trigger: ${trigger_state}" >&2
    exit 1
fi

error_log="${HEAVY_TMPDIR}/${DBNAME}_${ROW_COUNT}_no_go.log"
if ranked 1 >"${error_log}" 2>&1; then
    echo "${ROW_COUNT}-row adaptive no-go query unexpectedly returned output" >&2
    exit 1
fi
if ! grep -Eqi "statement timeout|comparison|budget" "${error_log}"; then
    echo "${ROW_COUNT}-row adaptive query failed for an unexpected reason" >&2
    cat "${error_log}" >&2
    exit 1
fi
wait_for_cancellation
expansions="$(scalar "
    SELECT sum(total_expansions)
      FROM pgcontext.query_execution_stats()
     WHERE collection_name = 'adaptive_scale_docs'
       AND completion = 'cancelled'")"
if [[ "${expansions}" != "0" ]]; then
    echo "${ROW_COUNT}-row no-go query performed prefix expansions: ${expansions}" >&2
    exit 1
fi
if [[ "${ROW_COUNT}" == "1000000" ]]; then
    echo "adaptive_scale_1m_elapsed_budget_exhausted"
else
    echo "adaptive_scale_10m_failed_closed"
fi

echo "adaptive-dimension scale evidence (rows=${ROW_COUNT}):"
psql_db -c "
    SELECT strategy, query_count, total_candidates, total_rechecks,
           total_expansions, adaptive_prefix_dimensions,
           adaptive_termination, round(avg_latency_ms::numeric, 3) AS avg_latency_ms
      FROM pgcontext.query_execution_stats()
     WHERE collection_name = 'adaptive_scale_docs'
     ORDER BY strategy"

cleanup_database
echo "adaptive_prefix_scale: ok"
