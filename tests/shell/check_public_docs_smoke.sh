#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-public-docs.XXXXXX")"
trap 'rm -rf "${WORK_DIR}"' EXIT

mkdir -p "${WORK_DIR}/docs"
cat >"${WORK_DIR}/docs/index.md" <<'DOC'
# Fixture

[Details](details.md#working-anchor)
DOC
cat >"${WORK_DIR}/docs/details.md" <<'DOC'
# Details

## Working anchor
DOC
cat >"${WORK_DIR}/docs/navigation.json" <<'JSON'
{"version":1,"renderer":"github-markdown","sections":[{"title":"Fixture","pages":[{"title":"Home","path":"index.md"},{"title":"Details","path":"details.md"}]}]}
JSON
cat >"${WORK_DIR}/README.md" <<'DOC'
# Public fixture

[Asset](asset.svg)
DOC
printf '<svg xmlns="http://www.w3.org/2000/svg"/>\n' >"${WORK_DIR}/asset.svg"

"${ROOT}/scripts/check-public-docs.py" --docs-root "${WORK_DIR}/docs" \
  --public-file "${WORK_DIR}/README.md" \
  --write --manifest "${WORK_DIR}/docs/site-manifest.json" >/dev/null
"${ROOT}/scripts/check-public-docs.py" --docs-root "${WORK_DIR}/docs" \
  --public-file "${WORK_DIR}/README.md" \
  --check --manifest "${WORK_DIR}/docs/site-manifest.json" >/dev/null

printf '\n[Broken](missing.md)\n' >>"${WORK_DIR}/README.md"
if "${ROOT}/scripts/check-public-docs.py" --docs-root "${WORK_DIR}/docs" \
  --public-file "${WORK_DIR}/README.md" \
  --check --manifest "${WORK_DIR}/docs/site-manifest.json" \
  >"${WORK_DIR}/broken.log" 2>&1; then
  echo "public docs checker accepted a broken link" >&2
  exit 1
fi
grep -qF 'broken local link' "${WORK_DIR}/broken.log"
