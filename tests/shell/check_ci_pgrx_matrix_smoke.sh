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

coexist_job="$(job_block pgvector-coexist)"
if [[ -z "${coexist_job}" ]]; then
  echo "CI must define a pgvector coexistence job" >&2
  exit 1
fi
grep -qF "awk '\$1 == \"17\" && \$2 == \"main\" { print \$3; exit }'" \
  <<<"${coexist_job}"
grep -qF '/usr/lib/postgresql/17/bin/pg_isready' <<<"${coexist_job}"
grep -qF 'echo "PGCONTEXT_CI_PG_PORT=${pg_port}" >> "${GITHUB_ENV}"' \
  <<<"${coexist_job}"
grep -qF 'postgres_psql="sudo -u postgres psql -p ${PGCONTEXT_CI_PG_PORT}"' \
  <<<"${coexist_job}"
grep -qF 'PGCONTEXT_BRIDGE_PG_DUMP="${postgres_dump}"' <<<"${coexist_job}"
grep -qF 'PGPORT="${PGCONTEXT_CI_PG_PORT}"' <<<"${coexist_job}"

bench_workflow=.github/workflows/bench-regression.yml
grep -qF '/usr/lib/postgresql/17/bin/pg_isready' "${bench_workflow}"
grep -qF 'createuser -p "${pg_port}"' "${bench_workflow}"
grep -qF 'port=${PGCONTEXT_CI_PG_PORT} dbname=postgres' "${bench_workflow}"

if rg -n 'cargo pgrx test' .github/workflows scripts tests/heavy \
  --glob '*.yml' --glob '*.yaml' --glob '*.sh'; then
  echo "CI and release scripts must run pg_tests through the in-server runner" >&2
  exit 1
fi
if rg -n 'cargo test -p context-pg' scripts/run-postgres-matrix-gates.sh; then
  echo "PostgreSQL matrix must not link context-pg test binaries standalone" >&2
  exit 1
fi
grep -qF 'cargo check -p context-pg --tests' scripts/run-postgres-matrix-gates.sh
grep -qF 'PG_MAJOR=${major} scripts/run-v1-pgrx-tests.sh' \
  scripts/run-postgres-matrix-gates.sh
grep -qF 'PG_MAJOR=${PG_MAJOR} scripts/run-v1-pgrx-tests.sh' \
  scripts/run-security-review-report.sh

for replica_script in \
  tests/heavy/exact_first_replica_promotion.sh \
  tests/heavy/ivfflat_replica_promotion.sh
do
  if grep -qF '/private/tmp' "${replica_script}"; then
    echo "replica socket path is macOS-specific: ${replica_script}" >&2
    exit 1
  fi
  grep -qF 'SOCKET_ROOT="${HEAVY_SOCKET_ROOT:-/tmp}"' "${replica_script}"
done
