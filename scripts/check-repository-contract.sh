#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "${REPO_ROOT}"

required_files=(
  Cargo.toml
  README.md
  META.json
  docs/user_guide/support_policy.md
  release/docker/Dockerfile
  release/tool-versions.env
)
for path in "${required_files[@]}"; do
  [[ -f "${path}" ]] || {
    echo "repository contract input is missing: ${path}" >&2
    exit 1
  }
done

if grep -R -nF 'github.com/pgcontext/pgcontext' "${required_files[@]}" >&2; then
  echo "obsolete repository identity found; use github.com/evokoa/pgcontext" >&2
  exit 1
fi

for expected in \
  'repository = "https://github.com/evokoa/pgcontext"' \
  'homepage = "https://github.com/evokoa/pgcontext"' \
  'documentation = "https://github.com/evokoa/pgcontext/tree/main/docs"'
do
  grep -qF "${expected}" Cargo.toml || {
    echo "Cargo workspace metadata is missing: ${expected}" >&2
    exit 1
  }
done

grep -qF 'supported-postgres-versions = ["17"]' Cargo.toml || {
  echo "supported-postgres-versions must contain only PostgreSQL 17" >&2
  exit 1
}
grep -qF 'planned-postgres-versions = ["15", "16", "18"]' Cargo.toml || {
  echo "planned-postgres-versions must contain PostgreSQL 15, 16, and 18" >&2
  exit 1
}
grep -qF 'legacy-best-effort-postgres-versions = ["14"]' Cargo.toml || {
  echo "legacy-best-effort-postgres-versions must contain PostgreSQL 14" >&2
  exit 1
}

grep -qF 'PostgreSQL 17 is the only supported V1 major' \
  docs/user_guide/support_policy.md || {
  echo "support policy does not identify PostgreSQL 17 as the V1 target" >&2
  exit 1
}
grep -qF '15, 16,' docs/user_guide/support_policy.md || {
  echo "support policy does not name planned PostgreSQL targets" >&2
  exit 1
}
grep -qF 'PostgreSQL 14 is legacy best-effort' \
  docs/user_guide/support_policy.md || {
  echo "support policy does not name PostgreSQL 14 as legacy best-effort" >&2
  exit 1
}

source release/tool-versions.env
grep -qF "pgrx = \"=${CARGO_PGRX_VERSION}\"" Cargo.toml || {
  echo "workspace pgrx version differs from release/tool-versions.env" >&2
  exit 1
}
grep -qF "ARG PGRX_VERSION=${CARGO_PGRX_VERSION}" release/docker/Dockerfile || {
  echo "Docker cargo-pgrx version differs from release/tool-versions.env" >&2
  exit 1
}
