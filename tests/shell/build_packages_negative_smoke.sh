#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-package-negative.XXXXXX")"
trap 'rm -rf "${WORK_DIR}"' EXIT

cd "${ROOT}"

mkdir -p "${WORK_DIR}/stale"
touch "${WORK_DIR}/stale/old-artifact"
if release/build-packages.sh --allow-dirty \
  --out-dir "${WORK_DIR}/stale" v0.1.0 >"${WORK_DIR}/stale.log" 2>&1; then
  echo "package build accepted stale output" >&2
  exit 1
fi
grep -qF 'output directory must be empty' "${WORK_DIR}/stale.log"

mkdir -p "${WORK_DIR}/real-output"
ln -s "${WORK_DIR}/real-output" "${WORK_DIR}/linked-output"
if release/build-packages.sh --allow-dirty \
  --out-dir "${WORK_DIR}/linked-output" v0.1.0 >"${WORK_DIR}/symlink.log" 2>&1; then
  echo "package build accepted a symlinked output directory" >&2
  exit 1
fi
grep -qF 'through a symlink' "${WORK_DIR}/symlink.log"

if release/build-packages.sh --allow-dirty \
  --out-dir "${WORK_DIR}/bad-tag" 0.1.0 >"${WORK_DIR}/tag.log" 2>&1; then
  echo "package build accepted an invalid release tag" >&2
  exit 1
fi
grep -qF 'TAG must use vX.Y.Z form' "${WORK_DIR}/tag.log"

grep -qF 'pgContext V1 only supports PostgreSQL 17' release/docker/Dockerfile
