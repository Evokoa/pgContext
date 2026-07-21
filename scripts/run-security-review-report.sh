#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
PG_MAJOR="${PG_MAJOR:-17}"
PG_FEATURE="${PG_FEATURE:-pg${PG_MAJOR}}"
git_sha="$(git -C "${REPO_ROOT}" rev-parse --verify HEAD 2>/dev/null || printf 'unknown')"
out_dir="${SECURITY_REVIEW_REPORT_DIR:-${REPO_ROOT}/target/security-review/${git_sha}}"
dry_run=0

usage() {
  cat <<'USAGE'
Usage: scripts/run-security-review-report.sh [options]

Run release security-review gates and write auditable logs.

Options:
  --pg-major N       PostgreSQL major for pgrx/heavy gates. Defaults to 17.
  --out-dir PATH     Report/log directory. Defaults under target/security-review.
  --dry-run          Write the report plan without executing commands.
  -h, --help         Show this help text.
USAGE
}

repo_relative_path() {
  local path="$1"

  case "${path}" in
    "${REPO_ROOT}"/*) printf '%s\n' "${path#"${REPO_ROOT}/"}" ;;
    *) printf '%s\n' "${path}" ;;
  esac
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --pg-major)
      [[ $# -ge 2 ]] || {
        echo "--pg-major requires a value" >&2
        exit 2
      }
      PG_MAJOR="$2"
      PG_FEATURE="pg${PG_MAJOR}"
      shift 2
      ;;
    --out-dir)
      [[ $# -ge 2 ]] || {
        echo "--out-dir requires a value" >&2
        exit 2
      }
      out_dir="$2"
      shift 2
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

if ! [[ "${PG_MAJOR}" =~ ^[0-9]+$ ]]; then
  echo "--pg-major must be a PostgreSQL major number" >&2
  exit 2
fi

if [[ -z "${out_dir}" || "${out_dir}" == "/" ]]; then
  echo "--out-dir must be a non-root path" >&2
  exit 2
fi
if [[ "${out_dir}" != /* ]]; then
  out_dir="${REPO_ROOT}/${out_dir}"
fi

mkdir -p "${out_dir}"
summary_tsv="${out_dir}/summary.tsv"
report_md="${out_dir}/report.md"

worktree_state="unknown"
if git -C "${REPO_ROOT}" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  if [[ -z "$(git -C "${REPO_ROOT}" status --short)" ]]; then
    worktree_state="clean"
  else
    worktree_state="dirty"
  fi
fi

host_os="$(uname -srm)"
rustc_version="$(rustc -V 2>/dev/null || printf 'unavailable')"
cargo_version="$(cargo -V 2>/dev/null || printf 'unavailable')"
cargo_pgrx_version="$(cargo pgrx --version 2>/dev/null || printf 'unavailable')"
started_all="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

{
  printf 'gate\tkind\tstatus\texit_code\tstarted_utc\tfinished_utc\tlog\tboundary\tcommand\tselected_tests\tmin_selected_tests\tlog_bytes\n'
} >"${summary_tsv}"

{
  printf '# Security Review Gate Report\n\n'
  printf -- '- Commit: `%s`\n' "${git_sha}"
  printf -- '- Worktree: `%s`\n' "${worktree_state}"
  printf -- '- Host: `%s`\n' "${host_os}"
  printf -- '- Rust: `%s`\n' "${rustc_version}"
  printf -- '- Cargo: `%s`\n' "${cargo_version}"
  printf -- '- Cargo pgrx: `%s`\n' "${cargo_pgrx_version}"
  printf -- '- PostgreSQL major: `%s`\n' "${PG_MAJOR}"
  printf -- '- Started: `%s`\n' "${started_all}"
  if [[ "${dry_run}" -eq 1 ]]; then
    printf -- '- Mode: `dry-run`\n'
  else
    printf -- '- Mode: `execute`\n'
  fi
  printf '\n'
  printf '| Gate | Kind | Boundary | Status | Log |\n'
  printf '|---|---|---|---|---|\n'
} >"${report_md}"

append_result() {
  local gate="$1"
  local kind="$2"
  local status="$3"
  local exit_code="$4"
  local started="$5"
  local finished="$6"
  local log_file="$7"
  local boundary="$8"
  local command="$9"
  local selected_tests="${10}"
  local min_selected_tests="${11}"
  local log_bytes="${12}"
  local log_path

  log_path="$(repo_relative_path "${log_file}")"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${gate}" "${kind}" "${status}" "${exit_code}" "${started}" "${finished}" \
    "${log_path}" "${boundary}" "${command}" "${selected_tests}" "${min_selected_tests}" "${log_bytes}" >>"${summary_tsv}"
  printf '| `%s` | `%s` | %s | `%s` | `%s` |\n' \
    "${gate}" "${kind}" "${boundary}" "${status}" "${log_path}" >>"${report_md}"
}

overall_status=0
total_rows=0
passed_rows=0
dry_run_rows=0
failed_rows=0
run_gate() {
  local gate="$1"
  local kind="$2"
  local boundary="$3"
  local command="$4"
  local min_selected_tests="$5"
  local log_file="${out_dir}/${gate}.log"
  shift 5

  local started
  local finished
  local status="passed"
  local exit_code=0
  local selected_tests=0
  local log_bytes
  started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  if [[ "${dry_run}" -eq 1 ]]; then
    status="dry-run"
    {
      printf 'gate: %s\n' "${gate}"
      printf 'kind: %s\n' "${kind}"
      printf 'dry run: %s\n' "${command}"
      printf 'boundary: %s\n' "${boundary}"
      printf 'postgres major: %s\n' "${PG_MAJOR}"
      printf 'command: %s\n' "${command}"
      printf 'minimum selected tests: %s\n' "${min_selected_tests}"
    } >"${log_file}"
  else
    {
      printf 'gate: %s\n' "${gate}"
      printf 'kind: %s\n' "${kind}"
      printf 'boundary: %s\n' "${boundary}"
      printf 'postgres major: %s\n' "${PG_MAJOR}"
      printf 'command: %s\n' "${command}"
      printf 'minimum selected tests: %s\n\n' "${min_selected_tests}"
      cd "${REPO_ROOT}"
      "$@"
    } >"${log_file}" 2>&1 || {
      exit_code=$?
      status="failed"
      overall_status=1
    }
    if [[ "${status}" == "passed" && "${min_selected_tests}" -gt 0 ]]; then
      selected_tests="$(awk '/^running [0-9]+ tests?/ { total += $2 } END { print total + 0 }' "${log_file}")"
      if [[ "${selected_tests}" -lt "${min_selected_tests}" ]]; then
        {
          printf '\nexpected at least %s selected tests, saw %s\n' \
            "${min_selected_tests}" "${selected_tests}"
        } >>"${log_file}"
        exit_code=90
        status="failed"
        overall_status=1
      fi
    fi
  fi

  finished="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  {
    printf 'selected tests: %s\n' "${selected_tests}"
    printf 'security gate status: %s\n' "${status}"
    printf 'security gate exit code: %s\n' "${exit_code}"
  } >>"${log_file}"
  log_bytes="$(wc -c <"${log_file}" | tr -d ' ')"
  append_result "${gate}" "${kind}" "${status}" "${exit_code}" "${started}" "${finished}" "${log_file}" "${boundary}" "${command}" "${selected_tests}" "${min_selected_tests}" "${log_bytes}"
  total_rows=$((total_rows + 1))
  case "${status}" in
    passed) passed_rows=$((passed_rows + 1)) ;;
    dry-run) dry_run_rows=$((dry_run_rows + 1)) ;;
    failed) failed_rows=$((failed_rows + 1)) ;;
  esac
}

run_gate \
  "pgrx-search-path" \
  "pgrx" \
  "hostile search_path and shadow-catalog pg_tests" \
  "cargo pgrx test --release -p context-pg pg${PG_MAJOR} security_definer" \
  2 \
  cargo pgrx test --release -p context-pg "pg${PG_MAJOR}" security_definer

run_gate \
  "pgrx-telemetry-privacy" \
  "pgrx" \
  "telemetry privacy pg_test rejects vector, payload, filter, and query-text storage" \
  "cargo pgrx test --release -p context-pg pg${PG_MAJOR} telemetry_surfaces_do_not_store" \
  1 \
  cargo pgrx test --release -p context-pg "pg${PG_MAJOR}" telemetry_surfaces_do_not_store

run_gate \
  "pgrx-acl-denial" \
  "pgrx" \
  "source-table ACL and collection ownership denial pg_tests" \
  "cargo pgrx test --release -p context-pg pg${PG_MAJOR} denies" \
  10 \
  cargo pgrx test --release -p context-pg "pg${PG_MAJOR}" denies

run_gate \
  "pgrx-point-mutation-acl" \
  "pgrx" \
  "point mutation ACL denial pg_test" \
  "cargo pgrx test --release -p context-pg pg${PG_MAJOR} point_mutations_deny" \
  1 \
  cargo pgrx test --release -p context-pg "pg${PG_MAJOR}" point_mutations_deny

run_gate \
  "pgrx-rls-acl" \
  "pgrx" \
  "source-table RLS and split-owner ACL pg_tests" \
  "cargo pgrx test --release -p context-pg pg${PG_MAJOR} rls" \
  2 \
  cargo pgrx test --release -p context-pg "pg${PG_MAJOR}" rls

run_gate \
  "pgrx-sqlstate-contract" \
  "pgrx" \
  "SQLSTATE contract for documented bad paths" \
  "cargo pgrx test --release -p context-pg pg${PG_MAJOR} sqlstate_contract" \
  4 \
  cargo pgrx test --release -p context-pg "pg${PG_MAJOR}" sqlstate_contract

run_gate \
  "unsafe-comments" \
  "static" \
  "unsafe blocks must carry SAFETY comments" \
  "scripts/check-unsafe-safety-comments.sh" \
  0 \
  "${REPO_ROOT}/scripts/check-unsafe-safety-comments.sh"

run_gate \
  "heavy-rls-acl-boundary" \
  "heavy" \
  "RLS and ACL behavior across source-table boundaries" \
  "PG_VERSION=pg${PG_MAJOR} PG_FEATURE=${PG_FEATURE} tests/heavy/rls_acl_boundary.sh" \
  0 \
  env PG_VERSION="pg${PG_MAJOR}" PG_FEATURE="${PG_FEATURE}" "${REPO_ROOT}/tests/heavy/rls_acl_boundary.sh"

run_gate \
  "heavy-sqlstate-contract" \
  "heavy" \
  "heavy-wrapper SQLSTATE contract using the configured PostgreSQL major" \
  "PG_VERSION=pg${PG_MAJOR} PG_FEATURE=${PG_FEATURE} tests/heavy/sqlstate_contract.sh" \
  4 \
  env PG_VERSION="pg${PG_MAJOR}" PG_FEATURE="${PG_FEATURE}" "${REPO_ROOT}/tests/heavy/sqlstate_contract.sh"

{
  approval="complete"
  if [[ "${dry_run_rows}" -gt 0 || "${failed_rows}" -gt 0 || "${total_rows}" -eq 0 ]]; then
    approval="incomplete"
  fi
  printf '\n## Summary\n\n'
  printf -- '- Rows: `%s`\n' "${total_rows}"
  printf -- '- Passed: `%s`\n' "${passed_rows}"
  printf -- '- Dry-run: `%s`\n' "${dry_run_rows}"
  printf -- '- Failed: `%s`\n' "${failed_rows}"
  printf -- '- Approval: `%s`\n' "${approval}"
  printf '\n## Summary TSV\n\n'
  printf 'See `%s`.\n' "$(repo_relative_path "${summary_tsv}")"
} >>"${report_md}"

printf 'security review report: %s\n' "${report_md}"
exit "${overall_status}"
