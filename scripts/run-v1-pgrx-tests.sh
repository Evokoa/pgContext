#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
PG_MAJOR="${PG_MAJOR:-17}"

# macOS postmasters abort with "postmaster became multithreaded during
# startup" when the spawning environment carries no valid locale (locale
# discovery spawns a thread); non-interactive harness shells hit this.
export LC_ALL="${LC_ALL:-C}"

if [[ "${PG_MAJOR}" != "17" ]]; then
  echo "PG_MAJOR must be 17 for the GitHub V1 SQL gate" >&2
  exit 2
fi

# The whole pg_test suite, unfiltered. This script used to run a curated
# filter list, which twice reported green while tests outside the list were
# failing — including a broken INSERT path — so a filter may never come
# back. A test that should not gate the release must be deleted or fixed,
# not skipped.
cd "${REPO_ROOT}"
PGRX_TEST_PLATFORM="${PGRX_TEST_PLATFORM:-$(uname -s)}"
if [[ "${PGRX_TEST_PLATFORM}" != "Darwin" ]]; then
  cargo pgrx test --release -p context-pg pg17
  exit 0
fi

# cargo-pgrx links pg_test as a standalone Rust test executable. Mach-O cannot
# resolve that executable's PostgreSQL server data symbols, so it aborts before
# the harness starts. Install the test-enabled extension and execute the same
# generated wrappers inside PostgreSQL, where those symbols are available.
PGRX_TEST_DBNAME="${PGRX_TEST_DBNAME:-pgcontext_pgrx_tests}"
PGHOST="${PGHOST:-localhost}"
PGPORT="${PGPORT:-28817}"
if [[ ! "${PGRX_TEST_DBNAME}" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
  echo "PGRX_TEST_DBNAME must be a simple SQL identifier" >&2
  exit 2
fi

pgrx_pg_config="$(cargo pgrx info pg-config pg17)"
pg17_psql="$(dirname "${pgrx_pg_config}")/psql"
extension_version="$(sed -n "s/^default_version = '\([^']*\)'/\1/p" \
  crates/context-pg/pgcontext.control)"
if [[ -z "${extension_version}" ]]; then
  echo "could not read pgcontext default_version" >&2
  exit 2
fi
extension_sql="$("${pgrx_pg_config}" --sharedir)/extension/pgcontext--${extension_version}.sql"
psql_connection=(-X -h "${PGHOST}" -p "${PGPORT}" -v ON_ERROR_STOP=1)
runner_command=(
  python3 scripts/run_pgrx_tests_in_server.py
  --repo-root "${REPO_ROOT}"
  --psql "${pg17_psql}"
  --host "${PGHOST}"
  --port "${PGPORT}"
  --database "${PGRX_TEST_DBNAME}"
  --extension-sql "${extension_sql}"
)
if [[ -n "${PGUSER:-}" ]]; then
  psql_connection+=(-U "${PGUSER}")
  runner_command+=(--user "${PGUSER}")
fi

cargo pgrx start pg17
cargo pgrx install --test --release -p context-pg \
  --pg-config "${pgrx_pg_config}" \
  --no-default-features --features "pg17 pg_test"
"${pg17_psql}" "${psql_connection[@]}" -d postgres \
  -c "DROP DATABASE IF EXISTS ${PGRX_TEST_DBNAME}" \
  -c "CREATE DATABASE ${PGRX_TEST_DBNAME}"
"${runner_command[@]}"
