#!/usr/bin/env bash
set -euo pipefail

PG_VERSION="${PG_VERSION:-pg17}"
PG_FEATURE="${PG_FEATURE:-pg17}"
PG_CONFIG="${PG_CONFIG:-$(cargo pgrx info pg-config "${PG_VERSION}")}"
PGHOST="${PGHOST:-localhost}"
PGPORT="${PGPORT:-28817}"
DBNAME="${DBNAME:-pgcontext_heavy}"
REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
HEAVY_TMPDIR="${HEAVY_TMPDIR:-${REPO_ROOT}/target/tmp/heavy}"

mkdir -p "${HEAVY_TMPDIR}"

require_simple_identifier() {
    local value="$1"
    local label="$2"
    if [[ ! "${value}" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
        echo "${label} must be a simple SQL identifier" >&2
        exit 2
    fi
}

psql_postgres() {
    psql -h "${PGHOST}" -p "${PGPORT}" -d postgres -v ON_ERROR_STOP=1 "$@"
}

psql_db() {
    # Omit pg_catalog so PostgreSQL searches it implicitly before these
    # explicit schemas while keeping public as the CREATE target.
    PGOPTIONS="${PGOPTIONS:-} -c search_path=public,pgcontext" \
        psql -h "${PGHOST}" -p "${PGPORT}" -d "${DBNAME}" -v ON_ERROR_STOP=1 "$@"
}

start_and_install_extension() {
    cargo pgrx start "${PG_VERSION}"
    cargo pgrx install -p context-pg --features "${PG_FEATURE}" --pg-config "${PG_CONFIG}"
}

start_and_install_test_extension() {
    cargo pgrx start "${PG_VERSION}"
    cargo pgrx install --test -p context-pg \
        --no-default-features --features "${PG_FEATURE} pg_test" \
        --pg-config "${PG_CONFIG}"
}

reset_database() {
    require_simple_identifier "${DBNAME}" "DBNAME"
    psql_postgres \
        -c "DROP DATABASE IF EXISTS ${DBNAME} WITH (FORCE)" \
        -c "CREATE DATABASE ${DBNAME}"
}

drop_database() {
    local dbname="$1"
    require_simple_identifier "${dbname}" "database name"
    psql_postgres -c "DROP DATABASE IF EXISTS ${dbname} WITH (FORCE)"
}

create_database() {
    local dbname="$1"
    require_simple_identifier "${dbname}" "database name"
    psql_postgres -c "CREATE DATABASE ${dbname}"
}

drop_role_if_exists() {
    local role_name="$1"
    require_simple_identifier "${role_name}" "role name"
    psql_postgres -c "DROP ROLE IF EXISTS ${role_name}"
}

create_login_role() {
    local role_name="$1"
    require_simple_identifier "${role_name}" "role name"
    psql_postgres -c "CREATE ROLE ${role_name} LOGIN"
}

cleanup_database() {
    drop_database "${DBNAME}"
}

assert_sql_equals() {
    local sql="$1"
    local expected="$2"
    local actual
    actual="$(psql_db -Atc "${sql}" | tail -n 1)"
    if [[ "${actual}" != "${expected}" ]]; then
        echo "assertion failed" >&2
        echo "sql: ${sql}" >&2
        echo "expected: ${expected}" >&2
        echo "actual: ${actual}" >&2
        exit 1
    fi
}

load_linear_vector_fixture() {
    local table_name="$1"
    local row_count="$2"
    require_simple_identifier "${table_name}" "table name"
    if [[ ! "${row_count}" =~ ^[1-9][0-9]*$ ]]; then
        echo "row count must be a positive integer" >&2
        exit 2
    fi
    psql_db <<SQL
CREATE TABLE public.${table_name} (
    id bigint PRIMARY KEY,
    embedding vector NOT NULL,
    body text NOT NULL
);

INSERT INTO public.${table_name} (id, embedding, body)
SELECT value,
       format('[%s,0]', value)::vector,
       format('fixture %s', value)
  FROM generate_series(1, ${row_count}) AS value;
SQL
}

pg_bin() {
    local executable="$1"
    "$(dirname "${PG_CONFIG}")/${executable}" --version >/dev/null
    printf '%s/%s\n' "$(dirname "${PG_CONFIG}")" "${executable}"
}

installed_test_extension_sql() {
    local extension_version
    extension_version="$(sed -n "s/^default_version = '\([^']*\)'/\1/p" \
        "${REPO_ROOT}/crates/context-pg/pgcontext.control")"
    if [[ -z "${extension_version}" ]]; then
        echo "could not read pgcontext default_version" >&2
        exit 2
    fi
    printf '%s/extension/pgcontext--%s.sql\n' \
        "$("${PG_CONFIG}" --sharedir)" "${extension_version}"
}
