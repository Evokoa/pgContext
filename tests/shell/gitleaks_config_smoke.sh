#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-gitleaks-config.XXXXXX")"
trap 'rm -rf "${WORK_DIR}"' EXIT

mkdir -p "${WORK_DIR}/allowed" "${WORK_DIR}/rejected"
printf '%s%s%s\n' 'MARKER = b"-----BEGIN ' 'PRIVATE KEY-----' '"' \
  >"${WORK_DIR}/allowed/detector.py"
gitleaks dir "${WORK_DIR}/allowed" --config "${ROOT}/.gitleaks.toml" \
  --redact=100 --no-banner --no-color >/dev/null

{
  printf '%s%s%s\n' '-----BEGIN ' 'PRIVATE ' 'KEY-----'
  printf '%s%s\n' \
    'MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC9' \
    'RuntimeOnlyMaterial'
  printf '%s%s%s\n' '-----END ' 'PRIVATE ' 'KEY-----'
} >"${WORK_DIR}/rejected/leaked.key"
if gitleaks dir "${WORK_DIR}/rejected" --config "${ROOT}/.gitleaks.toml" \
  --redact=100 --no-banner --no-color >/dev/null 2>&1; then
  echo "gitleaks config allowed private-key material outside detector syntax" >&2
  exit 1
fi
