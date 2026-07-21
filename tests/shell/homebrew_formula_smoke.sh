#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-homebrew.XXXXXX")"
trap 'rm -rf "${WORK_DIR}"' EXIT

cd "${ROOT}"
scripts/build-pgxn-dist.sh --allow-dirty --out-dir "${WORK_DIR}/dist" v0.1.0
scripts/render-homebrew-formula.sh \
  --archive "${WORK_DIR}/dist/pgContext-0.1.0.zip" \
  --out-dir "${WORK_DIR}/Formula"

formula="${WORK_DIR}/Formula/pgcontext.rb"
grep -qF 'releases/download/v0.1.0/pgContext-0.1.0.zip' "${formula}"
grep -qE '^[[:space:]]+sha256 "[0-9a-f]{64}"$' "${formula}"
grep -qF 'depends_on "pgrx@0.19.1" => :build' "${formula}"
grep -qF 'depends_on "postgresql@17" => [:build, :test]' "${formula}"
grep -qF 'CREATE EXTENSION pgcontext;' "${formula}"
grep -qF "OPERATOR(pgcontext.<->)" "${formula}"

ruby -c "${WORK_DIR}/Formula/pgcontext.rb"
ruby -c "${WORK_DIR}/Formula/pgrx@0.19.1.rb"
