#!/usr/bin/env bash
set -euo pipefail

# Phase 5 release gate: one million authoritative rows, three codec families,
# exact-source recall, latency, resident/index bytes, publication, REINDEX,
# VACUUM, and server-restart recovery. The context-test manifest is the sole
# threshold authority; this runner records its hash beside every raw sample.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${repo_root}"

pg_major="${PG_VERSION_FEATURE:-pg17}"
database="${DBNAME:-pgcontext_p5_quantized_1m}"
pg_host="${PGHOST:-localhost}"
pgrx_home="${P5_PGRX_HOME:-${PGRX_HOME:-${HOME}/.pgrx}}"
pg_port="${PGPORT:-28817}"
report_path="${P5_REPORT_PATH:-${repo_root}/.temporary_files/p5-quantized-hnsw-1m.tsv}"
manifest_path="${P5_MANIFEST_PATH:-${repo_root}/.temporary_files/p5-codec-manifest.tsv}"
resume="${P5_RESUME:-0}"
modes="${P5_MODES:-binary scalar pq}"

mkdir -p "$(dirname "${report_path}")" "$(dirname "${manifest_path}")"
cargo run --quiet -p context-test --bin p5_codec_manifest >"${manifest_path}"

manifest_value() {
  awk -F '\t' -v key="$1" '$1 == key { print $2; exit }' "${manifest_path}"
}

rows="$(manifest_value rows)"
dimensions="$(manifest_value dimensions)"
manifest_hash="$(manifest_value manifest_hash)"
query_count="$(manifest_value query_count)"
top_k="$(manifest_value top_k)"
seed="$(manifest_value seed)"
generator_revision="$(manifest_value generator_revision)"
query_ids="$(manifest_value query_ids)"

if [[ "${rows}" != "1000000" || "${dimensions}" != "32" || "${generator_revision}" != "2" ]]; then
  echo "P5 manifest is not the frozen 1M x 32 workload" >&2
  exit 64
fi

profile="$(cargo metadata --no-deps --format-version 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
release_library=""
for candidate in "${profile}/release/libpgcontext.dylib" "${profile}/release/libpgcontext.so"; do
  if [[ -f "${candidate}" ]]; then
    release_library="${candidate}"
    break
  fi
done
if [[ -z "${release_library}" ]]; then
  echo "P5 performance gate requires a release-built extension (cargo pgrx install --release ...)" >&2
  exit 64
fi
pgrx_config="${P5_PGRX_CONFIG:-${pgrx_home}/config.toml}"
pg_config_path="$(awk -F '"' -v key="${pg_major}" '$1 ~ "^[[:space:]]*" key "[[:space:]]*=" { print $2; exit }' "${pgrx_config}")"
if [[ -z "${pg_config_path}" || ! -x "${pg_config_path}" ]]; then
  echo "P5 cannot resolve ${pg_major} pg_config from ${pgrx_config}" >&2
  exit 64
fi
installed_library=""
for candidate in "$("${pg_config_path}" --pkglibdir)/pgcontext.dylib" "$("${pg_config_path}" --pkglibdir)/pgcontext.so"; do
  if [[ -f "${candidate}" ]]; then
    installed_library="${candidate}"
    break
  fi
done
if [[ -z "${installed_library}" ]] || ! cmp -s "${release_library}" "${installed_library}"; then
  echo "P5 installed extension does not match the current release artifact; run cargo pgrx install --release for ${pg_major}" >&2
  exit 64
fi
pg_ctl_path="$(dirname "${pg_config_path}")/pg_ctl"
pg_isready_path="$(dirname "${pg_config_path}")/pg_isready"
pg_data="${P5_PGDATA:-${pgrx_home}/data-${pg_major#pg}}"
if [[ ! -x "${pg_ctl_path}" || ! -x "${pg_isready_path}" || ! -d "${pg_data}" ]]; then
  echo "P5 cannot resolve pg_ctl, pg_isready, or PGDATA for ${pg_major}" >&2
  exit 64
fi

psql_base=(psql -X -v ON_ERROR_STOP=1 -h "${pg_host}" -p "${pg_port}")
if [[ "${resume}" != "1" ]]; then
"${psql_base[@]}" -d postgres \
  -c "DROP DATABASE IF EXISTS ${database} WITH (FORCE)" \
  -c "CREATE DATABASE ${database}"

"${psql_base[@]}" -d "${database}" \
  -v rows="${rows}" -v dimensions="${dimensions}" \
  -v manifest_hash="${manifest_hash}" -v query_count="${query_count}" \
  -v top_k="${top_k}" -v seed="${seed}" -v query_ids="${query_ids}" \
  -v generator_revision="${generator_revision}" <<'SQL'
CREATE EXTENSION pgcontext;
SET search_path = public, pgcontext;

CREATE FUNCTION public.p5_vector(
    point_id bigint,
    generator_seed bigint DEFAULT :seed::bigint
)
RETURNS pgcontext.vector
LANGUAGE plpgsql IMMUTABLE STRICT PARALLEL SAFE AS $$
DECLARE
    values real[] := ARRAY[]::real[];
    dimension integer;
BEGIN
    FOR dimension IN 1..32 LOOP
        values := array_append(
            values,
            (
                (
                    pg_catalog.hashint8extended(
                        point_id,
                        generator_seed + dimension::bigint
                    ) & 2147483647
                )::double precision / 1073741824.0 - 1.0
            )::real
        );
    END LOOP;
    RETURN values::pgcontext.vector;
END
$$;

CREATE TABLE public.p5_vectors (
    id bigint PRIMARY KEY,
    embedding pgcontext.vector(32) NOT NULL
);
INSERT INTO public.p5_vectors
SELECT id, public.p5_vector(id)
FROM generate_series(1, :rows::bigint) AS id;
ANALYZE public.p5_vectors;

CREATE TABLE public.p5_queries AS
SELECT row_number() OVER (ORDER BY id)::integer AS query_number,
       id AS query_id,
       embedding
FROM public.p5_vectors
WHERE id = ANY (pg_catalog.string_to_array(:'query_ids', ',')::bigint[])
ORDER BY id;
ALTER TABLE public.p5_queries ADD PRIMARY KEY (query_number);

CREATE TABLE public.p5_workload_manifest (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    manifest_hash text NOT NULL,
    generator_revision integer NOT NULL,
    generator_seed bigint NOT NULL,
    source_rows bigint NOT NULL,
    dimensions integer NOT NULL,
    query_ids text NOT NULL,
    query_count integer NOT NULL,
    top_k integer NOT NULL
);
INSERT INTO public.p5_workload_manifest(
    manifest_hash,
    generator_revision,
    generator_seed,
    source_rows,
    dimensions,
    query_ids,
    query_count,
    top_k
)
VALUES (
    :'manifest_hash',
    :generator_revision,
    :seed,
    :rows,
    :dimensions,
    :'query_ids',
    :query_count,
    :top_k
);

SET enable_indexscan = off;
SET enable_bitmapscan = off;
SET enable_seqscan = on;
CREATE TABLE public.p5_exact AS
SELECT query.query_number,
       row_number() OVER (PARTITION BY query.query_number ORDER BY candidate.distance, candidate.id)::integer AS rank,
       candidate.id AS point_id
FROM public.p5_queries AS query
CROSS JOIN LATERAL (
    SELECT item.id,
           item.embedding OPERATOR(pgcontext.<->) query.embedding AS distance
    FROM public.p5_vectors AS item
    ORDER BY item.embedding OPERATOR(pgcontext.<->) query.embedding, item.id
    LIMIT :top_k
) AS candidate;

CREATE TABLE public.p5_results (
    manifest_hash text NOT NULL,
    mode text PRIMARY KEY,
    rows bigint NOT NULL,
    dimensions integer NOT NULL,
    build_ms bigint,
    index_bytes bigint,
    resident_bytes bigint,
    minimum_recall double precision,
    p95_latency_ms double precision,
    parallel_segment_scans bigint,
    parallel_admission_denials bigint,
    max_segments_observed bigint,
    mapped_or_shared_published boolean DEFAULT false,
    vacuum_reindex_pass boolean DEFAULT false,
    restart_recovery_pass boolean DEFAULT false
);
SQL
else
  "${psql_base[@]}" -d "${database}" \
    -v manifest_hash="${manifest_hash}" -v generator_revision="${generator_revision}" \
    -v seed="${seed}" -v rows="${rows}" -v dimensions="${dimensions}" \
    -v query_ids="${query_ids}" -v query_count="${query_count}" \
    -v top_k="${top_k}" <<'SQL'
CREATE TEMP TABLE p5_runner_manifest AS
SELECT :'manifest_hash'::text AS manifest_hash,
       :generator_revision::integer AS generator_revision,
       :seed::bigint AS generator_seed,
       :rows::bigint AS source_rows,
       :dimensions::integer AS dimensions,
       :'query_ids'::text AS query_ids,
       :query_count::integer AS query_count,
       :top_k::integer AS top_k;

DO $resume$
DECLARE
    runner record;
    source_count bigint;
BEGIN
    SELECT * INTO STRICT runner FROM pg_temp.p5_runner_manifest;
    IF NOT EXISTS (
        SELECT 1
        FROM public.p5_workload_manifest
        WHERE singleton
          AND manifest_hash = runner.manifest_hash
          AND generator_revision = runner.generator_revision
          AND generator_seed = runner.generator_seed
          AND source_rows = runner.source_rows
          AND dimensions = runner.dimensions
          AND query_ids = runner.query_ids
          AND query_count = runner.query_count
          AND top_k = runner.top_k
    ) THEN
        RAISE EXCEPTION 'P5 resume workload manifest does not match the runner manifest';
    END IF;

    SELECT count(*) INTO source_count FROM public.p5_vectors;
    IF source_count <> runner.source_rows THEN
        RAISE EXCEPTION 'P5 resume source row count mismatch: database=%, runner=%',
            source_count, runner.source_rows;
    END IF;

    IF EXISTS (
        SELECT 1
        FROM public.p5_results
        WHERE manifest_hash IS DISTINCT FROM runner.manifest_hash
    ) THEN
        RAISE EXCEPTION 'P5 resume contains results from another workload manifest';
    END IF;
END
$resume$;

DO $queries$
DECLARE
    runner record;
    actual_count integer;
BEGIN
    SELECT * INTO STRICT runner FROM pg_temp.p5_runner_manifest;
    SELECT count(*) INTO actual_count FROM public.p5_queries;
    IF actual_count <> runner.query_count THEN
        RAISE EXCEPTION 'P5 resume frozen query count mismatch: database=%, runner=%',
            actual_count, runner.query_count;
    END IF;

    IF EXISTS (
        WITH expected AS (
            SELECT ordinality::integer AS query_number,
                   query_id::bigint AS query_id
            FROM unnest(pg_catalog.string_to_array(runner.query_ids, ',')::bigint[])
                 WITH ORDINALITY AS frozen(query_id, ordinality)
        )
        SELECT 1
        FROM expected
        FULL JOIN public.p5_queries AS actual
          USING (query_number, query_id)
        WHERE expected.query_number IS NULL
           OR actual.query_number IS NULL
           OR actual.embedding::text IS DISTINCT FROM
              public.p5_vector(expected.query_id, runner.generator_seed)::text
    ) THEN
        RAISE EXCEPTION 'P5 resume frozen query rows do not match the runner manifest';
    END IF;
END
$queries$;
SQL
fi

"${psql_base[@]}" -d "${database}" -v manifest_hash="${manifest_hash}" <<'SQL'
ALTER TABLE public.p5_results
    ADD COLUMN IF NOT EXISTS parallel_segment_scans bigint,
    ADD COLUMN IF NOT EXISTS parallel_admission_denials bigint,
    ADD COLUMN IF NOT EXISTS max_segments_observed bigint;
SQL

for mode in ${modes}; do
  case "${mode}" in
    binary) options="quantization = 'binary'" ;;
    scalar) options="quantization = 'scalar', scalar_min = -1.0, scalar_max = 1.0, scalar_levels = 256" ;;
    pq) options="quantization = 'pq', pq_subvector_dimensions = 8" ;;
  esac

  # A resumed lane must never inherit an index or a partial result from an
  # interrupted attempt. Source rows and the exact oracle are reusable; every
  # requested codec is rebuilt and remeasured from a clean artifact state.
  "${psql_base[@]}" -d "${database}" -v mode="${mode}" <<'SQL'
DROP INDEX IF EXISTS public.p5_quantized_idx;
DELETE FROM public.p5_results WHERE mode = :'mode';
SQL

  build_started="${SECONDS}"
  "${psql_base[@]}" -d "${database}" -c \
    "SET search_path = public, pgcontext; SET maintenance_work_mem = '4GB'; CREATE INDEX p5_quantized_idx ON public.p5_vectors USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_ops) WITH (${options})"
  build_ms="$(( (SECONDS - build_started) * 1000 ))"
  "${psql_base[@]}" -d "${database}" \
    -v mode="${mode}" -v rows="${rows}" -v dimensions="${dimensions}" \
    -v manifest_hash="${manifest_hash}" -v build_ms="${build_ms}" <<'SQL'
INSERT INTO public.p5_results(manifest_hash, mode, rows, dimensions, build_ms, index_bytes)
VALUES (:'manifest_hash', :'mode', :rows, :dimensions, :build_ms,
        pg_relation_size('public.p5_quantized_idx'));
SQL

  "${psql_base[@]}" -d "${database}" -v mode="${mode}" <<'SQL'
SET search_path = public, pgcontext;
SET pgcontext.hnsw_ef_search = 4096;
SET pgcontext.hnsw_candidate_budget = 10000;
SET pgcontext.hnsw_iterative_expansion_limit = 10000;
SET pgcontext.hnsw_shared_serving_budget_mb = 4096;
SET pgcontext.hnsw_mmap_serving_budget_mb = 4096;
SET pgcontext.hnsw_segment_parallel_workers = 4;
SET enable_indexscan = on;
SET enable_bitmapscan = off;
SET enable_seqscan = off;

CREATE TEMP TABLE p5_actual(query_number integer, rank integer, point_id bigint);
CREATE TEMP TABLE p5_latency(sample integer, elapsed_ms double precision);
DO $plan$
DECLARE plan_line text; plan_text text := '';
BEGIN
    FOR plan_line IN EXECUTE
        'EXPLAIN (COSTS OFF) SELECT id FROM public.p5_vectors ORDER BY embedding OPERATOR(pgcontext.<->) public.p5_vector(17), id LIMIT 10'
    LOOP
        plan_text := plan_text || E'\n' || plan_line;
    END LOOP;
    IF position('Index Scan using p5_quantized_idx' IN plan_text) = 0 THEN
        RAISE EXCEPTION 'P5 timed query did not use p5_quantized_idx: %', plan_text;
    END IF;
END
$plan$;
DO $gate$
DECLARE query record; warm_query pgcontext.vector; started timestamptz; sample integer := 0;
BEGIN
    SELECT embedding INTO STRICT warm_query
    FROM public.p5_queries
    WHERE query_number = 1;
    PERFORM item.id
    FROM public.p5_vectors AS item
    ORDER BY item.embedding OPERATOR(pgcontext.<->) warm_query, item.id
    LIMIT 10;

    FOR query IN SELECT * FROM public.p5_queries ORDER BY query_number LOOP
        sample := sample + 1;
        started := clock_timestamp();
        INSERT INTO p5_actual
        SELECT query.query_number,
               row_number() OVER (ORDER BY candidate.distance, candidate.id)::integer,
               candidate.id
        FROM (
            SELECT item.id,
                   item.embedding OPERATOR(pgcontext.<->) query.embedding AS distance
            FROM public.p5_vectors AS item
            ORDER BY item.embedding OPERATOR(pgcontext.<->) query.embedding, item.id
            LIMIT 10
        ) AS candidate;
        INSERT INTO p5_latency VALUES (
            sample,
            extract(epoch FROM clock_timestamp() - started) * 1000
        );
    END LOOP;
END
$gate$;

WITH recall AS (
    SELECT expected.query_number,
           count(*) FILTER (WHERE actual.point_id IS NOT NULL)::double precision / 10.0 AS value
    FROM public.p5_exact AS expected
    LEFT JOIN p5_actual AS actual
      ON actual.query_number = expected.query_number
     AND actual.point_id = expected.point_id
    GROUP BY expected.query_number
), latency AS (
    SELECT percentile_cont(0.95) WITHIN GROUP (ORDER BY elapsed_ms) AS value
    FROM p5_latency
), serving AS (
    SELECT last_pack_bytes,
           parallel_segment_scans,
           parallel_admission_denials,
           max_segments_observed,
           shared_publishes > 0 OR mapped_publishes > 0 AS published
    FROM pgcontext.hnsw_serving_stats()
)
UPDATE public.p5_results
SET resident_bytes = serving.last_pack_bytes,
    minimum_recall = (SELECT min(value) FROM recall),
    p95_latency_ms = latency.value,
    parallel_segment_scans = serving.parallel_segment_scans,
    parallel_admission_denials = serving.parallel_admission_denials,
    max_segments_observed = serving.max_segments_observed,
    mapped_or_shared_published = serving.published
FROM latency, serving
WHERE mode = :'mode';

VACUUM (ANALYZE) public.p5_vectors;
SET maintenance_work_mem = '4GB';
REINDEX INDEX public.p5_quantized_idx;
SELECT id
FROM public.p5_vectors
ORDER BY embedding OPERATOR(pgcontext.<->) public.p5_vector(17), id
LIMIT 10;
UPDATE public.p5_results SET vacuum_reindex_pass = true WHERE mode = :'mode';
SQL

  # Use an immediate stop so this lane proves WAL/crash recovery and cannot
  # hang indefinitely behind a graceful shutdown checkpoint.
  "${pg_ctl_path}" -D "${pg_data}" stop -m immediate -w -t 60
  cargo pgrx start "${pg_major}"
  ready=0
  for _attempt in $(seq 1 60); do
    if "${pg_isready_path}" -h "${pg_host}" -p "${pg_port}" -d "${database}" >/dev/null 2>&1; then
      ready=1
      break
    fi
    sleep 1
  done
  if [[ "${ready}" != "1" ]]; then
    echo "P5 ${pg_major} cluster did not recover within 60 seconds" >&2
    exit 1
  fi

  "${psql_base[@]}" -d "${database}" -v mode="${mode}" <<'SQL'
SET search_path = public, pgcontext;
SET pgcontext.hnsw_ef_search = 4096;
SET pgcontext.hnsw_candidate_budget = 10000;
SET pgcontext.hnsw_iterative_expansion_limit = 10000;
SET pgcontext.hnsw_shared_serving_budget_mb = 4096;
SET pgcontext.hnsw_mmap_serving_budget_mb = 4096;
SET pgcontext.hnsw_segment_parallel_workers = 4;
SET enable_indexscan = on;
SET enable_seqscan = off;
DO $gate$
DECLARE actual bigint[]; expected bigint[];
BEGIN
    SELECT array_agg(point_id ORDER BY rank) INTO expected
    FROM public.p5_exact WHERE query_number = 1;
    SELECT array_agg(id ORDER BY distance, id) INTO actual
    FROM (
        SELECT id, embedding OPERATOR(pgcontext.<->) public.p5_vector(17) AS distance
        FROM public.p5_vectors
        ORDER BY embedding OPERATOR(pgcontext.<->) public.p5_vector(17), id
        LIMIT 10
    ) AS ranked;
    IF actual IS DISTINCT FROM expected THEN
        RAISE EXCEPTION 'restart exact-rerank mismatch';
    END IF;
END
$gate$;
UPDATE public.p5_results SET restart_recovery_pass = true WHERE mode = :'mode';
DROP INDEX public.p5_quantized_idx;
SQL
done

"${psql_base[@]}" -d "${database}" <<'SQL'
DROP TABLE IF EXISTS public.p5_thresholds;
CREATE TABLE public.p5_thresholds (
    mode text PRIMARY KEY,
    minimum_recall double precision,
    maximum_p95_latency_ms double precision,
    maximum_resident_bytes bigint,
    maximum_index_bytes bigint,
    maximum_build_seconds bigint
);
SQL

while IFS=$'\t' read -r kind mode minimum_recall maximum_latency maximum_resident maximum_index maximum_build; do
  if [[ "${kind}" == "codec" ]]; then
    "${psql_base[@]}" -d "${database}" -v mode="${mode}" \
      -v minimum_recall="${minimum_recall}" -v maximum_latency="${maximum_latency}" \
      -v maximum_resident="${maximum_resident}" -v maximum_index="${maximum_index}" \
      -v maximum_build="${maximum_build}" <<'SQL'
INSERT INTO public.p5_thresholds VALUES (
    :'mode', :minimum_recall, :maximum_latency,
    :maximum_resident, :maximum_index, :maximum_build
);
SQL
  fi
done <"${manifest_path}"

"${psql_base[@]}" -d "${database}" -v manifest_hash="${manifest_hash}" <<'SQL'

CREATE TEMP TABLE p5_expected_manifest AS
SELECT :'manifest_hash'::text AS manifest_hash;

DO $gate$
DECLARE result record; threshold record;
BEGIN
    IF (SELECT array_agg(mode ORDER BY mode) FROM public.p5_results)
       IS DISTINCT FROM
       (SELECT array_agg(mode ORDER BY mode) FROM public.p5_thresholds) THEN
        RAISE EXCEPTION 'P5 result codecs do not exactly match the frozen manifest';
    END IF;
    IF EXISTS (
        SELECT 1
        FROM public.p5_results
        WHERE manifest_hash IS DISTINCT FROM
              (SELECT manifest_hash FROM pg_temp.p5_expected_manifest)
    ) THEN
        RAISE EXCEPTION 'P5 retained result manifest hash does not match the frozen workload';
    END IF;
    FOR result IN SELECT * FROM public.p5_results LOOP
        SELECT minimum_recall,
               maximum_p95_latency_ms,
               maximum_resident_bytes,
               maximum_index_bytes,
               maximum_build_seconds
        INTO threshold
        FROM public.p5_thresholds
        WHERE mode = result.mode;
        IF result.minimum_recall IS NULL
           OR result.p95_latency_ms IS NULL
           OR result.resident_bytes IS NULL
           OR result.index_bytes IS NULL
           OR result.build_ms IS NULL
           OR result.parallel_segment_scans IS NULL
           OR result.parallel_admission_denials IS NULL
           OR result.max_segments_observed IS NULL
           OR result.minimum_recall < threshold.minimum_recall
           OR result.p95_latency_ms > threshold.maximum_p95_latency_ms
           OR result.resident_bytes > threshold.maximum_resident_bytes
           OR result.index_bytes > threshold.maximum_index_bytes
           OR result.build_ms > threshold.maximum_build_seconds * 1000
           OR result.parallel_segment_scans = 0
           OR result.max_segments_observed < 3
           OR result.resident_bytes <= result.index_bytes
           OR NOT result.mapped_or_shared_published
           OR NOT result.vacuum_reindex_pass
           OR NOT result.restart_recovery_pass THEN
            RAISE EXCEPTION 'P5 codec gate failed for %: %', result.mode, row_to_json(result);
        END IF;
    END LOOP;
END
$gate$;
SQL

"${psql_base[@]}" -d "${database}" -c \
  "COPY (SELECT * FROM public.p5_results ORDER BY mode) TO STDOUT WITH (FORMAT csv, HEADER true)" \
  >"${report_path}"

echo "P5 quantized HNSW 1M gate passed: ${report_path}"
