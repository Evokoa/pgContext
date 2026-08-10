#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TMPDIR="${TMPDIR:-${REPO_ROOT}/target/tmp}"
mkdir -p "${TMPDIR}"
work_dir="$(mktemp -d "${TMPDIR}/supported-feature-markers.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT

contract="${work_dir}/capability_contract.data"
supported="${work_dir}/supported_features.md"

cat >"${contract}" <<'DATA'
Capability ID|Capability|Maturity
CAP-LEXICAL-RETRIEVAL|PostgreSQL-native lexical retrieval|stable
CAP-FUZZY-RETRIEVAL|Trigram fuzzy retrieval|experimental
DATA

cat >"${supported}" <<'MARKDOWN'
| Feature | Maturity |
|---|---|
| Lexical <!-- capability:CAP-LEXICAL-RETRIEVAL --> | Stable |
| Fuzzy <!-- capability:CAP-FUZZY-RETRIEVAL --> | Experimental |
MARKDOWN

"${REPO_ROOT}/scripts/check-supported-feature-markers.sh" "${contract}" "${supported}"

cat >>"${supported}" <<'MARKDOWN'
| Duplicate lexical <!-- capability:CAP-LEXICAL-RETRIEVAL --> | Stable |
MARKDOWN
if "${REPO_ROOT}/scripts/check-supported-feature-markers.sh" \
    "${contract}" "${supported}" 2>"${work_dir}/duplicate.err"; then
  echo "duplicate capability marker unexpectedly passed" >&2
  exit 1
fi
grep -qF "duplicate capability markers: CAP-LEXICAL-RETRIEVAL" \
  "${work_dir}/duplicate.err"

cat >"${supported}" <<'MARKDOWN'
| Feature | Maturity |
|---|---|
| Unknown <!-- capability:CAP-UNKNOWN --> | Experimental |
MARKDOWN
if "${REPO_ROOT}/scripts/check-supported-feature-markers.sh" \
    "${contract}" "${supported}" 2>"${work_dir}/unknown.err"; then
  echo "unknown capability marker unexpectedly passed" >&2
  exit 1
fi
grep -qF "unknown capability ID: CAP-UNKNOWN" "${work_dir}/unknown.err"

echo "supported feature marker smoke: ok"
