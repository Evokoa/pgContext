#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TMPDIR="${TMPDIR:-${REPO_ROOT}/target/tmp}"
mkdir -p "${TMPDIR}"
work_dir="$(mktemp -d "${TMPDIR}/repository-contract-test.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT

"${REPO_ROOT}/scripts/check-repository-contract.sh"

make_fixture() {
  local root="$1"
  mkdir -p "${root}/docs/user_guide" "${root}/release/docker"
  cp "${REPO_ROOT}/Cargo.toml" "${root}/Cargo.toml"
  cp "${REPO_ROOT}/README.md" "${root}/README.md"
  cp "${REPO_ROOT}/META.json" "${root}/META.json"
  cp "${REPO_ROOT}/docs/user_guide/support_policy.md" \
    "${root}/docs/user_guide/support_policy.md"
  cp "${REPO_ROOT}/release/docker/Dockerfile" \
    "${root}/release/docker/Dockerfile"
  cp "${REPO_ROOT}/release/tool-versions.env" \
    "${root}/release/tool-versions.env"
}

wrong_identity="${work_dir}/wrong-identity"
make_fixture "${wrong_identity}"
perl -0pi -e 's#github\.com/evokoa/pgcontext#github.com/pgcontext/pgcontext#g' \
  "${wrong_identity}/META.json"
if REPO_ROOT="${wrong_identity}" "${REPO_ROOT}/scripts/check-repository-contract.sh" \
  2>"${work_dir}/wrong-identity.err"; then
  echo "obsolete repository identity should fail" >&2
  exit 1
fi
grep -qF 'obsolete repository identity' "${work_dir}/wrong-identity.err"

wrong_support="${work_dir}/wrong-support"
make_fixture "${wrong_support}"
perl -0pi -e 's/supported-postgres-versions = \["17", "18"\]/supported-postgres-versions = ["17"]/' \
  "${wrong_support}/Cargo.toml"
if REPO_ROOT="${wrong_support}" "${REPO_ROOT}/scripts/check-repository-contract.sh" \
  2>"${work_dir}/wrong-support.err"; then
  echo "unsupported PostgreSQL support promotion should fail" >&2
  exit 1
fi
grep -qF 'supported-postgres-versions must contain PostgreSQL 17 and 18' \
  "${work_dir}/wrong-support.err"
