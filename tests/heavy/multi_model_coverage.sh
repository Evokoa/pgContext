#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_multi_model_coverage}"
ROW_COUNT="${ROW_COUNT:-10000}"
HARDWARE_ARCH="$(uname -m 2>/dev/null || printf unknown)"
HARDWARE_OS="$(uname -s 2>/dev/null || printf unknown)"
HARDWARE_CPUS="$(getconf _NPROCESSORS_ONLN 2>/dev/null || printf unknown)"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

if [[ ! "${ROW_COUNT}" =~ ^[1-9][0-9]*$ ]]; then
    echo "ROW_COUNT must be a positive integer" >&2
    exit 2
fi
if (( ROW_COUNT >= 100000 )); then
    SETWISE_POINT_SEED=true
else
    SETWISE_POINT_SEED=false
fi

start_and_install_extension
reset_database

psql_db \
    -v row_count="${ROW_COUNT}" \
    -v setwise_point_seed="${SETWISE_POINT_SEED}" \
    -v hardware_arch="${HARDWARE_ARCH}" \
    -v hardware_os="${HARDWARE_OS}" \
    -v hardware_cpus="${HARDWARE_CPUS}" <<'SQL'
CREATE EXTENSION pgcontext;

CREATE TABLE public.multi_model_docs (
    id bigint PRIMARY KEY,
    tenant text NOT NULL,
    coverage_class text NOT NULL,
    source_version bigint NOT NULL DEFAULT 1,
    legacy_version bigint,
    modern_version bigint,
    legacy pgcontext.vector(4),
    modern pgcontext.vector(8)
);

INSERT INTO public.multi_model_docs (
    id, tenant, coverage_class, legacy_version, modern_version, legacy, modern
)
SELECT id,
       CASE WHEN id % 2 = 0 THEN 'even' ELSE 'odd' END,
       CASE
           WHEN id % 5 <> 0 AND id % 3 = 0 THEN 'a_only'
           WHEN id % 5 = 0 AND id % 3 <> 0 THEN 'b_only'
           WHEN id % 5 <> 0 AND id % 3 <> 0 THEN 'both'
           ELSE 'neither'
       END,
       CASE WHEN id % 5 <> 0 THEN 1 END,
       CASE WHEN id % 3 <> 0 THEN 1 END,
       CASE WHEN id % 5 <> 0
            THEN format(
                '[%s,%s,%s,%s]',
                id % 997, (id * 31) % 991, (id * 17) % 983, (id * 7) % 977
            )::pgcontext.vector
       END,
       CASE WHEN id % 3 <> 0
            THEN format(
                '[%s,%s,%s,%s,%s,%s,%s,%s]',
                (id * 13) % 991, id % 983, (id * id) % 977, (id * 43) % 971,
                (id * 5) % 967, (id * 19) % 953, (id * 29) % 947, id % 941
            )::pgcontext.vector
       END
  FROM pg_catalog.generate_series(1::bigint, :row_count::bigint) AS id;

SELECT pgcontext.create_collection('multi_model_docs', 'public.multi_model_docs');
SELECT * FROM pgcontext.configure_collection_limits(
    'multi_model_docs', true,
    NULL, NULL, NULL, NULL, NULL, NULL, 60000, NULL
);
SELECT pgcontext.register_vector('multi_model_docs', 'legacy', 'legacy', 4, 'l2');
SELECT pgcontext.register_filter_column('multi_model_docs', 'tenant', 'tenant');
SELECT pgcontext.register_filter_column(
    'multi_model_docs', 'coverage_class', 'coverage_class'
);

\if :setwise_point_seed
BEGIN;
ALTER TABLE pgcontext._collection_points
    DISABLE TRIGGER pgcontext_capture_build_point_delta;
INSERT INTO pgcontext._collection_points (collection_id, source_key)
SELECT collections.collection_id, source.id::text
  FROM pgcontext._collections AS collections
  CROSS JOIN public.multi_model_docs AS source
 WHERE collections.collection_name = 'multi_model_docs'
 ORDER BY source.id;
ALTER TABLE pgcontext._collection_points
    ENABLE TRIGGER pgcontext_capture_build_point_delta;
UPDATE pgcontext._collection_source_revisions AS revisions
   SET source_version = :row_count::bigint + 1,
       updated_at = pg_catalog.now()
  FROM pgcontext._collections AS collections
 WHERE collections.collection_id = revisions.collection_id
   AND collections.collection_name = 'multi_model_docs';
COMMIT;
\else
SELECT pgcontext.backfill_points('multi_model_docs', :row_count + 1);
\endif

SET maintenance_work_mem = '4GB';
CREATE INDEX multi_model_docs_legacy_hnsw
    ON public.multi_model_docs
    USING pgcontext_hnsw (legacy pgcontext.vector_hnsw_ops);
CREATE INDEX multi_model_docs_modern_hnsw
    ON public.multi_model_docs
    USING pgcontext_hnsw (modern pgcontext.vector_hnsw_ops);
RESET maintenance_work_mem;

SELECT pgcontext.register_embedding_profile(
    'multi_model_docs', 'legacy_v1', 'legacy',
    'public.multi_model_docs_legacy_hnsw',
    jsonb_build_object(
        'representation', 'dense', 'dimensions', 4,
        'normalization', 'none', 'metric', 'l2',
        'provider', 'fixture', 'model', 'legacy', 'revision', '1',
        'input_template', '{text}', 'output_template', '{vector}',
        'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
        'configuration_hash', '0123456789abcdef',
        'source_version_column', 'source_version',
        'embedding_version_column', 'legacy_version'
    ),
    'active'
);
SELECT pgcontext.register_embedding_profile(
    'multi_model_docs', 'modern_v2', 'modern',
    'public.multi_model_docs_modern_hnsw',
    jsonb_build_object(
        'representation', 'dense', 'dimensions', 8,
        'normalization', 'none', 'metric', 'l2',
        'provider', 'fixture', 'model', 'modern', 'revision', '2',
        'input_template', '{text}', 'output_template', '{vector}',
        'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
        'configuration_hash', 'fedcba9876543210',
        'source_version_column', 'source_version',
        'embedding_version_column', 'modern_version'
    ),
    'active'
);

CREATE TEMP TABLE p11_queries AS
SELECT query_id,
       seed,
       pg_catalog.format(
           '[%s,%s,%s,%s]',
           seed % 997, (seed * 31) % 991, (seed * 17) % 983, (seed * 7) % 977
       ) AS legacy_query,
       pg_catalog.format(
           '[%s,%s,%s,%s,%s,%s,%s,%s]',
           (seed * 13) % 991, seed % 983, (seed * seed) % 977, (seed * 43) % 971,
           (seed * 5) % 967, (seed * 19) % 953, (seed * 29) % 947, seed % 941
       ) AS modern_query
  FROM (
      SELECT query_id,
             GREATEST(
                 1::bigint,
                 (:row_count::bigint * query_id) / 9
             ) AS seed
        FROM pg_catalog.generate_series(1, 8) AS query_id
  ) AS held_out;

CREATE TEMP TABLE p11_meta AS
SELECT pg_catalog.md5(
           'p11-mixed-model-spaces-v2|rows=' || :row_count::text
           || '|coverage=mod5/mod3|legacy=4d-modular|modern=8d-independent-modular'
       ) AS dataset_hash,
       pg_catalog.md5(
           'p11-held-out-eight-v2|'
           || pg_catalog.string_agg(
                  query_id::text || ':' || legacy_query || ':' || modern_query,
                  '|' ORDER BY query_id
              )
       ) AS workload_hash
  FROM p11_queries;

CREATE TEMP TABLE p11_oracle (
    query_id integer NOT NULL,
    scope text NOT NULL,
    id bigint NOT NULL,
    PRIMARY KEY (query_id, scope, id)
);

INSERT INTO p11_oracle (query_id, scope, id)
SELECT query.query_id, scope.scope, relevant.id
  FROM p11_queries AS query
  CROSS JOIN (VALUES ('full'), ('even')) AS scope(scope)
  CROSS JOIN LATERAL (
      (SELECT source.id
         FROM public.multi_model_docs AS source
        WHERE source.legacy IS NOT NULL
          AND source.legacy_version = source.source_version
          AND (scope.scope <> 'even' OR source.tenant = 'even')
        ORDER BY source.legacy OPERATOR(pgcontext.<->)
                     query.legacy_query::pgcontext.vector,
                 source.id
        LIMIT 20)
      UNION
      (SELECT source.id
         FROM public.multi_model_docs AS source
        WHERE source.modern IS NOT NULL
          AND source.modern_version = source.source_version
          AND (scope.scope <> 'even' OR source.tenant = 'even')
        ORDER BY source.modern OPERATOR(pgcontext.<->)
                     query.modern_query::pgcontext.vector,
                 source.id
        LIMIT 20)
  ) AS relevant;

INSERT INTO p11_oracle (query_id, scope, id)
SELECT query.query_id, 'legacy', relevant.id
  FROM p11_queries AS query
  CROSS JOIN LATERAL (
      SELECT source.id
        FROM public.multi_model_docs AS source
       WHERE source.legacy IS NOT NULL
         AND source.legacy_version = source.source_version
       ORDER BY source.legacy OPERATOR(pgcontext.<->)
                    query.legacy_query::pgcontext.vector,
                source.id
       LIMIT 20
  ) AS relevant;

CREATE TEMP TABLE p11_raw_reports (
    query_id integer NOT NULL,
    curve text NOT NULL,
    elapsed_us bigint NOT NULL,
    report jsonb NOT NULL,
    PRIMARY KEY (query_id, curve)
);

DO $held_out$
DECLARE
    query p11_queries%ROWTYPE;
    report jsonb;
    started_at timestamptz;
BEGIN
    FOR query IN SELECT * FROM p11_queries ORDER BY query_id LOOP
        started_at := pg_catalog.clock_timestamp();
        report := pgcontext.query_multi_model(
            'multi_model_docs',
            pg_catalog.jsonb_build_array(pg_catalog.jsonb_build_object(
                'profile', 'legacy_v1',
                'configuration_hash', '0123456789abcdef',
                'query', query.legacy_query,
                'limit', 101,
                'weight', 1.0
            )),
            NULL, 20, 60, 102, true
        );
        INSERT INTO p11_raw_reports VALUES (
            query.query_id, 'a_only',
            (extract(epoch FROM pg_catalog.clock_timestamp() - started_at)
                * 1000000)::bigint,
            report
        );

        started_at := pg_catalog.clock_timestamp();
        report := pgcontext.query_multi_model(
            'multi_model_docs',
            pg_catalog.jsonb_build_array(pg_catalog.jsonb_build_object(
                'profile', 'modern_v2',
                'configuration_hash', 'fedcba9876543210',
                'query', query.modern_query,
                'limit', 101,
                'weight', 1.0
            )),
            NULL, 20, 60, 102, true
        );
        INSERT INTO p11_raw_reports VALUES (
            query.query_id, 'b_only',
            (extract(epoch FROM pg_catalog.clock_timestamp() - started_at)
                * 1000000)::bigint,
            report
        );

        started_at := pg_catalog.clock_timestamp();
        report := pgcontext.query_multi_model(
            'multi_model_docs',
            pg_catalog.jsonb_build_array(
                pg_catalog.jsonb_build_object(
                    'profile', 'legacy_v1',
                    'configuration_hash', '0123456789abcdef',
                    'query', query.legacy_query,
                    'limit', 50,
                    'weight', 1.0
                ),
                pg_catalog.jsonb_build_object(
                    'profile', 'modern_v2',
                    'configuration_hash', 'fedcba9876543210',
                    'query', query.modern_query,
                    'limit', 50,
                    'weight', 1.0
                )
            ),
            NULL, 20, 60, 102, true
        );
        INSERT INTO p11_raw_reports VALUES (
            query.query_id, 'fused',
            (extract(epoch FROM pg_catalog.clock_timestamp() - started_at)
                * 1000000)::bigint,
            report
        );

        started_at := pg_catalog.clock_timestamp();
        report := pgcontext.query_multi_model(
            'multi_model_docs',
            pg_catalog.jsonb_build_array(
                pg_catalog.jsonb_build_object(
                    'profile', 'legacy_v1',
                    'configuration_hash', '0123456789abcdef',
                    'query', query.legacy_query,
                    'limit', 50,
                    'weight', 1.0
                ),
                pg_catalog.jsonb_build_object(
                    'profile', 'modern_v2',
                    'configuration_hash', 'fedcba9876543210',
                    'query', query.modern_query,
                    'limit', 50,
                    'weight', 1.0
                )
            ),
            '{"must":[{"key":"tenant","match":"even"}]}'::jsonb,
            20, 60, 102, true
        );
        INSERT INTO p11_raw_reports VALUES (
            query.query_id, 'partial',
            (extract(epoch FROM pg_catalog.clock_timestamp() - started_at)
                * 1000000)::bigint,
            report
        );
    END LOOP;
END
$held_out$;

SELECT pgcontext.set_embedding_profile_lifecycle(
    'multi_model_docs', 'modern_v2', 'draining'
);
SELECT pgcontext.set_embedding_profile_lifecycle(
    'multi_model_docs', 'modern_v2', 'retired'
);

DO $degraded$
DECLARE
    query p11_queries%ROWTYPE;
    report jsonb;
    started_at timestamptz;
BEGIN
    FOR query IN SELECT * FROM p11_queries ORDER BY query_id LOOP
        started_at := pg_catalog.clock_timestamp();
        report := pgcontext.query_multi_model(
            'multi_model_docs',
            pg_catalog.jsonb_build_array(
                pg_catalog.jsonb_build_object(
                    'profile', 'legacy_v1',
                    'configuration_hash', '0123456789abcdef',
                    'query', query.legacy_query,
                    'limit', 50,
                    'weight', 1.0
                ),
                pg_catalog.jsonb_build_object(
                    'profile', 'modern_v2',
                    'configuration_hash', 'fedcba9876543210',
                    'query', query.modern_query,
                    'limit', 50,
                    'weight', 1.0
                )
            ),
            NULL, 20, 60, 102, false
        );
        INSERT INTO p11_raw_reports VALUES (
            query.query_id, 'degraded',
            (extract(epoch FROM pg_catalog.clock_timestamp() - started_at)
                * 1000000)::bigint,
            report
        );
    END LOOP;
END
$degraded$;

CREATE TEMP TABLE p11_samples AS
SELECT raw.query_id,
       raw.curve,
       CASE raw.curve WHEN 'partial' THEN 'even'
                      WHEN 'degraded' THEN 'legacy'
                      ELSE 'full' END AS oracle_scope,
       raw.elapsed_us,
       raw.report,
       raw.report->>'completion' AS completion,
       COALESCE(pg_catalog.jsonb_array_length(raw.report->'results'), 0)
           AS result_count,
       pg_catalog.string_agg(
           result.value->>'source_key', ',' ORDER BY result.ordinality
       ) FILTER (WHERE result.value IS NOT NULL) AS result_signature,
       pg_catalog.count(result.value) FILTER (
           WHERE EXISTS (
               SELECT 1
                 FROM p11_oracle AS oracle
                WHERE oracle.query_id = raw.query_id
                  AND oracle.scope = CASE raw.curve WHEN 'partial' THEN 'even'
                                                        WHEN 'degraded' THEN 'legacy'
                                                        ELSE 'full' END
                  AND oracle.id = (result.value->>'source_key')::bigint
           )
       )::integer AS hits,
       COALESCE(
           pg_catalog.sum(pg_catalog.jsonb_array_length(result.value->'contributions')),
           0
       )::integer AS contributions,
       COALESCE(work.candidates, 0)::integer AS candidates,
       COALESCE(work.rechecks, 0)::integer AS rechecks,
       COALESCE((raw.report#>>'{budget_usage,comparisons}')::bigint, 0)
           AS comparisons
  FROM p11_raw_reports AS raw
  LEFT JOIN LATERAL pg_catalog.jsonb_array_elements(raw.report->'results')
      WITH ORDINALITY AS result(value, ordinality)
    ON true
  LEFT JOIN LATERAL (
      SELECT COALESCE(pg_catalog.sum((branch->>'candidate_count')::integer), 0)
                 AS candidates,
             COALESCE(pg_catalog.sum((branch->>'recheck_count')::integer), 0)
                 AS rechecks
        FROM pg_catalog.jsonb_array_elements(raw.report->'branches') AS branch
  ) AS work ON true
 GROUP BY raw.query_id, raw.curve, raw.elapsed_us, raw.report, work.candidates, work.rechecks;

CREATE TEMP TABLE p11_coverage AS
SELECT pg_catalog.count(*) FILTER (WHERE coverage_class = 'a_only') AS a_only,
       pg_catalog.count(*) FILTER (WHERE coverage_class = 'b_only') AS b_only,
       pg_catalog.count(*) FILTER (WHERE coverage_class = 'both') AS both,
       pg_catalog.count(*) FILTER (WHERE coverage_class = 'neither') AS neither
  FROM public.multi_model_docs;

CREATE TEMP TABLE p11_decision AS
SELECT NOT EXISTS (
           SELECT 1
             FROM p11_samples AS fused
             JOIN p11_samples AS a
               ON a.query_id = fused.query_id AND a.curve = 'a_only'
             JOIN p11_samples AS b
               ON b.query_id = fused.query_id AND b.curve = 'b_only'
            WHERE fused.curve = 'fused'
              AND fused.hits < GREATEST(a.hits, b.hits)
       ) AS promote,
       pg_catalog.sum(hits) FILTER (WHERE curve = 'fused')::integer AS fused_hits,
       pg_catalog.sum(hits) FILTER (WHERE curve = 'a_only')::integer AS a_hits,
       pg_catalog.sum(hits) FILTER (WHERE curve = 'b_only')::integer AS b_hits,
       pg_catalog.max(candidates) FILTER (WHERE curve = 'fused')::integer
           AS fused_candidates,
       pg_catalog.max(rechecks) FILTER (WHERE curve = 'fused')::integer
           AS fused_rechecks,
       pg_catalog.sum(contributions) FILTER (WHERE curve = 'fused')::integer
           AS fused_contributions
  FROM p11_samples;

COPY (
    SELECT pg_catalog.format(
        'multi_model_sample dataset_hash=%s workload_hash=%s query=%s curve=%s hits=%s results=%s candidates=%s rechecks=%s comparisons=%s contributions=%s elapsed_us=%s completion=%s',
        meta.dataset_hash, meta.workload_hash, sample.query_id, sample.curve,
        sample.hits, sample.result_count, sample.candidates, sample.rechecks,
        sample.comparisons, sample.contributions, sample.elapsed_us, sample.completion
    )
      FROM p11_samples AS sample
      CROSS JOIN p11_meta AS meta
     ORDER BY sample.query_id, sample.curve
) TO STDOUT;

COPY (
    SELECT pg_catalog.format(
        'multi_model_quality rows=%s coverage=%s/%s/%s/%s recall=%s/%s/%s work=%s/%s contributions=%s',
        :row_count, coverage.a_only, coverage.b_only, coverage.both, coverage.neither,
        decision.fused_hits, decision.a_hits, decision.b_hits,
        decision.fused_candidates, decision.fused_rechecks,
        decision.fused_contributions
    )
      FROM p11_coverage AS coverage
      CROSS JOIN p11_decision AS decision
) TO STDOUT;

COPY (
    SELECT pg_catalog.format(
        'multi_model_quality_contract dataset_hash=%s workload_hash=%s queries=8 weights=1/1 top_k=20 rrf_k=60 budget=102 decision=%s curves=a_only,b_only,fused,partial,degraded',
        meta.dataset_hash, meta.workload_hash,
        CASE WHEN decision.promote THEN 'pass' ELSE 'no_go' END
    )
      FROM p11_meta AS meta
      CROSS JOIN p11_decision AS decision
) TO STDOUT;

COPY (
    SELECT pg_catalog.format(
        'multi_model_latency_cost dataset_hash=%s workload_hash=%s curve=%s samples=%s latency_us_p50=%s latency_us_p95=%s latency_us_max=%s candidates_avg=%s rechecks_avg=%s comparisons_avg=%s',
        meta.dataset_hash, meta.workload_hash, sample.curve, pg_catalog.count(*),
        pg_catalog.round(pg_catalog.percentile_cont(0.50) WITHIN GROUP
            (ORDER BY sample.elapsed_us)::numeric, 2),
        pg_catalog.round(pg_catalog.percentile_cont(0.95) WITHIN GROUP
            (ORDER BY sample.elapsed_us)::numeric, 2),
        pg_catalog.max(sample.elapsed_us),
        pg_catalog.round(pg_catalog.avg(sample.candidates), 2),
        pg_catalog.round(pg_catalog.avg(sample.rechecks), 2),
        pg_catalog.round(pg_catalog.avg(sample.comparisons), 2)
    )
      FROM p11_samples AS sample
      CROSS JOIN p11_meta AS meta
     GROUP BY meta.dataset_hash, meta.workload_hash, sample.curve
     ORDER BY sample.curve
) TO STDOUT;

COPY (
    SELECT pg_catalog.format(
        'multi_model_environment dataset_hash=%s workload_hash=%s pg_major=%s pg_version_num=%s hardware_os=%s hardware_arch=%s hardware_cpus=%s guc_work_mem=%s guc_maintenance_work_mem=%s guc_effective_cache_size=%s guc_parallel=%s guc_jit=%s',
        meta.dataset_hash, meta.workload_hash,
        pg_catalog.current_setting('server_version_num')::integer / 10000,
        pg_catalog.current_setting('server_version_num'),
        :'hardware_os', :'hardware_arch', :'hardware_cpus',
        pg_catalog.current_setting('work_mem'),
        pg_catalog.current_setting('maintenance_work_mem'),
        pg_catalog.current_setting('effective_cache_size'),
        pg_catalog.current_setting('max_parallel_workers_per_gather'),
        pg_catalog.current_setting('jit')
    )
      FROM p11_meta AS meta
) TO STDOUT;

DO $certify$
DECLARE
    coverage p11_coverage%ROWTYPE;
    decision p11_decision%ROWTYPE;
BEGIN
    SELECT * INTO STRICT coverage FROM p11_coverage;
    SELECT * INTO STRICT decision FROM p11_decision;
    IF coverage.a_only = 0 OR coverage.b_only = 0
       OR coverage.both = 0 OR coverage.neither = 0 THEN
        RAISE EXCEPTION 'mixed coverage classes are incomplete: %', row_to_json(coverage);
    END IF;
    IF (SELECT count(*) FROM p11_samples) <> 40
       OR EXISTS (
           SELECT 1
             FROM (VALUES ('a_only'), ('b_only'), ('fused'), ('partial'), ('degraded'))
                  AS expected(curve)
            WHERE (SELECT count(*) FROM p11_samples WHERE curve = expected.curve) <> 8
       ) THEN
        RAISE EXCEPTION 'held-out quality report did not contain 8 samples for all curves';
    END IF;
    IF EXISTS (
        SELECT 1 FROM p11_samples
         WHERE candidates > 102 OR rechecks > 101 OR result_count <> 20
    ) THEN
        RAISE EXCEPTION 'quality/cost sample exceeded a frozen bound';
    END IF;
    IF EXISTS (
        SELECT 1 FROM p11_samples
         WHERE (curve = 'degraded' AND completion <> 'degraded')
            OR (curve <> 'degraded' AND completion <> 'complete')
    ) THEN
        RAISE EXCEPTION 'curve completion contract was not preserved';
    END IF;
    IF NOT EXISTS (
        SELECT 1
         FROM p11_samples AS a
          JOIN p11_samples AS b USING (query_id)
         WHERE a.curve = 'a_only' AND b.curve = 'b_only'
           AND a.result_signature IS DISTINCT FROM b.result_signature
    ) THEN
        RAISE EXCEPTION 'model spaces produced no distinguishable held-out result';
    END IF;
    IF EXISTS (
        SELECT 1
          FROM p11_raw_reports AS raw
          CROSS JOIN LATERAL pg_catalog.jsonb_array_elements(raw.report->'results')
               AS result(value)
          CROSS JOIN LATERAL pg_catalog.jsonb_array_elements(
               result.value->'contributions'
          ) AS contribution(value)
         WHERE raw.curve IN ('fused', 'partial')
           AND (contribution.value->>'weight')::numeric <> 1.0
    ) THEN
        RAISE EXCEPTION 'equal 1:1 profile weights were not preserved in provenance';
    END IF;
    IF EXISTS (
        SELECT 1
          FROM p11_raw_reports AS raw
         WHERE raw.curve = 'degraded'
           AND NOT (raw.report->'missing_profiles' @> '[{"profile":"modern_v2"}]'::jsonb)
    ) THEN
        RAISE EXCEPTION 'degraded curve did not explicitly name the unavailable profile';
    END IF;
    IF NOT decision.promote THEN
        RAISE NOTICE 'P11 no-go recorded: equal-weight fused quality was worse than a stronger single profile';
    END IF;
END
$certify$;
SQL

printf 'multi_model_coverage: ok (%s mixed-coverage rows)\n' "${ROW_COUNT}"
