#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-release-validation.XXXXXX")"
trap 'rm -rf "${WORK_DIR}"' EXIT

cd "${ROOT}"
scripts/validate-release.py --tag v0.1.0

if scripts/validate-release.py --tag 0.1.0 >"${WORK_DIR}/bad-tag.log" 2>&1; then
  echo "release validation accepted a tag without the v prefix" >&2
  exit 1
fi
grep -qF 'tag must use vX.Y.Z form' "${WORK_DIR}/bad-tag.log"

if scripts/validate-release.py --tag v0.1.1 >"${WORK_DIR}/bad-version.log" 2>&1; then
  echo "release validation accepted mismatched package metadata" >&2
  exit 1
fi
grep -qF 'context-pg version' "${WORK_DIR}/bad-version.log"
