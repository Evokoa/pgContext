#!/usr/bin/env bash
set -uo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
PG_CONFIG_BIN="${PG_CONFIG:-}"
OUT_DIR=""
ALLOW_DIRTY=0
DRY_RUN=0

usage() {
  cat <<'USAGE'
Usage: scripts/run-install-report.sh [options]

Verify the source archive and Docker playground and write SHA-named evidence.

Options:
  --pg-config PATH  PostgreSQL 17 pg_config for the source install.
  --out-dir PATH    Report directory. Defaults under target/install-gates/COMMIT.
  --allow-dirty     Permit diagnostics, but keep approval incomplete.
  --dry-run         Write the command plan without installing or using Docker.
  -h, --help        Show this help.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --pg-config)
      [[ $# -ge 2 ]] || { echo "--pg-config requires a path" >&2; exit 2; }
      PG_CONFIG_BIN="$2"
      shift 2
      ;;
    --out-dir)
      [[ $# -ge 2 ]] || { echo "--out-dir requires a path" >&2; exit 2; }
      OUT_DIR="$2"
      shift 2
      ;;
    --allow-dirty)
      ALLOW_DIRTY=1
      shift
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

SHA="$(git -C "${REPO_ROOT}" rev-parse HEAD)"
SHORT_SHA="${SHA:0:12}"
STARTED_UTC="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
if [[ -z "${OUT_DIR}" ]]; then
  OUT_DIR="${REPO_ROOT}/target/install-gates/${SHA}"
elif [[ "${OUT_DIR}" != /* ]]; then
  OUT_DIR="${REPO_ROOT}/${OUT_DIR}"
fi
case "${OUT_DIR}" in
  "" | "/") echo "--out-dir must be a non-root path" >&2; exit 2 ;;
esac

WORKTREE="clean"
if [[ -n "$(git -C "${REPO_ROOT}" status --short)" ]]; then
  WORKTREE="dirty"
fi
if [[ "${DRY_RUN}" -eq 0 && "${ALLOW_DIRTY}" -eq 0 && "${WORKTREE}" == "dirty" ]]; then
  echo "dirty worktree cannot produce install evidence; use --allow-dirty for diagnostics" >&2
  exit 1
fi

if [[ -z "${PG_CONFIG_BIN}" ]]; then
  PG_CONFIG_BIN="$(command -v pg_config || true)"
fi
if [[ "${DRY_RUN}" -eq 0 ]]; then
  if [[ -z "${PG_CONFIG_BIN}" || ! -x "${PG_CONFIG_BIN}" ]]; then
    echo "PostgreSQL 17 pg_config is required" >&2
    exit 1
  fi
  PG_VERSION="$(${PG_CONFIG_BIN} --version)"
  [[ "${PG_VERSION}" == "PostgreSQL 17"* ]] || {
    echo "pg_config must report PostgreSQL 17: ${PG_VERSION}" >&2
    exit 1
  }
else
  PG_VERSION="not executed"
fi

VERSION="$(awk '/^default_version[[:space:]]*=/ { value = $3; gsub(/\047/, "", value); print value; exit }' "${REPO_ROOT}/pgcontext.control")"
ARTIFACT_DIR="${OUT_DIR}/artifacts"
SOURCE_ARCHIVE="${ARTIFACT_DIR}/pgContext-${VERSION}.zip"
PSQL_BIN="$(dirname "${PG_CONFIG_BIN:-/missing/pg_config}")/psql"
DBNAME="pgcontext_install_${SHORT_SHA}"
DBNAME="${DBNAME//-/_}"
WORK_DIR=""
COMPOSE_FILE="${REPO_ROOT}/release/docker/compose.yml"
SUMMARY="${OUT_DIR}/summary.tsv"
REPORT="${OUT_DIR}/report.md"
OVERALL=0
HOST="$(uname -srm)"
RUST_VERSION="$(rustc -V 2>/dev/null || printf unavailable)"
CARGO_VERSION="$(cargo -V 2>/dev/null || printf unavailable)"
DOCKER_VERSION="$(docker --version 2>/dev/null || printf unavailable)"
WAIVER="none"
if [[ "${ALLOW_DIRTY}" -eq 1 || "${DRY_RUN}" -eq 1 ]]; then
  WAIVER="diagnostic-only dirty/dry-run override; approval remains incomplete"
fi
INVOCATION="scripts/run-install-report.sh --pg-config ${PG_CONFIG_BIN:-not-selected} --out-dir ${OUT_DIR}"

mkdir -p "${OUT_DIR}"
printf 'gate\tstatus\texit_code\tlog\tcommand\n' >"${SUMMARY}"
{
  printf '# PostgreSQL 17 Install Report\n\n'
  printf -- '- Commit: `%s`\n' "${SHA}"
  printf -- '- Worktree: `%s`\n' "${WORKTREE}"
  printf -- '- Environment: `%s`\n' "${HOST}"
  printf -- '- Rust: `%s`\n' "${RUST_VERSION}"
  printf -- '- Cargo: `%s`\n' "${CARGO_VERSION}"
  printf -- '- Docker: `%s`\n' "${DOCKER_VERSION}"
  printf -- '- PostgreSQL: `%s`\n' "${PG_VERSION}"
  printf -- '- pg_config: `%s`\n' "${PG_CONFIG_BIN:-not selected}"
  printf -- '- Invocation: `%s`\n' "${INVOCATION}"
  printf -- '- Started UTC: `%s`\n' "${STARTED_UTC}"
  printf -- '- Waiver: `%s`\n' "${WAIVER}"
  printf -- '- Mode: `%s`\n\n' "$([[ "${DRY_RUN}" -eq 1 ]] && printf dry-run || printf run)"
  printf '| Gate | Status | Log |\n'
  printf '|---|---|---|\n'
} >"${REPORT}"

cleanup() {
  if [[ -n "${WORK_DIR}" && -d "${WORK_DIR}" ]]; then
    rm -rf "${WORK_DIR}"
  fi
  if [[ "${DRY_RUN}" -eq 0 ]] && command -v docker >/dev/null 2>&1; then
    docker compose -f "${COMPOSE_FILE}" --project-directory "${REPO_ROOT}" \
      down --volumes --remove-orphans >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

run_gate() {
  local gate="$1"
  local command="$2"
  local function_name="$3"
  local log="${OUT_DIR}/${gate}.log"
  local status="passed"
  local code=0

  if [[ "${DRY_RUN}" -eq 1 ]]; then
    status="planned"
    : >"${log}"
  else
    "${function_name}" >"${log}" 2>&1
    code=$?
    if [[ "${code}" -ne 0 ]]; then
      status="failed"
      OVERALL=1
    fi
  fi
  printf '%s\t%s\t%s\t%s\t%s\n' "${gate}" "${status}" "${code}" "${log}" "${command}" >>"${SUMMARY}"
  printf '| `%s` | `%s` | `%s` |\n' "${gate}" "${status}" "${log}" >>"${REPORT}"
}

package_source() {
  local -a package_args
  mkdir -p "${ARTIFACT_DIR}"
  package_args=(--out-dir "${ARTIFACT_DIR}")
  if [[ "${ALLOW_DIRTY}" -eq 1 ]]; then
    package_args+=(--allow-dirty)
  fi
  "${REPO_ROOT}/release/build-packages.sh" "${package_args[@]}" "v${VERSION}"
  WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-install-${SHORT_SHA}.XXXXXX")"
  mkdir -p "${WORK_DIR}/source"
  unzip -q "${SOURCE_ARCHIVE}" -d "${WORK_DIR}/source"
  test -f "${WORK_DIR}/source/pgContext-${VERSION}/Cargo.lock"
  [[ "$(jq -r .commit "${ARTIFACT_DIR}/PROVENANCE.json")" == "${SHA}" ]]
  [[ "$(jq -r .dirty "${ARTIFACT_DIR}/PROVENANCE.json")" == "$([[ "${WORKTREE}" == dirty ]] && printf true || printf false)" ]]
}

install_source() {
  cd "${WORK_DIR}/source/pgContext-${VERSION}"
  make install PG_CONFIG="${PG_CONFIG_BIN}"
  test -f "$("${PG_CONFIG_BIN}" --sharedir)/extension/pgcontext--0.1.0--0.2.0.sql"
}

source_demo() {
  "${PSQL_BIN}" -h localhost -p 28817 -d postgres -v ON_ERROR_STOP=1 \
    -c "DROP DATABASE IF EXISTS ${DBNAME}" \
    -c "CREATE DATABASE ${DBNAME}"
  cd "${WORK_DIR}/source/pgContext-${VERSION}"
  "${PSQL_BIN}" -h localhost -p 28817 -d "${DBNAME}" -v ON_ERROR_STOP=1 \
    -f playground/demo.sql
}

source_remove_recreate() {
  "${PSQL_BIN}" -h localhost -p 28817 -d "${DBNAME}" -v ON_ERROR_STOP=1 \
    -c "SELECT pgcontext.drop_collection('playground_docs')" \
    -c 'DROP TABLE public.pgcontext_playground_docs' \
    -c 'DROP EXTENSION pgcontext' \
    -c 'CREATE EXTENSION pgcontext' \
    -c "SELECT extversion FROM pg_extension WHERE extname = 'pgcontext'"
}

docker_demo() {
  cd "${REPO_ROOT}"
  scripts/quickstart.sh demo
  local container_id
  container_id="$(docker compose -f "${COMPOSE_FILE}" --project-directory "${REPO_ROOT}" ps -q postgres)"
  [[ -n "${container_id}" ]]
  [[ "$(docker inspect --format '{{.State.Health.Status}}' "${container_id}")" == "healthy" ]]
  scripts/quickstart.sh clean
  [[ -z "$(docker compose -f "${COMPOSE_FILE}" --project-directory "${REPO_ROOT}" ps -q postgres)" ]]
}

negative_installs() {
  cd "${REPO_ROOT}"
  tests/shell/build_packages_negative_smoke.sh
  local docker_log="${OUT_DIR}/unsupported-docker-major.log"
  if docker build --build-arg PG_MAJOR=16 --target builder \
    -f release/docker/Dockerfile . >"${docker_log}" 2>&1; then
    echo "Docker build accepted unsupported PG_MAJOR=16" >&2
    return 1
  fi
  grep -qF 'pgContext V1 only supports PostgreSQL 17' "${docker_log}"
  cat "${docker_log}"
}

run_gate source_archive \
  "release/build-packages.sh --out-dir ${ARTIFACT_DIR} v${VERSION}; verify and unpack ${SOURCE_ARCHIVE} outside checkout" package_source
run_gate source_install \
  "cargo pgrx install -p context-pg --release --pg-config ${PG_CONFIG_BIN} --no-default-features --features pg17 (from ${SOURCE_ARCHIVE})" install_source
run_gate source_demo \
  "psql -h localhost -p 28817 -d ${DBNAME}; CREATE EXTENSION; run playground/demo.sql from ${SOURCE_ARCHIVE}" source_demo
run_gate source_remove_recreate \
  'drop collection/table/extension; recreate extension' source_remove_recreate
run_gate docker_demo \
  'scripts/quickstart.sh demo; healthcheck; scripts/quickstart.sh clean' docker_demo
run_gate negative_installs \
  'reject stale/symlinked payload output and Docker PG_MAJOR=16' negative_installs

if [[ "${WORKTREE}" == "dirty" || "${DRY_RUN}" -eq 1 ]]; then
  OVERALL=1
fi
FINISHED_UTC="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
printf '\n- Finished UTC: `%s`\n' "${FINISHED_UTC}" >>"${REPORT}"
printf '\nOverall: `%s`\n' "$([[ "${OVERALL}" -eq 0 ]] && printf passed || printf incomplete)" >>"${REPORT}"
printf 'install report written to %s\n' "${OUT_DIR}"
exit "${OVERALL}"
