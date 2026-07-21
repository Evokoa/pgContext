#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
out_dir="${SUPPLY_CHAIN_REPORT_DIR:-${REPO_ROOT}/target/supply-chain/$(git -C "${REPO_ROOT}" rev-parse HEAD)}"
dry_run=0
allow_dirty=0

usage() {
  cat <<'USAGE'
Usage: scripts/run-supply-chain-report.sh [options]

Run supply-chain release gates and write a TSV/Markdown report.

Options:
  --out-dir PATH   Report/log directory. Defaults under target/supply-chain.
  --allow-dirty    Keep running for diagnostic reports from a dirty worktree.
  --dry-run        Write the report plan without executing cargo commands.
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

repo_relative_path() {
  local path="$1"

  case "${path}" in
    "${REPO_ROOT}"/*) printf '%s\n' "${path#"${REPO_ROOT}/"}" ;;
    *) printf '%s\n' "${path}" ;;
  esac
}

worktree_state="unknown"
if git -C "${REPO_ROOT}" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  if [[ -z "$(git -C "${REPO_ROOT}" status --short)" ]]; then
    worktree_state="clean"
  else
    worktree_state="dirty"
  fi
fi
if [[ "${dry_run}" -eq 0 && "${allow_dirty}" -eq 0 && "${worktree_state}" == "dirty" ]]; then
  echo "dirty worktree cannot produce release supply-chain evidence; use --allow-dirty for diagnostic runs" >&2
  exit 1
fi

mkdir -p "${out_dir}"
repo_physical="$(cd -P "${REPO_ROOT}" && pwd -P)"
out_dir_physical="$(cd -P "${out_dir}" && pwd -P)"
out_dir_repo_contained=0
case "${out_dir_physical}" in
  "${repo_physical}" | "${repo_physical}"/*) out_dir_repo_contained=1 ;;
esac
summary_tsv="${out_dir}/summary.tsv"
report_md="${out_dir}/report.md"
git_sha="unknown"
if git -C "${REPO_ROOT}" rev-parse --verify HEAD >/dev/null 2>&1; then
  git_sha="$(git -C "${REPO_ROOT}" rev-parse --verify HEAD)"
fi
host_os="$(uname -srm)"
rustc_version="$(rustc -V 2>/dev/null || printf 'unavailable')"
cargo_version="$(cargo -V 2>/dev/null || printf 'unavailable')"
started_all="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
waiver_note="none"
if [[ "${allow_dirty}" -eq 1 || "${dry_run}" -eq 1 ]]; then
  waiver_note="diagnostic-only dirty/dry-run override; approval remains incomplete"
fi

{
  printf 'gate\tstatus\texit_code\tstarted_utc\tfinished_utc\tlog\tcommand\tlog_bytes\n'
} >"${summary_tsv}"

{
  printf '# Supply Chain Report\n\n'
  printf -- '- Commit: `%s`\n' "${git_sha}"
  printf -- '- Worktree: `%s`\n' "${worktree_state}"
  printf -- '- Host: `%s`\n' "${host_os}"
  printf -- '- Rust: `%s`\n' "${rustc_version}"
  printf -- '- Cargo: `%s`\n' "${cargo_version}"
  printf -- '- PostgreSQL: `not applicable (dependency metadata only)`\n'
  printf -- '- Started: `%s`\n' "${started_all}"
  printf -- '- Invocation: `scripts/run-supply-chain-report.sh --out-dir %s`\n' "$(repo_relative_path "${out_dir}")"
  printf -- '- Waiver: `%s`\n' "${waiver_note}"
  if [[ "${out_dir_repo_contained}" -eq 1 ]]; then
    printf -- '- Evidence directory: `repo`\n'
  else
    printf -- '- Evidence directory: `external`\n'
  fi
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
  local log_bytes="$8"
  local log_path

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
  local raw_log_file="${out_dir}/${gate}.raw.log"
  local started
  local finished
  local status="passed"
  local exit_code=0
  local log_bytes=0
  local raw_log_bytes=0

  started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if [[ "${dry_run}" -eq 1 ]]; then
    status="dry-run"
    printf 'dry run: %s\n' "${command}" >"${log_file}"
  else
    (
      cd "${REPO_ROOT}"
      bash -c "${command}"
    ) >"${raw_log_file}" 2>&1 || {
      exit_code=$?
      status="failed"
    }
    raw_log_bytes="$(awk '{ bytes += length($0) + 1 } END { print bytes + 0 }' "${raw_log_file}")"
    if [[ "${status}" == "passed" && "${raw_log_bytes}" -le 0 ]]; then
      printf 'expected non-empty supply-chain evidence output for %s\n' "${gate}" >"${raw_log_file}"
      raw_log_bytes="$(awk '{ bytes += length($0) + 1 } END { print bytes + 0 }' "${raw_log_file}")"
      exit_code=91
      status="failed"
    fi
    {
      printf 'supply-chain gate: %s\n' "${gate}"
      printf 'command: %s\n' "${command}"
      printf 'supply-chain gate status: %s\n' "${status}"
      printf 'supply-chain gate exit code: %s\n' "${exit_code}"
      printf 'supply-chain raw output bytes: %s\n' "${raw_log_bytes}"
      printf 'supply-chain command output begin\n'
      awk '{ print }' "${raw_log_file}"
      printf 'supply-chain command output end\n'
    } >"${log_file}"
    rm -f "${raw_log_file}"
    log_bytes="$(wc -c <"${log_file}" | tr -d ' ')"
  fi
  finished="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  append_result "${gate}" "${status}" "${exit_code}" "${started}" "${finished}" "${log_file}" "${command}" "${log_bytes}"
}

run_gate "cargo-audit" "cargo audit --db target/cargo-audit-advisory-db"
run_gate "cargo-deny" "cargo deny check"

total_rows="$(tail -n +2 "${summary_tsv}" | wc -l | tr -d ' ')"
passed_rows="$(awk -F '\t' 'NR > 1 && $2 == "passed" { count++ } END { print count + 0 }' "${summary_tsv}")"
dry_run_rows="$(awk -F '\t' 'NR > 1 && $2 == "dry-run" { count++ } END { print count + 0 }' "${summary_tsv}")"
failed_rows="$(awk -F '\t' 'NR > 1 && $2 == "failed" { count++ } END { print count + 0 }' "${summary_tsv}")"

{
  printf '\n## Summary\n\n'
  printf -- '- Rows: `%s`\n' "${total_rows}"
  printf -- '- Passed: `%s`\n' "${passed_rows}"
  printf -- '- Dry-run: `%s`\n' "${dry_run_rows}"
  printf -- '- Failed: `%s`\n' "${failed_rows}"
  if [[ "${total_rows}" -eq 2 &&
        "${passed_rows}" -eq 2 &&
        "${dry_run_rows}" -eq 0 &&
        "${failed_rows}" -eq 0 &&
        "${worktree_state}" == "clean" &&
        "${out_dir_repo_contained}" -eq 1 &&
        "${allow_dirty}" -eq 0 ]]; then
    printf -- '- Approval: `complete`\n'
  else
    printf -- '- Approval: `incomplete`\n'
    printf -- '- Approval note: supply-chain release evidence requires cargo audit and cargo deny check to pass from a clean worktree with repo-contained evidence paths and no dry-run rows.\n'
  fi
  printf '\nSummary TSV: `%s`\n' "$(repo_relative_path "${summary_tsv}")"
} >>"${report_md}"

printf -- '\n- Finished: `%s`\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"${report_md}"

printf 'supply-chain report: %s\n' "${report_md}"
if [[ "${failed_rows}" -gt 0 ]]; then
  exit 1
fi
