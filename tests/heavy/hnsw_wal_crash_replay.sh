#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DBNAME="${DBNAME:-pgcontext_hnsw_wal_crash_replay}"
PG_FEATURE="${PG_FEATURE:-pg17 pg_test}"
# shellcheck source=tests/heavy/lib.sh
source "${SCRIPT_DIR}/lib.sh"

PG_CTL="$(pg_bin pg_ctl)"
PGRX_DATA_DIR="${PGRX_DATA_DIR:-}"
FAILPOINTS=(
  before_page_initialization after_page_initialization
  before_append after_append
  before_rewiring after_rewiring
  before_generic_xlog_finish after_generic_xlog_finish
  before_metapage_publication after_metapage_publication
)
if [[ -n "${HNSW_FAILPOINTS:-}" ]]; then
    IFS=',' read -r -a FAILPOINTS <<<"${HNSW_FAILPOINTS}"
fi

validate_oracle() {
    local phase="$1"
    local exact indexed
    exact="$(psql_db -At <<'SQL' | tail -n 1
SET enable_indexscan = off;
SELECT string_agg(id::text, ',' ORDER BY embedding OPERATOR(pgcontext.<->) array_fill(9::real, ARRAY[512])::vector)
  FROM (SELECT id, embedding FROM public.hnsw_wal_crash_docs ORDER BY embedding OPERATOR(pgcontext.<->) array_fill(9::real, ARRAY[512])::vector LIMIT 3) AS ranked;
SQL
)"
    indexed="$(psql_db -At <<'SQL' | tail -n 1
SET enable_seqscan = off;
SELECT string_agg(id::text, ',' ORDER BY embedding OPERATOR(pgcontext.<->) array_fill(9::real, ARRAY[512])::vector)
  FROM (SELECT id, embedding FROM public.hnsw_wal_crash_docs ORDER BY embedding OPERATOR(pgcontext.<->) array_fill(9::real, ARRAY[512])::vector LIMIT 3) AS ranked;
SQL
)"
    if [[ "${indexed}" != "${exact}" ]]; then
        echo "HNSW replay oracle mismatch at ${phase}: indexed=${indexed}, exact=${exact}" >&2
        exit 1
    fi
    printf 'hnsw_wal_crash_oracle: %s\n' "${phase}"
}

start_and_install_extension
reset_database

psql_db <<'SQL'
CREATE EXTENSION pgcontext;
CREATE TABLE public.hnsw_wal_crash_docs (id bigint PRIMARY KEY, embedding vector NOT NULL);
INSERT INTO public.hnsw_wal_crash_docs
SELECT id, array_fill(id::real, ARRAY[512])::vector
  FROM (VALUES (1), (2), (9)) AS fixture(id);
CREATE INDEX hnsw_wal_crash_docs_idx ON public.hnsw_wal_crash_docs USING pgcontext_hnsw (embedding);
CHECKPOINT;
SQL

for failpoint in "${FAILPOINTS[@]}"; do
    if psql_db <<SQL
SELECT pgcontext.test_set_hnsw_physical_failpoint('${failpoint}');
INSERT INTO public.hnsw_wal_crash_docs VALUES (100 + ${#failpoint}, array_fill(3::real, ARRAY[512])::vector) ON CONFLICT DO NOTHING;
SQL
    then
        echo "expected injected HNSW failpoint ${failpoint} to interrupt its write" >&2
        exit 1
    fi
    psql_db -c "SELECT pgcontext.test_set_hnsw_physical_failpoint(NULL);"
    if [[ -n "${PGRX_DATA_DIR}" ]]; then
        "${PG_CTL}" -D "${PGRX_DATA_DIR}" stop -m immediate
    else
        # Local developer fallback when pgrx does not expose its data directory.
        cargo pgrx stop "${PG_VERSION}"
    fi
    cargo pgrx start "${PG_VERSION}"
    validate_oracle "${failpoint}"
done
