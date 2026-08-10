#!/usr/bin/env bash
set -euo pipefail

# Adaptive-dimension (Matryoshka) retrieval lifecycle and parity gate.
#
# A certified prefix only changes how wide the *candidate* stage reads; the
# executor still rechecks and reranks against the full authoritative dimensions.
# So the contract is exact: at a sufficient candidate budget every declared
# prefix must return the identical ordered answer the full-vector path returns.
# This script proves that on a real server, proves the prefix path is actually
# selected rather than silently skipped, and reports the candidate and latency
# evidence behind the promotion decision.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_adaptive_prefix_recall}"
ROW_COUNT="${ROW_COUNT:-5000}"
QUERY_COUNT="${QUERY_COUNT:-25}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

if [[ ! "${ROW_COUNT}" =~ ^[0-9]+$ ]] || (( ROW_COUNT < 500 || ROW_COUNT > 9960 )); then
    echo "ROW_COUNT must be an integer between 500 and 9960" >&2
    exit 2
fi
if [[ ! "${QUERY_COUNT}" =~ ^[0-9]+$ ]] || (( QUERY_COUNT < 1 )); then
    echo "QUERY_COUNT must be a positive integer" >&2
    exit 2
fi

assert_equal() {
    local label="$1" expected="$2" actual="$3"
    if [[ "${expected}" != "${actual}" ]]; then
        echo "${label}: expected '${expected}', got '${actual}'" >&2
        exit 1
    fi
    echo "${label}: ok"
}

scalar() {
    psql_db -q -tAc "$1" | tr -d '[:space:]'
}

wait_for_positive_scalar() {
    local sql="$1" label="$2" value=""
    for _ in $(seq 1 50); do
        value="$(scalar "${sql}")"
        if [[ "${value}" =~ ^[0-9]+$ ]] && (( value > 0 )); then
            printf '%s\n' "${value}"
            return 0
        fi
        sleep 0.2
    done
    echo "timed out waiting for ${label}" >&2
    return 1
}

# Ranked answer for one query id at one adaptive-prefix setting.
ranked() {
    local setting="$1" query_id="$2"
    psql_db -tAc "
        SET pgcontext.adaptive_prefix_dimensions = ${setting};
        SELECT string_agg(source_key, ',' ORDER BY score ASC, point_id ASC)
          FROM pgcontext.search(
              'adaptive_docs',
              (SELECT embedding FROM public.adaptive_docs WHERE id = ${query_id}),
              10
          )" | tr -d '[:space:]'
}

# Ranked answer for one query id with a registered metadata filter.
ranked_filtered() {
    local setting="$1" query_id="$2"
    psql_db -tAc "
        SET pgcontext.adaptive_prefix_dimensions = ${setting};
        SELECT string_agg(source_key, ',' ORDER BY score ASC, point_id ASC)
          FROM pgcontext.search(
              'adaptive_docs',
              (SELECT embedding FROM public.adaptive_docs WHERE id = ${query_id}),
              '{\"must\":[{\"key\":\"bucket\",\"match\":\"b1\"}]}',
              10
          )" | tr -d '[:space:]'
}

start_and_install_extension
reset_database

psql_db <<SQL
CREATE EXTENSION pgcontext;

CREATE TABLE public.adaptive_docs (
    id bigint PRIMARY KEY,
    embedding vector(64) NOT NULL,
    bucket text NOT NULL
);

-- Deterministic unit-L2 vectors. The generator is correlated on id so every row
-- gets its own vector: an uncorrelated subquery would become an InitPlan and
-- give all rows the same value, which would make any parity check vacuous.
INSERT INTO public.adaptive_docs (id, embedding, bucket)
SELECT d.id,
       (SELECT pg_catalog.array_agg((c.v / sqrt(c.sq))::real)::vector
          FROM (SELECT v, pg_catalog.sum(v * v) OVER () AS sq
                  FROM (SELECT sin(d.id * 0.7 + k * 1.3)::real AS v
                          FROM pg_catalog.generate_series(1, 64) AS k) AS raw) AS c),
       'b' || (d.id % 5)::text
  FROM pg_catalog.generate_series(1, ${ROW_COUNT}) AS d(id);

SELECT pgcontext.create_collection('adaptive_docs', 'public.adaptive_docs');
SELECT pgcontext.register_vector('adaptive_docs', 'embedding', 'embedding', 64, 'cosine');
SELECT pgcontext.register_filter_column('adaptive_docs', 'bucket', 'bucket');
SELECT pgcontext.backfill_points('adaptive_docs', ${ROW_COUNT});

CREATE INDEX adaptive_docs_hnsw ON public.adaptive_docs
    USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops);

SELECT pgcontext.register_embedding_profile(
    'adaptive_docs', 'mrl', 'embedding', 'public.adaptive_docs_hnsw',
    jsonb_build_object(
        'representation', 'dense', 'dimensions', 64,
        'normalization', 'unit_l2', 'metric', 'cosine',
        'provider', 'pgcontext-heavy', 'model', 'mrl-64', 'revision', '1',
        'input_template', '{text}', 'output_template', '{vector}',
        'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
        'configuration_hash', '0123456789abcdef',
        'matryoshka_prefixes', jsonb_build_array(8, 16, 32)
    )
);
ANALYZE public.adaptive_docs;
SQL

distinct_vectors="$(scalar "SELECT count(*) FROM (SELECT DISTINCT embedding::text FROM public.adaptive_docs) AS d")"
assert_equal "the corpus carries one distinct vector per row" "${ROW_COUNT}" "${distinct_vectors}"

# ---------------------------------------------------------------------------
# Ordered parity: every declared prefix reproduces the full-vector answer.
# ---------------------------------------------------------------------------

for query_id in $(seq 1 "${QUERY_COUNT}"); do
    baseline="$(ranked -1 "${query_id}")"
    if [[ -z "${baseline}" ]]; then
        echo "full-vector search returned no rows for query ${query_id}" >&2
        exit 1
    fi
    for setting in 0 8 16 32; do
        observed="$(ranked "${setting}" "${query_id}")"
        if [[ "${observed}" != "${baseline}" ]]; then
            echo "adaptive prefix ${setting} lost ordered parity on query ${query_id}" >&2
            echo "  full vector: ${baseline}" >&2
            echo "  prefix ${setting}: ${observed}" >&2
            exit 1
        fi
    done
done
echo "every declared prefix reproduces the full-vector ordered answer: ok"

filtered_baseline="$(ranked_filtered -1 7)"
if [[ -z "${filtered_baseline}" ]]; then
    echo "full-vector filtered search returned no rows" >&2
    exit 1
fi
for setting in 0 8 16 32; do
    filtered_observed="$(ranked_filtered "${setting}" 7)"
    assert_equal "filtered prefix ${setting} preserves ordered parity" \
        "${filtered_baseline}" "${filtered_observed}"
done

composite_count="$(scalar "
    SET pgcontext.adaptive_prefix_dimensions = 0;
    SET pgcontext.hnsw_mask_candidate_limit = 0;
    SELECT count(*)
      FROM pgcontext.execute_query(
          'adaptive_docs',
          pgcontext.query_rerank(
              pgcontext.query_prefetch(ARRAY[
                  pgcontext.query_nearest(
                      NULL,
                      (SELECT embedding FROM public.adaptive_docs WHERE id = 7),
                      '{\"must\":[{\"key\":\"bucket\",\"match\":\"b1\"}]}'::jsonb,
                      5
                  ),
                  pgcontext.query_nearest(
                      NULL,
                      (SELECT embedding FROM public.adaptive_docs WHERE id = 11),
                      '{\"must\":[{\"key\":\"bucket\",\"match\":\"b1\"}]}'::jsonb,
                      5
                  )
              ]),
              5
          )
      )")"
assert_equal "adaptive reservation is not multiplied across composite branches" \
    "5" "${composite_count}"

# ---------------------------------------------------------------------------
# The prefix path must actually be selected, and never selected without a
# certified policy.
# ---------------------------------------------------------------------------

adaptive_selected="$(wait_for_positive_scalar "
    SELECT count(*) FROM pgcontext.query_execution_stats()
     WHERE collection_name = 'adaptive_docs'
       AND strategy = 'dense_adaptive_prefix_exhaustive'" \
    "adaptive prefix telemetry")"
echo "the adaptive candidate path is actually selected: ok"

# Without oversampling the recheck can only reorder a set the prefix already
# chose, so parity above would only be measuring whether the corpus is easy.
adaptive_candidates_per_query="$(scalar "
    SELECT (sum(total_candidates) / sum(query_count))::bigint
      FROM pgcontext.query_execution_stats()
     WHERE collection_name = 'adaptive_docs'
       AND strategy = 'dense_adaptive_prefix_exhaustive'")"
if (( adaptive_candidates_per_query <= 10 )); then
    echo "the prefix probe did not oversample: ${adaptive_candidates_per_query} candidates for a limit of 10" >&2
    exit 1
fi
echo "the prefix probe oversamples (${adaptive_candidates_per_query} candidates for a limit of 10): ok"

adaptive_expansions_per_query="$(scalar "
    SELECT (sum(total_expansions) / sum(query_count))::bigint
      FROM pgcontext.query_execution_stats()
     WHERE collection_name = 'adaptive_docs'
       AND strategy = 'dense_adaptive_prefix_exhaustive'")"
if (( adaptive_expansions_per_query < 1 )); then
    echo "the adaptive path did not report its widening step" >&2
    exit 1
fi
echo "the adaptive path reports ${adaptive_expansions_per_query} widening step per query: ok"

full_vector_selected="$(scalar "
    SELECT count(*) FROM pgcontext.query_execution_stats()
     WHERE collection_name = 'adaptive_docs' AND strategy = 'dense_exact'")"
if [[ "${full_vector_selected}" == "0" ]]; then
    echo "a disabled setting never read the full authoritative dimensions" >&2
    exit 1
fi
echo "a disabled setting reads the full authoritative dimensions: ok"

psql_db <<SQL
CREATE TABLE public.adaptive_uncertified (
    id bigint PRIMARY KEY,
    embedding vector(8) NOT NULL
);
INSERT INTO public.adaptive_uncertified
SELECT id, ARRAY[id, 1, 0, 0, 0, 0, 0, 0]::real[]::vector
  FROM pg_catalog.generate_series(1, 50) AS id;
SELECT pgcontext.create_collection('adaptive_uncertified', 'public.adaptive_uncertified');
SELECT pgcontext.register_vector('adaptive_uncertified', 'embedding', 'embedding', 8, 'l2');
SELECT pgcontext.backfill_points('adaptive_uncertified', 100);
SET pgcontext.adaptive_prefix_dimensions = 4;
SELECT count(*) FROM pgcontext.search(
    'adaptive_uncertified', '[1,1,0,0,0,0,0,0]'::vector, 5
);
SQL

wait_for_positive_scalar "
    SELECT count(*) FROM pgcontext.query_execution_stats()
     WHERE collection_name = 'adaptive_uncertified'" \
    "uncertified full-vector telemetry" >/dev/null
uncertified_prefix="$(scalar "
    SELECT count(*) FROM pgcontext.query_execution_stats()
     WHERE collection_name = 'adaptive_uncertified'
       AND strategy LIKE 'dense_adaptive_prefix%'")"
assert_equal "an uncertified collection never reads a prefix" "0" "${uncertified_prefix}"

# ---------------------------------------------------------------------------
# A corpus that cannot fund exhaustive widening must choose full-vector exact
# before doing prefix work. Ten thousand visible rows need the final exhaustive
# page plus the initial oversampled page, which exceeds the 10,000-candidate
# execution ceiling.
# ---------------------------------------------------------------------------

psql_db <<SQL
INSERT INTO public.adaptive_docs (id, embedding, bucket)
SELECT d.id,
       (SELECT pg_catalog.array_agg((c.v / sqrt(c.sq))::real)::vector
          FROM (SELECT v, pg_catalog.sum(v * v) OVER () AS sq
                  FROM (SELECT sin(d.id * 0.7 + k * 1.3)::real AS v
                          FROM pg_catalog.generate_series(1, 64) AS k) AS raw) AS c),
       'b' || (d.id % 5)::text
  FROM pg_catalog.generate_series(${ROW_COUNT} + 1, 10000) AS d(id);
SELECT pgcontext.backfill_points('adaptive_docs', 10000);
ANALYZE public.adaptive_docs;
SQL

fallback_baseline="$(ranked -1 7)"
fallback_observed="$(ranked 8 7)"
assert_equal "over-budget widening preserves full-vector order" \
    "${fallback_baseline}" "${fallback_observed}"
wait_for_positive_scalar "
    SELECT count(*) FROM pgcontext.query_execution_stats()
     WHERE collection_name = 'adaptive_docs'
       AND strategy = 'dense_exact_adaptive_candidate_budget'" \
    "adaptive full-vector fallback telemetry" >/dev/null
fallback_expansions="$(scalar "
    SELECT sum(total_expansions)
      FROM pgcontext.query_execution_stats()
     WHERE collection_name = 'adaptive_docs'
       AND strategy = 'dense_exact_adaptive_candidate_budget'")"
assert_equal "over-budget widening performs no prefix expansion" "0" "${fallback_expansions}"
fallback_termination="$(scalar "
    SELECT string_agg(DISTINCT adaptive_termination, ',' ORDER BY adaptive_termination)
      FROM pgcontext.query_execution_stats()
     WHERE collection_name = 'adaptive_docs'
       AND strategy = 'dense_exact_adaptive_candidate_budget'")"
assert_equal "over-budget widening reports its termination" \
    "candidate_budget" "${fallback_termination}"
echo "over-budget widening selects full-vector exact before prefix work: ok"

# ---------------------------------------------------------------------------
# Evidence for the promotion decision.
# ---------------------------------------------------------------------------

echo
echo "adaptive-dimension evidence (rows=${ROW_COUNT}, queries=${QUERY_COUNT}, top_k=10):"
psql_db -c "
    SELECT strategy,
           query_count,
           total_candidates,
           total_rechecks,
           adaptive_prefix_dimensions,
           adaptive_termination,
           round(avg_latency_ms::numeric, 3) AS avg_latency_ms
      FROM pgcontext.query_execution_stats()
     WHERE collection_name = 'adaptive_docs'
     ORDER BY strategy"

cleanup_database
echo "adaptive_prefix_recall: ok"
