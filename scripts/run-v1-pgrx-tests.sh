#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
PG_MAJOR="${PG_MAJOR:-17}"

# macOS postmasters abort with "postmaster became multithreaded during
# startup" when the spawning environment carries no valid locale (locale
# discovery spawns a thread); non-interactive harness shells hit this.
export LC_ALL="${LC_ALL:-C}"

case "${PG_MAJOR}" in
  17 | 18) ;;
  *)
    echo "PG_MAJOR must be 17 or 18" >&2
    exit 2
    ;;
esac
PG_FEATURE="pg${PG_MAJOR}"

# The whole pg_test suite, unfiltered. This script used to run a curated
# filter list, which twice reported green while tests outside the list were
# failing — including a broken INSERT path — so a filter may never come
# back. A test that should not gate the release must be deleted or fixed,
# not skipped.
cd "${REPO_ROOT}"
PGRX_TEST_PLATFORM="${PGRX_TEST_PLATFORM:-$(uname -s)}"
if [[ "${PGRX_TEST_PLATFORM}" != "Darwin" ]]; then
  cargo pgrx test --release -p context-pg "${PG_FEATURE}"
  exit 0
fi

# cargo-pgrx links pg_test as a standalone Rust test executable. Mach-O cannot
# resolve that executable's PostgreSQL server data symbols, so it aborts before
# the harness starts. Install the test-enabled extension and execute the same
# generated wrappers inside PostgreSQL, where those symbols are available.
PGRX_TEST_DBNAME="${PGRX_TEST_DBNAME:-pgcontext_pgrx_tests}"
PGHOST="${PGHOST:-localhost}"
PGPORT="${PGPORT:-288${PG_MAJOR}}"
if [[ ! "${PGRX_TEST_DBNAME}" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
  echo "PGRX_TEST_DBNAME must be a simple SQL identifier" >&2
  exit 2
fi

pgrx_pg_config="$(cargo pgrx info pg-config "${PG_FEATURE}")"
pg_bin="$(dirname "${pgrx_pg_config}")"
pg_psql="${pg_bin}/psql"
extension_version="$(sed -n "s/^default_version = '\([^']*\)'/\1/p" \
  crates/context-pg/pgcontext.control)"
if [[ -z "${extension_version}" ]]; then
  echo "could not read pgcontext default_version" >&2
  exit 2
fi
extension_sql="$("${pgrx_pg_config}" --sharedir)/extension/pgcontext--${extension_version}.sql"
createdb_connection=(-h "${PGHOST}" -p "${PGPORT}")
runner_command=(
  python3 scripts/run_pgrx_tests_in_server.py
  --repo-root "${REPO_ROOT}"
  --psql "${pg_psql}"
  --host "${PGHOST}"
  --port "${PGPORT}"
  --database "${PGRX_TEST_DBNAME}"
  --extension-sql "${extension_sql}"
)
if [[ -n "${PGUSER:-}" ]]; then
  createdb_connection+=(-U "${PGUSER}")
  runner_command+=(--user "${PGUSER}")
fi

cargo pgrx install --test --release -p context-pg \
  --pg-config "${pgrx_pg_config}" \
  --no-default-features --features "${PG_FEATURE} pg_test"

PGRX_TEST_TMPDIR="${PGRX_TEST_TMPDIR:-${TMPDIR:-/tmp}}"
mkdir -p "${PGRX_TEST_TMPDIR}"
cluster_base="$(cd "${PGRX_TEST_TMPDIR}" && pwd)"
cluster_root="$(mktemp -d "${cluster_base}/pgcontext-pgrx-pg${PG_MAJOR}.XXXXXX")"
cluster_data="${cluster_root}/data"
cluster_log="${cluster_root}/postgres.log"
cluster_started=false
cleanup_cluster() {
  if [[ "${cluster_started}" == true ]]; then
    "${pg_bin}/pg_ctl" stop -D "${cluster_data}" -m fast || true
  fi
  case "${cluster_root}" in
    "${cluster_base}/pgcontext-pgrx-pg${PG_MAJOR}."*)
      rm -rf -- "${cluster_root}"
      ;;
    *)
      echo "refusing to remove unexpected test directory: ${cluster_root}" >&2
      ;;
  esac
}
trap cleanup_cluster EXIT

"${pg_bin}/initdb" -D "${cluster_data}" --no-locale --encoding=UTF8
"${pg_bin}/pg_ctl" start -D "${cluster_data}" -l "${cluster_log}" \
  -o "-p ${PGPORT} -h ${PGHOST}"
cluster_started=true
"${pg_bin}/createdb" "${createdb_connection[@]}" "${PGRX_TEST_DBNAME}"
"${runner_command[@]}"
