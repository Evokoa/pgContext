#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE="${ROOT_DIR}/docs/user_guide/parity_matrix.data"
TARGET="${ROOT_DIR}/docs/user_guide/parity_matrix.md"
mode="write"

usage() {
    cat <<'USAGE'
Usage: scripts/generate-parity-matrix.sh [--check]

Generate docs/user_guide/parity_matrix.md from parity_matrix.data.

Options:
  --check   Verify the generated file is current without rewriting it.
USAGE
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --check)
            mode="check"
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

output="${TARGET}"
tmp=""
if [[ "${mode}" == "check" ]]; then
    tmp="$(mktemp "${TMPDIR:-/tmp}/pgcontext-parity-matrix.XXXXXX")"
    output="${tmp}"
    trap 'rm -f "${tmp}"' EXIT
fi

while IFS='|' read -r capability _reference _status _contract owner; do
    if [[ "${capability}" == "Capability" ]]; then
        continue
    fi
    if [[ ! -e "${ROOT_DIR}/${owner}" ]]; then
        echo "missing parity owner reference for ${capability}: ${owner}" >&2
        exit 1
    fi
done < "${SOURCE}"

awk -F'|' '
NR == 1 {
    next
}
BEGIN {
    print "# pgvector and Qdrant Parity Matrix"
    print ""
    print "This file is generated from `docs/user_guide/parity_matrix.data`."
    print "Run `scripts/generate-parity-matrix.sh` after changing the source data."
    print ""
    print "Status values:"
    print ""
    print "- `stable`: covered by the first production SQL compatibility contract."
    print "- `experimental`: SQL-visible or implemented, but outside the production promise."
    print "- `planned`: explicitly not part of the first stable surface yet."
    print "- `intentionally different`: pgContext deliberately uses PostgreSQL-native semantics."
    print ""
    print "| Capability | Reference | Status | pgContext release contract | Owning reference |"
    print "|---|---|---|---|---|"
}
{
    if (NF != 5) {
        printf "invalid parity row %d: expected 5 columns, got %d\n", NR, NF > "/dev/stderr"
        exit 1
    }
    printf "| %s | %s | `%s` | %s | `%s` |\n", $1, $2, $3, $4, $5
}
' "${SOURCE}" > "${output}"

if [[ "${mode}" == "check" ]]; then
    diff -u "${TARGET}" "${output}"
fi
