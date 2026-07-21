#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"
source release/tool-versions.env

allow_dirty=false
case "${1:-}" in
  "") ;;
  --allow-dirty) allow_dirty=true ;;
  -h | --help)
    echo "usage: release/checks/open-source-readiness.sh [--allow-dirty]"
    exit 0
    ;;
  *)
    echo "unknown argument: ${1}" >&2
    exit 2
    ;;
esac

required=(cargo git gitleaks tar)
for command_name in "${required[@]}"; do
  command -v "${command_name}" >/dev/null 2>&1 || {
    echo "required command not found: ${command_name}" >&2
    exit 1
  }
done

[[ "$(gitleaks version)" == "${GITLEAKS_VERSION}" ]] || {
  echo "gitleaks ${GITLEAKS_VERSION} is required" >&2
  exit 1
}

gitleaks git . --config .gitleaks.toml --redact=100 --no-banner --no-color
scan_root="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-gitleaks.XXXXXX")"
trap 'rm -rf "${scan_root}"' EXIT
git ls-files --cached --others --exclude-standard -z \
  | tar --null -T - -cf - \
  | tar -xf - -C "${scan_root}"
gitleaks dir "${scan_root}" --config .gitleaks.toml \
  --redact=100 --no-banner --no-color
tests/shell/gitleaks_config_smoke.sh
cargo fmt --check
cargo clippy --workspace --exclude context-pg --all-targets --all-features -- -D warnings
cargo test --workspace --exclude context-pg --all-features
cargo doc --workspace --exclude context-pg --no-deps

pg_config="${PG_CONFIG:-$(command -v pg_config || true)}"
[[ -n "${pg_config}" && -x "${pg_config}" ]] || {
  echo "PostgreSQL 17 pg_config is required; set PG_CONFIG" >&2
  exit 1
}
case "$("${pg_config}" --version)" in
  'PostgreSQL 17.'*) ;;
  *) echo "PG_CONFIG must identify PostgreSQL 17: $("${pg_config}" --version)" >&2; exit 1 ;;
esac
PG_CONFIG="${pg_config}" cargo check -p context-pg --no-default-features --features pg17
PG_CONFIG="${pg_config}" cargo clippy -p context-pg --all-targets \
  --no-default-features --features pg17 -- -D warnings
PG_CONFIG="${pg_config}" cargo doc -p context-pg --no-default-features --features pg17 --no-deps
scripts/check-crate-boundaries.sh
scripts/check-repository-contract.sh
scripts/check-capability-contract.sh
scripts/check-parity-matrix.sh --require-v1-launch-complete
scripts/generate-sql-object-inventory.sh --check
scripts/check-public-docs.py --check
tests/shell/check_public_docs_smoke.sh
scripts/check-source-hygiene.sh
scripts/check-unsafe-safety-comments.sh
tests/shell/check_repository_contract_smoke.sh
tests/shell/release_public_surface_smoke.sh
bash -n scripts/*.sh release/*.sh release/checks/*.sh tests/shell/*.sh

command -v docker >/dev/null 2>&1 || {
  echo "Docker with Compose v2 is required for release configuration validation" >&2
  exit 1
}
docker compose -f release/docker/compose.yml config --quiet

mkdir -p target
package_out="$(mktemp -d "${ROOT}/target/source-readiness-package.XXXXXX")"
package_args=(--out-dir "${package_out}")
if [[ "${allow_dirty}" == true ]]; then
  package_args+=(--allow-dirty)
fi
release/build-packages.sh "${package_args[@]}" v0.1.0

echo "open-source readiness gates passed"
