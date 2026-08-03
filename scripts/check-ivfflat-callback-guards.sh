#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"

exec python3 "${REPO_ROOT}/scripts/check_ivfflat_callback_guards.py" \
  "${REPO_ROOT}/crates/context-pg/src/ivfflat_am.rs" \
  "${REPO_ROOT}/crates/context-pg/src/ivfflat_am/external_build.rs" \
  "${REPO_ROOT}/crates/context-pg/src/ivfflat_am/options.rs" \
  "${REPO_ROOT}/crates/context-pg/src/ivfflat_am/callback_contract.rs" \
  "${REPO_ROOT}/crates/context-pg/src/ivfflat_am/unsafe_inventory.data"
