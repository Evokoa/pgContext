#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-install-report.XXXXXX")"
trap 'rm -rf "${WORK_DIR}"' EXIT

cd "${ROOT}"
if scripts/run-install-report.sh \
  --dry-run \
  --allow-dirty \
  --pg-config /missing/pg_config \
  --out-dir "${WORK_DIR}/report"; then
  echo "dry-run install report unexpectedly approved a candidate" >&2
  exit 1
fi

grep -qF $'source_archive\tplanned\t0' "${WORK_DIR}/report/summary.tsv"
grep -qF $'docker_demo\tplanned\t0' "${WORK_DIR}/report/summary.tsv"
grep -qF $'negative_installs\tplanned\t0' "${WORK_DIR}/report/summary.tsv"
grep -qF -- '- Environment: `' "${WORK_DIR}/report/report.md"
grep -qF -- '- Rust: `' "${WORK_DIR}/report/report.md"
grep -qF -- '- Cargo: `' "${WORK_DIR}/report/report.md"
grep -qF -- '- Docker: `' "${WORK_DIR}/report/report.md"
grep -qF -- '- Invocation: `scripts/run-install-report.sh --pg-config /missing/pg_config --out-dir ' "${WORK_DIR}/report/report.md"
grep -qF -- '- Started UTC: `' "${WORK_DIR}/report/report.md"
grep -qF -- '- Finished UTC: `' "${WORK_DIR}/report/report.md"
grep -qF -- '- Waiver: `diagnostic-only dirty/dry-run override; approval remains incomplete`' "${WORK_DIR}/report/report.md"
if grep -Eq 'vX\.Y\.Z|--pg-config PG17' "${WORK_DIR}/report/summary.tsv"; then
  echo 'install report retained a placeholder command' >&2
  exit 1
fi
grep -qF 'Overall: `incomplete`' "${WORK_DIR}/report/report.md"
