#!/usr/bin/env bash
# Install the separately packaged pgcontext_pgvector control and SQL artifacts.
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
PG_CONFIG_BIN=${1:-${PG_CONFIG:-pg_config}}
PG_MAJOR=$("${PG_CONFIG_BIN}" --version | sed -E 's/[^0-9]*([0-9]+).*/\1/')
if [[ "${PG_MAJOR}" != "17" ]]; then
  echo "pgcontext_pgvector 0.1.0 supports PostgreSQL 17; selected ${PG_MAJOR}" >&2
  exit 1
fi

SHARE_DIR=$("${PG_CONFIG_BIN}" --sharedir)
if [[ -n "${DESTDIR:-}" ]]; then
  EXTENSION_DIR=${DESTDIR%/}${SHARE_DIR}/extension
else
  EXTENSION_DIR=${SHARE_DIR}/extension
fi

install -d "${EXTENSION_DIR}"
install -m 0644 "${REPO_ROOT}/pgcontext_pgvector.control" "${EXTENSION_DIR}/pgcontext_pgvector.control"
install -m 0644 "${REPO_ROOT}/sql/pgcontext_pgvector--0.1.0.sql" \
  "${EXTENSION_DIR}/pgcontext_pgvector--0.1.0.sql"

echo "installed pgcontext_pgvector bridge artifacts in ${EXTENSION_DIR}"
