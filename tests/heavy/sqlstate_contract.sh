#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

if [[ "$(uname -s)" == Darwin ]]; then
    # A standalone Mach-O Rust test executable cannot resolve PostgreSQL server
    # symbols. Install the pg_test build and invoke the generated wrappers in a
    # live backend so the same Rust test bodies and assertions execute where
    # those symbols are available.
    start_and_install_extension
    reset_database
    psql_db <<'SQL'
CREATE EXTENSION pgcontext;
SELECT tests.sqlstate_contract_covers_vector_and_search_bad_paths();
SELECT tests.sqlstate_contract_covers_collection_and_registration_bad_paths();
SELECT tests.sqlstate_contract_covers_filter_and_operation_bad_paths();
SELECT tests.t33_sqlstate_contract_covers_model_migration_and_telemetry_bad_();
SQL
    printf 'sqlstate_contract_in_server_complete\n'
else
    cargo pgrx test -p context-pg "${PG_VERSION}" sqlstate_contract
fi
