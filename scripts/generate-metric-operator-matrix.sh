#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE="${ROOT_DIR}/docs/user_guide/metric_operator_matrix.data"
TARGET="${ROOT_DIR}/docs/user_guide/metric_operator_matrix.md"
mode="write"

usage() {
    cat <<'USAGE'
Usage: scripts/generate-metric-operator-matrix.sh [--check]

Generate docs/user_guide/metric_operator_matrix.md from metric_operator_matrix.data.

Options:
  --check   Verify the generated file is current without rewriting it.
USAGE
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --check) mode="check"; shift ;;
        -h | --help) usage; exit 0 ;;
        *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

output="${TARGET}"
tmp=""
if [[ "${mode}" == "check" ]]; then
    tmp="$(mktemp "${TMPDIR:-/tmp}/pgcontext-metric-operator-matrix.XXXXXX")"
    output="${tmp}"
    trap 'rm -f "${tmp}"' EXIT
fi

awk -F'|' '
NR == 1 {
    if (NF != 6) {
        printf "invalid metric matrix header: expected 6 columns, got %d\n", NF > "/dev/stderr"
        exit 1
    }
    next
}
{
    if (NF != 6) {
        printf "invalid metric matrix row %d: expected 6 columns, got %d\n", NR, NF > "/dev/stderr"
        exit 1
    }
    for (column = 1; column <= NF; column++) {
        if ($column == "") {
            printf "invalid metric matrix row %d: empty column %d\n", NR, column > "/dev/stderr"
            exit 1
        }
    }
}
BEGIN {
    print "# Exact Metric and Operator Matrix"
    print ""
    print "This file is generated from `docs/user_guide/metric_operator_matrix.data`."
    print "Run `scripts/generate-metric-operator-matrix.sh` after changing the source data."
    print ""
    print "Each row identifies the exact framework-free kernel, its SQL-facing operator or helper, and its score direction. Operators apply only to like representations."
    print "Definitions and edge-case behavior are pinned in [Metric Semantics](metric_semantics.md)."
    print ""
    print "| Representation | Metric | Exact core operator | SQL operator | Order for nearest-first search | Current SQL lifecycle |"
    print "|---|---|---|---|---|---|"
}
NR > 1 {
    printf "| %s | %s | %s | %s | %s | `%s` |\n", $1, $2, $3, $4, $5, $6
}
' "${SOURCE}" > "${output}"

if [[ "${mode}" == "check" ]]; then
    diff -u "${TARGET}" "${output}"
fi
