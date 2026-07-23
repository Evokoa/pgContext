#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"
workflow=.github/workflows/ci.yml

job_block() {
  awk -v job="$1" '
    $0 == "  " job ":" { found = 1 }
    found && $0 ~ /^  [a-z0-9-]+:$/ && $0 != "  " job ":" { exit }
    found { print }
  ' "${workflow}"
}

pgrx_job="$(job_block pgrx)"
if [[ -z "${pgrx_job}" ]]; then
  echo "CI must define a pgrx job" >&2
  exit 1
fi

grep -qF 'name: pgrx (PG${{ matrix.pg }})' <<<"${pgrx_job}"
grep -qF 'fail-fast: false' <<<"${pgrx_job}"
grep -qF 'pg: ["17", "18"]' <<<"${pgrx_job}"
grep -qF 'postgresql-${{ matrix.pg }} postgresql-server-dev-${{ matrix.pg }}' \
  <<<"${pgrx_job}"
grep -qF 'cargo install cargo-pgrx --version 0.19.1 --locked' <<<"${pgrx_job}"
grep -qF 'cargo pgrx init --pg${{ matrix.pg }} /usr/lib/postgresql/${{ matrix.pg }}/bin/pg_config' \
  <<<"${pgrx_job}"
grep -qF 'PG_MAJOR=${{ matrix.pg }} scripts/run-v1-pgrx-tests.sh' <<<"${pgrx_job}"

if grep -Eq 'pg:.*(15|16)' <<<"${pgrx_job}"; then
  echo "pgrx certification matrix must not include unsupported majors" >&2
  exit 1
fi
