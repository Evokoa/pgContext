#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-build-packages.XXXXXX")"
trap 'rm -rf "${WORK_DIR}"' EXIT
cd "${ROOT}"
DIRTY=0
if [[ -n "$(git status --porcelain=v1)" ]]; then
  DIRTY=1
fi

verify_payload() {
  local payload="$1"
  if [[ "${DIRTY}" -eq 1 ]]; then
    scripts/verify-release-payload.py --allow-dirty --tag v0.2.0 \
      --candidate-sha "$(git rev-parse HEAD)" "${payload}"
  else
    scripts/verify-release-payload.py --tag v0.2.0 \
      --candidate-sha "$(git rev-parse HEAD)" "${payload}"
  fi
}

for build in first second; do
  release/build-packages.sh --allow-dirty \
    --out-dir "${WORK_DIR}/${build}" v0.2.0 >/dev/null
  verify_payload "${WORK_DIR}/${build}" >/dev/null
done
diff -r "${WORK_DIR}/first" "${WORK_DIR}/second"

grep -qF '"spdxVersion": "SPDX-2.3"' "${WORK_DIR}/first/SBOM.spdx.json"
grep -qF '"reproducible_source": true' "${WORK_DIR}/first/PROVENANCE.json"
grep -qF '"signed": false' "${WORK_DIR}/first/PROVENANCE.json"
grep -qiF 'unsigned' "${WORK_DIR}/first/ARTIFACT_POLICY.md"

cp -R "${WORK_DIR}/first" "${WORK_DIR}/tampered"
printf 'tampered\n' >>"${WORK_DIR}/tampered/NOTICE"
if verify_payload "${WORK_DIR}/tampered" >"${WORK_DIR}/tampered.log" 2>&1; then
  echo "payload verifier accepted a checksum mismatch" >&2
  exit 1
fi
grep -qF 'SHA-256 mismatch for NOTICE' "${WORK_DIR}/tampered.log"

ln -s "${WORK_DIR}/first" "${WORK_DIR}/linked-payload"
if verify_payload "${WORK_DIR}/linked-payload" >"${WORK_DIR}/linked.log" 2>&1; then
  echo "payload verifier accepted a symlinked payload directory" >&2
  exit 1
fi
grep -qF 'payload directory must not be a symbolic link' "${WORK_DIR}/linked.log"
