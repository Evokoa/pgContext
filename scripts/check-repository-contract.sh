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
  'documentation = "https://github.com/evokoa/pgcontext/tree/master/docs"'
do
  grep -qF "${expected}" Cargo.toml || {
    echo "Cargo workspace metadata is missing: ${expected}" >&2
    exit 1
  }
done

grep -qF 'supported-postgres-versions = ["17", "18"]' Cargo.toml || {
  echo "supported-postgres-versions must contain PostgreSQL 17 and 18" >&2
  exit 1
}
grep -qF 'planned-postgres-versions = []' Cargo.toml || {
  echo "planned-postgres-versions must be empty" >&2
  exit 1
}
grep -qF 'legacy-best-effort-postgres-versions = []' Cargo.toml || {
  echo "legacy-best-effort-postgres-versions must be empty" >&2
  exit 1
}

grep -qF 'PostgreSQL 17 and 18 are supported release targets' \
  docs/user_guide/support_policy.md || {
  echo "support policy does not identify PostgreSQL 17 and 18 as release targets" >&2
  exit 1
}
grep -qF 'linux/amd64 and linux/arm64' docs/user_guide/support_policy.md || {
  echo "support policy does not name both release image architectures" >&2
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
