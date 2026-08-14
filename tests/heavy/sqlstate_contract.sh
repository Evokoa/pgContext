#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

# Standalone Rust pg_test executables cannot reliably resolve PostgreSQL server
# symbols across supported linkers. Execute the generated wrappers in a live
# backend on every platform.
start_and_install_test_extension
reset_database
runner_args=(
    --repo-root "${REPO_ROOT}"
    --psql "$(pg_bin psql)"
    --host "${PGHOST}"
    --port "${PGPORT}"
    --database "${DBNAME}"
    --extension-sql "$(installed_test_extension_sql)"
    --filter sqlstate_contract
)
if [[ -n "${PGUSER:-}" ]]; then
    runner_args+=(--user "${PGUSER}")
fi
python3 "${REPO_ROOT}/scripts/run_pgrx_tests_in_server.py" "${runner_args[@]}"
