#!/usr/bin/env bash
set -euo pipefail

PGVECTOR_VERSION="${PGVECTOR_VERSION:-0.8.5}"
PG_CONFIG="${PG_CONFIG:-/opt/homebrew/opt/postgresql@17/bin/pg_config}"
BUILD_DIR="${TMPDIR:-/tmp}/pgvector-v${PGVECTOR_VERSION}"

if [[ ! -x "${PG_CONFIG}" ]]; then
  echo "PG_CONFIG does not point to an executable: ${PG_CONFIG}" >&2
  exit 2
fi

if [[ ! -d "${BUILD_DIR}/.git" ]]; then
  git clone --branch "v${PGVECTOR_VERSION}" --depth 1 \
    https://github.com/pgvector/pgvector.git "${BUILD_DIR}"
fi

env PG_CONFIG="${PG_CONFIG}" make -C "${BUILD_DIR}" clean
env PG_CONFIG="${PG_CONFIG}" make -C "${BUILD_DIR}"
env PG_CONFIG="${PG_CONFIG}" make -C "${BUILD_DIR}" install

echo "installed pgvector ${PGVECTOR_VERSION} for $("${PG_CONFIG}" --version)"
