#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_multi_model_rls_acl}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

require_simple_identifier "${DBNAME}" "database"
ROLE_SUFFIX="${DBNAME:0:20}_${PGPORT}"
TABLE_OWNER_ROLE="${TABLE_OWNER_ROLE:-pgctx_mm_table_${ROLE_SUFFIX}}"
OWNER_ROLE="${OWNER_ROLE:-pgctx_mm_owner_${ROLE_SUFFIX}}"
DENIED_ROLE="${DENIED_ROLE:-pgctx_mm_denied_${ROLE_SUFFIX}}"

for role in "${TABLE_OWNER_ROLE}" "${OWNER_ROLE}" "${DENIED_ROLE}"; do
    require_simple_identifier "${role}" "role"
done

grant_api() {
    local role="$1"
    psql_db <<SQL
GRANT USAGE ON SCHEMA public, pgcontext TO ${role};
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA pgcontext TO ${role};
GRANT USAGE ON TYPE pgcontext.vector TO ${role};
SQL
}

start_and_install_extension
reset_database
drop_role_if_exists "${DENIED_ROLE}"
drop_role_if_exists "${OWNER_ROLE}"
drop_role_if_exists "${TABLE_OWNER_ROLE}"
create_login_role "${TABLE_OWNER_ROLE}"
create_login_role "${OWNER_ROLE}"
create_login_role "${DENIED_ROLE}"

psql_db <<SQL
CREATE EXTENSION pgcontext;
GRANT CREATE ON SCHEMA public TO ${TABLE_OWNER_ROLE};
SQL
grant_api "${TABLE_OWNER_ROLE}"
grant_api "${OWNER_ROLE}"
grant_api "${DENIED_ROLE}"

psql_db <<SQL
SET SESSION AUTHORIZATION ${TABLE_OWNER_ROLE};
CREATE TABLE public.multi_model_acl_docs (
    id bigint PRIMARY KEY,
    tenant text NOT NULL,
    source_version bigint NOT NULL,
    legacy_version bigint NOT NULL,
    modern_version bigint NOT NULL,
    legacy pgcontext.vector(2) NOT NULL,
    modern pgcontext.vector(3) NOT NULL
);
INSERT INTO public.multi_model_acl_docs VALUES
    (1, 'acme', 1, 1, 1, '[0,0]', '[0,0,0]'),
    (2, 'acme', 1, 1, 1, '[1,0]', '[1,0,0]'),
    (3, 'other', 1, 1, 1, '[0,0]', '[0,0,0]'),
    (4, 'other', 1, 1, 1, '[1,0]', '[1,0,0]');
INSERT INTO public.multi_model_acl_docs
SELECT id, 'acme', 1, 1, 1,
       ARRAY[id::real, 0]::real[]::pgcontext.vector,
       ARRAY[id::real, 0, 0]::real[]::pgcontext.vector
  FROM pg_catalog.generate_series(5, 24) AS id;
CREATE INDEX multi_model_acl_legacy_hnsw ON public.multi_model_acl_docs
    USING pgcontext_hnsw (legacy pgcontext.vector_hnsw_ops);
CREATE INDEX multi_model_acl_modern_hnsw ON public.multi_model_acl_docs
    USING pgcontext_hnsw (modern pgcontext.vector_hnsw_ops);
ALTER TABLE public.multi_model_acl_docs ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.multi_model_acl_docs FORCE ROW LEVEL SECURITY;
CREATE POLICY multi_model_tenant ON public.multi_model_acl_docs
    USING (tenant = current_setting('pgcontext_heavy.tenant', true));
GRANT SELECT ON public.multi_model_acl_docs TO ${OWNER_ROLE}, ${DENIED_ROLE};
RESET SESSION AUTHORIZATION;

SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'acme';
SELECT pgcontext.create_collection('multi_model_acl_docs', 'public.multi_model_acl_docs');
SELECT pgcontext.register_vector('multi_model_acl_docs', 'legacy', 'legacy', 2, 'l2');
SELECT pgcontext.register_filter_column('multi_model_acl_docs', 'tenant', 'tenant');
SELECT pgcontext.upsert_points(
    'multi_model_acl_docs',
    ARRAY(
        SELECT id::text
          FROM pg_catalog.generate_series(1, 24) AS id
         WHERE id NOT IN (3, 4)
    )
);
SET pgcontext_heavy.tenant = 'other';
SELECT pgcontext.upsert_points('multi_model_acl_docs', ARRAY['3', '4']);

SELECT pgcontext.register_embedding_profile(
    'multi_model_acl_docs', 'legacy_v1', 'legacy',
    'public.multi_model_acl_legacy_hnsw',
    jsonb_build_object(
        'representation', 'dense', 'dimensions', 2, 'normalization', 'none',
        'metric', 'l2', 'provider', 'fixture', 'model', 'legacy', 'revision', '1',
        'input_template', '{t}', 'output_template', '{v}', 'bit_order', NULL,
        'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
        'configuration_hash', '0123456789abcdef',
        'source_version_column', 'source_version',
        'embedding_version_column', 'legacy_version'
    )
);
SELECT pgcontext.register_embedding_profile(
    'multi_model_acl_docs', 'modern_v2', 'modern',
    'public.multi_model_acl_modern_hnsw',
    jsonb_build_object(
        'representation', 'dense', 'dimensions', 3, 'normalization', 'none',
        'metric', 'l2', 'provider', 'fixture', 'model', 'modern', 'revision', '2',
        'input_template', '{t}', 'output_template', '{v}', 'bit_order', NULL,
        'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
        'configuration_hash', 'fedcba9876543210',
        'source_version_column', 'source_version',
        'embedding_version_column', 'modern_version'
    )
);
RESET SESSION AUTHORIZATION;
ANALYZE public.multi_model_acl_docs;
SQL

psql_db <<'SQL'
CREATE TABLE public.multi_model_partition_docs (
    id bigint PRIMARY KEY,
    source_version bigint NOT NULL,
    legacy_version bigint NOT NULL,
    modern_version bigint NOT NULL,
    legacy pgcontext.vector(2) NOT NULL,
    modern pgcontext.vector(3) NOT NULL
) PARTITION BY RANGE (id);
CREATE TABLE public.multi_model_partition_docs_low
    PARTITION OF public.multi_model_partition_docs FOR VALUES FROM (1) TO (11);
CREATE TABLE public.multi_model_partition_docs_high
    PARTITION OF public.multi_model_partition_docs FOR VALUES FROM (11) TO (21);
INSERT INTO public.multi_model_partition_docs
SELECT id, 1, 1, 1,
       ARRAY[id::real, 0]::real[]::pgcontext.vector,
       ARRAY[(21 - id)::real, 0, 0]::real[]::pgcontext.vector
  FROM pg_catalog.generate_series(1, 20) AS id;
SELECT pgcontext.create_collection(
    'multi_model_partition_docs', 'public.multi_model_partition_docs'
);
SELECT pgcontext.register_vector(
    'multi_model_partition_docs', 'legacy', 'legacy', 2, 'l2'
);
SELECT pgcontext.backfill_points('multi_model_partition_docs', 100);
CREATE INDEX multi_model_partition_legacy_hnsw
    ON public.multi_model_partition_docs
    USING pgcontext_hnsw (legacy pgcontext.vector_hnsw_ops);
CREATE INDEX multi_model_partition_modern_hnsw
    ON public.multi_model_partition_docs
    USING pgcontext_hnsw (modern pgcontext.vector_hnsw_ops);
SELECT pgcontext.register_embedding_profile(
    'multi_model_partition_docs', 'legacy_v1', 'legacy',
    'public.multi_model_partition_legacy_hnsw',
    jsonb_build_object(
        'representation', 'dense', 'dimensions', 2, 'normalization', 'none',
        'metric', 'l2', 'provider', 'fixture', 'model', 'legacy', 'revision', '1',
        'input_template', '{t}', 'output_template', '{v}', 'bit_order', NULL,
        'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
        'configuration_hash', '0123456789abcdef',
        'source_version_column', 'source_version',
        'embedding_version_column', 'legacy_version'
    )
);
SELECT pgcontext.register_embedding_profile(
    'multi_model_partition_docs', 'modern_v2', 'modern',
    'public.multi_model_partition_modern_hnsw',
    jsonb_build_object(
        'representation', 'dense', 'dimensions', 3, 'normalization', 'none',
        'metric', 'l2', 'provider', 'fixture', 'model', 'modern', 'revision', '2',
        'input_template', '{t}', 'output_template', '{v}', 'bit_order', NULL,
        'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
        'configuration_hash', 'fedcba9876543210',
        'source_version_column', 'source_version',
        'embedding_version_column', 'modern_version'
    )
);
DO $partition_accounting$
DECLARE
    report jsonb;
    accounted bigint;
    reported bigint;
    final_branch_visits bigint;
    final_child_visits bigint;
BEGIN
    report := pgcontext.query_multi_model(
        'multi_model_partition_docs',
        jsonb_build_array(
            jsonb_build_object(
                'profile', 'legacy_v1',
                'configuration_hash', '0123456789abcdef',
                'query', '[1,0]', 'limit', 10, 'weight', 1.0
            ),
            jsonb_build_object(
                'profile', 'modern_v2',
                'configuration_hash', 'fedcba9876543210',
                'query', '[1,0,0]', 'limit', 10, 'weight', 1.0
            )
        ),
        NULL, 10, 60, 22, true
    );
    IF EXISTS (
        SELECT 1
          FROM pg_catalog.jsonb_array_elements(report->'branches') AS branch
         WHERE branch->>'strategy' <> 'hnsw_with_authoritative_recheck'
            OR (branch->>'hnsw_visits')::bigint <= 0
    ) THEN
        RAISE EXCEPTION 'partition branch did not use bounded HNSW: %', report;
    END IF;
    SELECT pg_catalog.sum(
               (branch->>'hnsw_visits')::bigint
               + (branch->>'candidate_count')::bigint
               + (branch->>'recheck_count')::bigint
           )
      INTO accounted
      FROM pg_catalog.jsonb_array_elements(report->'branches') AS branch;
    reported := (report->'budget_usage'->>'comparisons')::bigint;
    IF accounted > reported THEN
        RAISE EXCEPTION 'partition comparison accounting underreported: % > %',
            accounted, reported;
    END IF;
    SELECT (branch->>'hnsw_visits')::bigint
      INTO final_branch_visits
      FROM pg_catalog.jsonb_array_elements(report->'branches')
               WITH ORDINALITY AS branch(branch, ordinality)
      ORDER BY ordinality DESC
      LIMIT 1;
    SELECT node_reads INTO final_child_visits
      FROM pgcontext.hnsw_last_scan_work();
    IF final_branch_visits <= final_child_visits THEN
        RAISE EXCEPTION 'partition branch charged only its final child: % <= %',
            final_branch_visits, final_child_visits;
    END IF;
END
$partition_accounting$;
SQL

branches="jsonb_build_array(
    jsonb_build_object(
        'profile','legacy_v1','configuration_hash','0123456789abcdef',
        'query','[0,0]','limit',2,'weight',1.0
    ),
    jsonb_build_object(
        'profile','modern_v2','configuration_hash','fedcba9876543210',
        'query','[0,0,0]','limit',2,'weight',1.0
    )
)"

visible="$(psql_db -qAt <<SQL | tail -n 1
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'acme';
SELECT coalesce(string_agg(result->>'source_key', ',' ORDER BY result->>'source_key'), '')
  FROM pg_catalog.jsonb_array_elements(
      pgcontext.query_multi_model(
          'multi_model_acl_docs', ${branches}, NULL, 2, 60, 6, true
      )->'results'
  ) AS result;
SQL
)"
if [[ "${visible}" != "1,2" ]]; then
    echo "multi-model RLS returned unexpected source keys: ${visible}" >&2
    exit 1
fi

filtered="$(psql_db -qAt <<SQL | tail -n 1
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'acme';
SELECT pg_catalog.jsonb_array_length(
    pgcontext.query_multi_model(
        'multi_model_acl_docs', ${branches},
        '{"must":[{"key":"tenant","match":"other"}]}'::jsonb,
        2, 60, 6, true
    )->'results'
);
SQL
)"
if [[ "${filtered}" != "0" ]]; then
    echo "multi-model filter crossed the RLS boundary: ${filtered}" >&2
    exit 1
fi

coverage_acme="$(psql_db -qAt <<SQL | tail -n 1
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'acme';
SELECT pg_catalog.string_agg(
           pg_catalog.format(
               '%s:%s:%s:%s', profile_name, covered_points, stale_points, active_points
           ),
           ',' ORDER BY profile_name
       )
  FROM pgcontext.embedding_profile_coverage('multi_model_acl_docs');
SQL
)"
if [[ "${coverage_acme}" != "legacy_v1:22:0:22,modern_v2:22:0:22" ]]; then
    echo "multi-model coverage leaked or omitted acme RLS rows: ${coverage_acme}" >&2
    exit 1
fi

coverage_other="$(psql_db -qAt <<SQL | tail -n 1
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'other';
SELECT pg_catalog.string_agg(
           pg_catalog.format(
               '%s:%s:%s:%s', profile_name, covered_points, stale_points, active_points
           ),
           ',' ORDER BY profile_name
       )
  FROM pgcontext.embedding_profile_coverage('multi_model_acl_docs');
SQL
)"
if [[ "${coverage_other}" != "legacy_v1:2:0:2,modern_v2:2:0:2" ]]; then
    echo "multi-model coverage leaked or omitted other-tenant RLS rows: ${coverage_other}" >&2
    exit 1
fi

denied_log="${HEAVY_TMPDIR}/${DBNAME}_denied.log"
if psql_db 2>"${denied_log}" <<SQL
SET SESSION AUTHORIZATION ${DENIED_ROLE};
SET pgcontext_heavy.tenant = 'acme';
SELECT pgcontext.query_multi_model(
    'multi_model_acl_docs', ${branches}, NULL, 2, 60, 6, true
);
SQL
then
    echo "non-member multi-model query unexpectedly succeeded" >&2
    exit 1
fi
grep -qi "permission denied" "${denied_log}"

psql_db -c "REVOKE SELECT ON public.multi_model_acl_docs FROM ${OWNER_ROLE}"
revoke_log="${HEAVY_TMPDIR}/${DBNAME}_revoke.log"
if psql_db 2>"${revoke_log}" <<SQL
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'acme';
SELECT pgcontext.query_multi_model(
    'multi_model_acl_docs', ${branches}, NULL, 2, 60, 6, true
);
SQL
then
    echo "multi-model query ignored source SELECT revocation" >&2
    exit 1
fi
grep -qi "permission denied" "${revoke_log}"

printf 'multi_model_rls_acl: ok\n'
