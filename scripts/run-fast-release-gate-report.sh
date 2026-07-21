#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
out_dir="${FAST_RELEASE_GATE_REPORT_DIR:-${REPO_ROOT}/target/fast-release-gates/$(git -C "${REPO_ROOT}" rev-parse HEAD)}"
dry_run=0
allow_dirty=0
pg_major=""

usage() {
  cat <<'USAGE'
Usage: scripts/run-fast-release-gate-report.sh [options]

Run fast release-candidate gates and write a TSV/Markdown report.

Options:
  --out-dir PATH   Report/log directory. Defaults under target/fast-release-gates.
  --pg-major N     PostgreSQL major for context-pg pgrx gates. Defaults to workspace metadata.
  --allow-dirty    Keep running for diagnostic reports from a dirty worktree.
  --dry-run        Write the report plan without executing commands.
  -h, --help       Show this help text.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out-dir)
      [[ $# -ge 2 ]] || {
        echo "--out-dir requires a value" >&2
        exit 2
      }
      out_dir="$2"
      shift 2
      ;;
    --pg-major)
      [[ $# -ge 2 ]] || {
        echo "--pg-major requires a value" >&2
        exit 2
      }
      pg_major="$2"
      shift 2
      ;;
    --allow-dirty)
      allow_dirty=1
      shift
      ;;
    --dry-run)
      dry_run=1
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

case "${out_dir}" in
  "" | "/")
    echo "--out-dir must be a non-root path" >&2
    exit 2
    ;;
esac

metadata_value() {
  local key="$1"
  sed -nE "s/^${key}[[:space:]]*=[[:space:]]*\"([^\"]+)\".*/\\1/p" "${REPO_ROOT}/Cargo.toml" | head -n 1
}

metadata_array_csv() {
  local key="$1"
  sed -nE "s/^${key}[[:space:]]*=[[:space:]]*\\[(.*)\\].*/\\1/p" "${REPO_ROOT}/Cargo.toml" \
    | head -n 1 \
    | sed -E 's/"//g; s/[[:space:]]*,[[:space:]]*/, /g'
}

repo_relative_path() {
  local path="$1"

  case "${path}" in
    "${REPO_ROOT}"/*) printf '%s\n' "${path#"${REPO_ROOT}/"}" ;;
    *) printf '%s\n' "${path}" ;;
  esac
}

pg_config_for_major() {
  local major="$1"
  local selected

  selected="$(cargo pgrx info pg-config "pg${major}" 2>/dev/null || true)"
  if [[ -n "${selected}" && -x "${selected}" ]]; then
    printf '%s\n' "${selected}"
    return 0
  fi

  command -v pg_config 2>/dev/null || true
}

if [[ -z "${pg_major}" ]]; then
  pg_major="$(metadata_value "primary-postgres-version")"
fi
supported_postgres="$(metadata_array_csv "supported-postgres-versions")"
if [[ -z "${pg_major}" || -z "${supported_postgres}" ]]; then
  echo "workspace metadata must define primary-postgres-version and supported-postgres-versions" >&2
  exit 1
fi
pg_major="${pg_major#pg}"
if [[ ! "${pg_major}" =~ ^[0-9]+$ ]]; then
  echo "--pg-major must be a numeric PostgreSQL major" >&2
  exit 2
fi
pg_supported=0
IFS=',' read -ra supported_postgres_parts <<<"${supported_postgres}"
for supported in "${supported_postgres_parts[@]}"; do
  supported="${supported//[[:space:]]/}"
  if [[ "${supported}" == "${pg_major}" ]]; then
    pg_supported=1
    break
  fi
done
if [[ "${pg_supported}" -ne 1 ]]; then
  echo "--pg-major must be one of supported-postgres-versions (${supported_postgres})" >&2
  exit 2
fi

worktree_state="unknown"
if git -C "${REPO_ROOT}" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  if [[ -z "$(git -C "${REPO_ROOT}" status --short)" ]]; then
    worktree_state="clean"
  else
    worktree_state="dirty"
  fi
fi
if [[ "${dry_run}" -eq 0 && "${allow_dirty}" -eq 0 && "${worktree_state}" == "dirty" ]]; then
  echo "dirty worktree cannot produce fast release-gate evidence; use --allow-dirty for diagnostic runs" >&2
  exit 1
fi

mkdir -p "${out_dir}"
summary_tsv="${out_dir}/summary.tsv"
report_md="${out_dir}/report.md"
git_sha="unknown"
if git -C "${REPO_ROOT}" rev-parse --verify HEAD >/dev/null 2>&1; then
  git_sha="$(git -C "${REPO_ROOT}" rev-parse --verify HEAD)"
fi
host_os="$(uname -srm)"
rustc_version="$(rustc -V 2>/dev/null || printf 'unavailable')"
cargo_version="$(cargo -V 2>/dev/null || printf 'unavailable')"
cargo_pgrx_version="$(cargo pgrx --version 2>/dev/null || printf 'unavailable')"
pg_config_path="$(pg_config_for_major "${pg_major}")"
pg_config_version="$("${pg_config_path}" --version 2>/dev/null || printf 'unavailable')"
pg_config_matches_major=0
if [[ "${pg_config_version}" == "PostgreSQL ${pg_major}"* ]]; then
  pg_config_matches_major=1
fi
started_all="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

{
  printf 'gate\tstatus\texit_code\tstarted_utc\tfinished_utc\tlog\tcommand\tlog_bytes\n'
} >"${summary_tsv}"

{
  printf '# Fast Release Gate Report\n\n'
  printf -- '- Commit: `%s`\n' "${git_sha}"
  printf -- '- Worktree: `%s`\n' "${worktree_state}"
  printf -- '- Host: `%s`\n' "${host_os}"
  printf -- '- Rust: `%s`\n' "${rustc_version}"
  printf -- '- Cargo: `%s`\n' "${cargo_version}"
  printf -- '- Cargo pgrx: `%s`\n' "${cargo_pgrx_version}"
  printf -- '- PostgreSQL major: `%s`\n' "${pg_major}"
  printf -- '- PostgreSQL feature: `pg%s`\n' "${pg_major}"
  printf -- '- PG config path: `%s`\n' "${pg_config_path:-unavailable}"
  printf -- '- PG config: `%s`\n' "${pg_config_version}"
  printf -- '- PG config matches major: `%s`\n' "${pg_config_matches_major}"
  printf -- '- Started: `%s`\n' "${started_all}"
  printf -- '- Dirty override: `%s`\n' "${allow_dirty}"
  if [[ "${dry_run}" -eq 1 ]]; then
    printf -- '- Execution: `dry-run`\n'
  else
    printf -- '- Execution: `run`\n'
  fi
  printf '\n| Gate | Status | Log |\n'
  printf '|---|---|---|\n'
} >"${report_md}"

append_result() {
  local gate="$1"
  local status="$2"
  local exit_code="$3"
  local started="$4"
  local finished="$5"
  local log_file="$6"
  local command="$7"
  local log_bytes=0
  local log_path

  if [[ -f "${log_file}" ]]; then
    log_bytes="$(wc -c <"${log_file}" | tr -d ' ')"
  fi
  log_path="$(repo_relative_path "${log_file}")"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${gate}" "${status}" "${exit_code}" "${started}" "${finished}" \
    "${log_path}" "${command}" "${log_bytes}" >>"${summary_tsv}"
  printf '| `%s` | `%s` | `%s` |\n' "${gate}" "${status}" "${log_path}" >>"${report_md}"
}

run_gate() {
  local gate="$1"
  local command="$2"
  local log_file="${out_dir}/${gate}.log"
  local started
  local finished
  local status="passed"
  local exit_code=0

  started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if [[ "${dry_run}" -eq 1 ]]; then
    status="dry-run"
    printf 'dry run: %s\n' "${command}" >"${log_file}"
  else
    (
      cd "${REPO_ROOT}"
      bash -c "${command}"
    ) >"${log_file}" 2>&1 || {
      exit_code=$?
      status="failed"
    }
  fi
  finished="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  append_result "${gate}" "${status}" "${exit_code}" "${started}" "${finished}" "${log_file}" "${command}"
}

run_gate "fmt" "cargo fmt --check"
run_gate "clippy-workspace" "cargo clippy --workspace --exclude context-pg --all-targets --all-features -- -D warnings"
run_gate "clippy-context-pg" "cargo clippy -p context-pg --all-targets --features pg${pg_major} -- -D warnings"
run_gate "workspace-tests" "cargo test --workspace --exclude context-pg --all-features"
run_gate "context-pg-check" "cargo check -p context-pg --features pg${pg_major}"
run_gate "context-pg-pgrx" "PG_MAJOR=${pg_major} scripts/run-v1-pgrx-tests.sh"
run_gate "docs" "cargo doc --workspace --no-deps"
run_gate "parity-matrix" "scripts/check-parity-matrix.sh"
run_gate "source-hygiene" "scripts/check-source-hygiene.sh"
run_gate "cargo-audit" "cargo audit --db target/cargo-audit-advisory-db"
run_gate "cargo-deny" "cargo deny check"

total_rows="$(tail -n +2 "${summary_tsv}" | wc -l | tr -d ' ')"
passed_rows="$(awk -F '\t' 'NR > 1 && $2 == "passed" { count++ } END { print count + 0 }' "${summary_tsv}")"
dry_run_rows="$(awk -F '\t' 'NR > 1 && $2 == "dry-run" { count++ } END { print count + 0 }' "${summary_tsv}")"
failed_rows="$(awk -F '\t' 'NR > 1 && $2 == "failed" { count++ } END { print count + 0 }' "${summary_tsv}")"
missing_logs=0
while IFS=$'\t' read -r gate _status _exit_code _started _finished log_path _command _log_bytes; do
  [[ "${gate}" == "gate" ]] && continue
  case "${log_path}" in
    /*) absolute_log="${log_path}" ;;
    *) absolute_log="${REPO_ROOT}/${log_path}" ;;
  esac
  if [[ ! -f "${absolute_log}" ]]; then
    missing_logs=$((missing_logs + 1))
  fi
done <"${summary_tsv}"

{
  printf '\n## Summary\n\n'
  printf -- '- Rows: `%s`\n' "${total_rows}"
  printf -- '- Passed: `%s`\n' "${passed_rows}"
  printf -- '- Dry-run: `%s`\n' "${dry_run_rows}"
  printf -- '- Failed: `%s`\n' "${failed_rows}"
  printf -- '- Missing logs: `%s`\n' "${missing_logs}"
  if [[ "${total_rows}" -gt 0 &&
        "${passed_rows}" -eq "${total_rows}" &&
        "${dry_run_rows}" -eq 0 &&
        "${failed_rows}" -eq 0 &&
        "${missing_logs}" -eq 0 &&
        "${worktree_state}" == "clean" &&
        "${allow_dirty}" -eq 0 &&
        "${pg_config_matches_major}" -eq 1 &&
        "${cargo_pgrx_version}" != "unavailable" ]]; then
    printf -- '- Approval: `complete`\n'
  else
    printf -- '- Approval: `incomplete`\n'
    printf -- '- Approval note: fast release-gate evidence requires every baseline gate to pass from a clean worktree with cargo-pgrx available, pg_config matching the selected PostgreSQL major, logs present, and no dry-run rows.\n'
  fi
  printf '\nSummary TSV: `%s`\n' "$(repo_relative_path "${summary_tsv}")"
} >>"${report_md}"

printf 'fast release-gate report: %s\n' "${report_md}"
if [[ "${failed_rows}" -gt 0 ]]; then
  exit 1
fi
