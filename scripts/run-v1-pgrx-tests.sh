#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
PG_MAJOR="${PG_MAJOR:-17}"

# macOS postmasters abort with "postmaster became multithreaded during
# startup" when the spawning environment carries no valid locale (locale
# discovery spawns a thread); non-interactive harness shells hit this.
export LC_ALL="${LC_ALL:-C}"

if [[ "${PG_MAJOR}" != "17" ]]; then
  echo "PG_MAJOR must be 17 for the GitHub V1 SQL gate" >&2
  exit 2
fi

# The whole pg_test suite, unfiltered. This script used to run a curated
# filter list, which twice reported green while tests outside the list were
# failing — including a broken INSERT path — so a filter may never come
# back. A test that should not gate the release must be deleted or fixed,
# not skipped.
cd "${REPO_ROOT}"
cargo pgrx test --release -p context-pg pg17
