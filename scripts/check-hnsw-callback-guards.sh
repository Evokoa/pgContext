#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
CHECKER="${REPO_ROOT}/scripts/check_hnsw_callback_guards.py"

if [[ ! -f "${CHECKER}" ]]; then
  echo "HNSW callback guard checker is missing: ${CHECKER}" >&2
  exit 1
fi

exec python3 "${CHECKER}" \
  "${HNSW_AM_SOURCE:-${REPO_ROOT}/crates/context-pg/src/hnsw_am.rs}" \
  "${HNSW_CONTRACT_SOURCE:-${REPO_ROOT}/crates/context-pg/src/hnsw_am/callback_contract.rs}" \
  "${HNSW_MODULE_ROOT:-${REPO_ROOT}/crates/context-pg/src/hnsw_am}" \
  "${HNSW_PAGE_STORAGE_SOURCE:-${REPO_ROOT}/crates/context-pg/src/hnsw_am_page_storage.rs}" \
  "${HNSW_UNSAFE_INVENTORY:-${REPO_ROOT}/crates/context-pg/src/hnsw_am/unsafe_inventory.data}"
