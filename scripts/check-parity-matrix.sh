#!/usr/bin/env bash
set -euo pipefail

require_v1_launch_complete=false
if [[ "${1:-}" == "--require-v1-launch-complete" ]]; then
  require_v1_launch_complete=true
  shift
fi
if (( $# != 0 )); then
  echo "usage: scripts/check-parity-matrix.sh [--require-v1-launch-complete]" >&2
  exit 2
fi

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
"${REPO_ROOT}/scripts/generate-parity-matrix.sh" --check

SOURCE="${REPO_ROOT}/docs/user_guide/parity_matrix.data"

awk -F'|' '
  NR == 1 { next }
  NF != 5 {
    printf "invalid parity row %d: expected 5 columns, got %d\n", NR, NF > "/dev/stderr"
    exit 1
  }
  $3 != "stable" && $3 != "experimental" && $3 != "planned" && $3 != "intentionally different" {
    printf "invalid parity status for %s: %s\n", $1, $3 > "/dev/stderr"
    exit 1
  }
  $3 == "stable" {
    contract = tolower($4)
    if (contract ~ /(remain[s]? planned|outside (the )?(stable|production)|not part of (the )?(first )?(stable|production)|requires remaining|remain open)/) {
      printf "stable parity row has non-stable contract wording for %s: %s\n", $1, $4 > "/dev/stderr"
      exit 1
    }
  }
  seen[$1]++ {
    printf "duplicate parity capability: %s\n", $1 > "/dev/stderr"
    exit 1
  }
' "${SOURCE}"

if [[ "${require_v1_launch_complete}" == true ]]; then
  require_v1_row() {
    local capability="$1"
    local status="$2"
    if ! awk -F'|' -v capability="${capability}" -v status="${status}" '
        NR > 1 && $1 == capability && $3 == status && $5 ~ /^docs\/user_guide\// {
          found = 1
        }
        END { exit !found }
      ' "${SOURCE}"; then
      echo "V1 launch requires ${capability} with status ${status} and a public user-guide owner" >&2
      exit 1
    fi
  }

  require_v1_row "Dense vector SQL type, casts, operators, aggregates" stable
  require_v1_row "Exact vector search over arrays and registered tables" stable
  require_v1_row "Filter JSON over ordinary columns and JSONB paths" stable
  require_v1_row "HNSW access method" experimental
  require_v1_row "Filtered ANN serving" experimental

  if awk -F'|' '
      NR > 1 && $3 == "planned" && $5 !~ /^docs\/user_guide\/roadmap.md$/ { found = 1 }
      END { exit !found }
    ' \
      "${SOURCE}"; then
    echo "planned parity rows must point to docs/user_guide/roadmap.md" >&2
    exit 1
  fi
fi
