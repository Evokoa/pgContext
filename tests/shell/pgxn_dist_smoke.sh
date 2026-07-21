#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-pgxn-dist.XXXXXX")"
trap 'rm -rf "${WORK_DIR}"' EXIT

cd "${ROOT}"
mkdir -p "${WORK_DIR}/invalid-meta"
cp META.json "${WORK_DIR}/invalid-meta/META.json"
jq '.resources.documentation = "https://example.invalid/docs"' \
  "${WORK_DIR}/invalid-meta/META.json" \
  >"${WORK_DIR}/invalid-meta/META.changed.json"
mv "${WORK_DIR}/invalid-meta/META.changed.json" \
  "${WORK_DIR}/invalid-meta/META.json"
if scripts/validate-pgxn-meta.sh "${WORK_DIR}/invalid-meta/META.json" \
  >"${WORK_DIR}/invalid-meta.log" 2>&1; then
  echo "canonical PGXN validator accepted an unknown resource key" >&2
  exit 1
fi
grep -qF 'Unknown key' "${WORK_DIR}/invalid-meta.log"

scripts/build-pgxn-dist.sh --allow-dirty --out-dir "${WORK_DIR}" v0.1.0
scripts/verify-pgxn-dist.py --tag v0.1.0 "${WORK_DIR}/pgContext-0.1.0.zip"

if scripts/build-pgxn-dist.sh --allow-dirty --out-dir "${WORK_DIR}" 0.1.0 \
  >"${WORK_DIR}/bad-tag.log" 2>&1; then
  echo "PGXN builder accepted a tag without the v prefix" >&2
  exit 1
fi
grep -qF 'TAG must use vX.Y.Z form' "${WORK_DIR}/bad-tag.log"

for mode in secret binary allowlisted-binary identity metadata-key; do
  mkdir -p "${WORK_DIR}/${mode}"
  unzip -q "${WORK_DIR}/pgContext-0.1.0.zip" -d "${WORK_DIR}/${mode}/tree"
  case "${mode}" in
    secret)
      printf '%s%s\n%s\n' '-----BEGIN ' 'PRIVATE KEY-----' 'not-a-real-key' \
        >"${WORK_DIR}/${mode}/tree/pgContext-0.1.0/leaked.key"
      expected='private key material'
      ;;
    binary)
      printf '\177ELF\000fixture' \
        >"${WORK_DIR}/${mode}/tree/pgContext-0.1.0/compiled.so"
      expected='unexpected binary content'
      ;;
    allowlisted-binary)
      printf '\177ELF\000fixture' \
        >"${WORK_DIR}/${mode}/tree/pgContext-0.1.0/assets/pgcontext-banner.png"
      expected='allowlisted binary has an unexpected signature'
      ;;
    identity)
      sed 's#https://github.com/evokoa/pgcontext#https://example.invalid/project#' \
        "${WORK_DIR}/${mode}/tree/pgContext-0.1.0/Cargo.toml" \
        >"${WORK_DIR}/${mode}/tree/pgContext-0.1.0/Cargo.toml.changed"
      mv "${WORK_DIR}/${mode}/tree/pgContext-0.1.0/Cargo.toml.changed" \
        "${WORK_DIR}/${mode}/tree/pgContext-0.1.0/Cargo.toml"
      expected='repository identity'
      ;;
    metadata-key)
      jq '.resources.documentation = "https://example.invalid/docs"' \
        "${WORK_DIR}/${mode}/tree/pgContext-0.1.0/META.json" \
        >"${WORK_DIR}/${mode}/tree/pgContext-0.1.0/META.json.changed"
      mv "${WORK_DIR}/${mode}/tree/pgContext-0.1.0/META.json.changed" \
        "${WORK_DIR}/${mode}/tree/pgContext-0.1.0/META.json"
      expected='invalid resource keys: documentation'
      ;;
  esac
  (
    cd "${WORK_DIR}/${mode}/tree"
    zip -qr "../pgContext-0.1.0.zip" pgContext-0.1.0
  )
  if scripts/verify-pgxn-dist.py --tag v0.1.0 \
    "${WORK_DIR}/${mode}/pgContext-0.1.0.zip" \
    >"${WORK_DIR}/${mode}.log" 2>&1; then
    echo "PGXN verifier accepted ${mode} content" >&2
    exit 1
  fi
  grep -qF "${expected}" "${WORK_DIR}/${mode}.log"
done
