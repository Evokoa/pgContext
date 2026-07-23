#!/usr/bin/env bash
# Install version-to-version SQL scripts that cargo-pgrx does not package.
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
PG_CONFIG_BIN=${1:-${PG_CONFIG:-pg_config}}
PG_MAJOR=$("${PG_CONFIG_BIN}" --version | sed -E 's/[^0-9]*([0-9]+).*/\1/')
if [[ "${PG_MAJOR}" != "17" ]]; then
  echo "pgContext 0.2.0 upgrade artifacts support PostgreSQL 17; selected ${PG_MAJOR}" >&2
  exit 1
fi

SHARE_DIR=$("${PG_CONFIG_BIN}" --sharedir)
if [[ -n "${DESTDIR:-}" ]]; then
  EXTENSION_DIR=${DESTDIR%/}${SHARE_DIR}/extension
else
  EXTENSION_DIR=${SHARE_DIR}/extension
fi

install -d "${EXTENSION_DIR}"
install -m 0644 "${REPO_ROOT}/sql/pgcontext--0.1.0--0.2.0.sql" \
  "${EXTENSION_DIR}/pgcontext--0.1.0--0.2.0.sql"

echo "installed pgContext upgrade artifacts in ${EXTENSION_DIR}"
