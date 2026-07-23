#!/usr/bin/env bash
# Run the pgvector-derived HNSW compatibility profile through pg_regress.
set -euo pipefail

PSQL=${PGCONTEXT_REGRESSION_PSQL:-psql}
PG_CONFIG=${PGCONTEXT_REGRESSION_PG_CONFIG:-pg_config}
DB=${PGCONTEXT_REGRESSION_DB:-pgcontext_pgvector_regression_${$}}
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
INPUT_DIR=${ROOT}/tests/pgvector-regression
OUTPUT_DIR=${TMPDIR:-/tmp}/pgcontext-pgvector-regression-${$}

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

if [[ -n "${PG_REGRESS:-}" ]]; then
  PG_REGRESS_BIN=${PG_REGRESS}
else
  pgxs=$(${PG_CONFIG} --pgxs)
  PG_REGRESS_BIN=$(dirname "$(dirname "${pgxs}")")/test/regress/pg_regress
fi

[[ -x "${PG_REGRESS_BIN}" ]] \
  || fail "pg_regress is not executable at ${PG_REGRESS_BIN}"

mkdir -p "${OUTPUT_DIR}"

cleanup() {
  ${PSQL} -d postgres -v ON_ERROR_STOP=1 \
    -c "DROP DATABASE IF EXISTS ${DB};" >/dev/null 2>&1 || true
}
trap cleanup EXIT

${PSQL} -d postgres -v ON_ERROR_STOP=1 \
  -c "DROP DATABASE IF EXISTS ${DB};" \
  -c "CREATE DATABASE ${DB};" >/dev/null
${PSQL} -d "${DB}" -v ON_ERROR_STOP=1 \
  -c "CREATE EXTENSION vector" \
  -c "CREATE EXTENSION pgcontext" \
  -c "CREATE EXTENSION pgcontext_pgvector" >/dev/null

connection_args=(--use-existing --dbname="${DB}")
[[ -z "${PGHOST:-}" ]] || connection_args+=(--host="${PGHOST}")
[[ -z "${PGPORT:-}" ]] || connection_args+=(--port="${PGPORT}")
[[ -z "${PGUSER:-}" ]] || connection_args+=(--user="${PGUSER}")

for profile in hnsw_vector_pgcontext hnsw_halfvec_pgcontext; do
  "${PG_REGRESS_BIN}" \
    "${connection_args[@]}" \
    --bindir="$(${PG_CONFIG} --bindir)" \
    --inputdir="${INPUT_DIR}" \
    --outputdir="${OUTPUT_DIR}" \
    "${profile}"
done

echo "pgvector-derived HNSW regression profile passed (vector + halfvec, L2/IP/cosine/L1)"
