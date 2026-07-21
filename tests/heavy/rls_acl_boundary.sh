#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_rls_acl_boundary}"
TABLE_OWNER_ROLE="${TABLE_OWNER_ROLE:-pgctx_heavy_rls_table_owner}"
OWNER_ROLE="${OWNER_ROLE:-pgctx_heavy_rls_owner}"
DENIED_ROLE="${DENIED_ROLE:-pgctx_heavy_rls_denied}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

require_simple_identifier "${TABLE_OWNER_ROLE}" "TABLE_OWNER_ROLE"
require_simple_identifier "${OWNER_ROLE}" "OWNER_ROLE"
require_simple_identifier "${DENIED_ROLE}" "DENIED_ROLE"

grant_pgcontext_api_access() {
    local role_name="$1"
    require_simple_identifier "${role_name}" "role name"
    psql_db <<SQL
GRANT USAGE ON SCHEMA public, pgcontext TO ${role_name};
GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA pgcontext TO ${role_name};
GRANT USAGE ON TYPE vector TO ${role_name};
SQL
}

expect_search_denied() {
    local label="$1"
    local log_file="${HEAVY_TMPDIR}/${DBNAME}_${label}.log"
    rm -f "${log_file}"

    if psql_db 2>"${log_file}" <<SQL
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'acme';
SELECT * FROM pgcontext.search('rls_acl_docs', '[0,0]'::vector, 1);
SQL
    then
        echo "${label}: search unexpectedly succeeded" >&2
        exit 1
    fi

    if ! grep -qi "permission denied" "${log_file}"; then
        echo "${label}: search failed for an unexpected reason" >&2
        cat "${log_file}" >&2
        exit 1
    fi
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
grant_pgcontext_api_access "${TABLE_OWNER_ROLE}"
grant_pgcontext_api_access "${OWNER_ROLE}"
grant_pgcontext_api_access "${DENIED_ROLE}"

psql_db <<SQL
SET SESSION AUTHORIZATION ${TABLE_OWNER_ROLE};

CREATE TABLE public.rls_acl_docs (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    tenant text NOT NULL,
    body text NOT NULL
);

INSERT INTO public.rls_acl_docs (id, embedding, tenant, body)
VALUES
    (1, '[0,0]'::vector, 'acme', 'visible acme'),
    (2, '[1,0]'::vector, 'acme', 'visible acme neighbor'),
    (3, '[0,0]'::vector, 'other', 'blocked other'),
    (4, '[1,0]'::vector, 'other', 'blocked other neighbor');

ALTER TABLE public.rls_acl_docs ENABLE ROW LEVEL SECURITY;
ALTER TABLE public.rls_acl_docs FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON public.rls_acl_docs
    USING (tenant = current_setting('pgcontext_heavy.tenant', true))
    WITH CHECK (tenant = current_setting('pgcontext_heavy.tenant', true));

GRANT SELECT ON public.rls_acl_docs TO ${OWNER_ROLE}, ${DENIED_ROLE};

RESET SESSION AUTHORIZATION;

SET SESSION AUTHORIZATION ${OWNER_ROLE};

SELECT pgcontext.create_collection('rls_acl_docs', 'public.rls_acl_docs');
SELECT pgcontext.register_vector('rls_acl_docs', 'embedding', 'embedding', 2, 'l2');
SELECT pgcontext.register_filter_column('rls_acl_docs', 'tenant', 'tenant');
SELECT pgcontext.upsert_points('rls_acl_docs', ARRAY['1', '2', '3', '4']);

RESET SESSION AUTHORIZATION;
SQL

owner_visible="$(psql_db -qAt <<SQL | tail -n 1
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'acme';
SELECT string_agg(source_key, ',' ORDER BY source_key)
  FROM pgcontext.search('rls_acl_docs', '[0,0]'::vector, 10);
SQL
)"
if [[ "${owner_visible}" != "1,2" ]]; then
    echo "RLS owner search returned unexpected source keys: ${owner_visible}" >&2
    exit 1
fi

owner_filtered_count="$(psql_db -qAt <<SQL | tail -n 1
SET SESSION AUTHORIZATION ${OWNER_ROLE};
SET pgcontext_heavy.tenant = 'acme';
SELECT count(*)
  FROM pgcontext.search(
      'rls_acl_docs',
      '[0,0]'::vector,
      '{"must":[{"key":"tenant","match":"other"}]}',
      10
  );
SQL
)"
if [[ "${owner_filtered_count}" != "0" ]]; then
    echo "RLS owner could see rows outside tenant through filter: ${owner_filtered_count}" >&2
    exit 1
fi

denied_log="${HEAVY_TMPDIR}/${DBNAME}_denied_role.log"
rm -f "${denied_log}"
if psql_db 2>"${denied_log}" <<SQL
SET SESSION AUTHORIZATION ${DENIED_ROLE};
SET pgcontext_heavy.tenant = 'acme';
SELECT * FROM pgcontext.search('rls_acl_docs', '[0,0]'::vector, 1);
SQL
then
    echo "non-owner role unexpectedly searched owner collection" >&2
    exit 1
fi
if ! grep -qi "permission denied" "${denied_log}"; then
    echo "non-owner role search failed for an unexpected reason" >&2
    cat "${denied_log}" >&2
    exit 1
fi

psql_db <<SQL
REVOKE SELECT ON public.rls_acl_docs FROM ${OWNER_ROLE};
SQL

expect_search_denied "owner_revoke"
