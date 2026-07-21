#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
out_dir="${PLATFORM_BUILD_REPORT_DIR:-${REPO_ROOT}/target/platform-builds/$(git -C "${REPO_ROOT}" rev-parse HEAD)}"
dry_run=0
allow_dirty=0
linux_container=0
pg_major="${PG_MAJOR:-17}"
platforms=()
merge_reports=()
explicit_platforms=0

usage() {
  cat <<'USAGE'
Usage: scripts/run-platform-build-report.sh [options]

Run or plan release platform build gates and write a TSV/Markdown report.

Options:
  --platform NAME      macos or linux. May be repeated. Defaults to host.
  --linux-container   Run the Linux row through release-linux-container-gates.sh.
  --pg-major N        PostgreSQL major for the Linux container gate. Defaults to 17.
  --out-dir PATH      Report/log directory. Defaults under target/platform-builds.
  --merge-report PATH Merge an existing platform report.md. May be repeated.
                       Use this to combine real macOS and Linux CI artifacts.
  --allow-dirty       Keep running for diagnostic reports from a dirty worktree.
  --dry-run           Write the report plan without executing commands.
  -h, --help          Show this help text.
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
    --platform)
      [[ $# -ge 2 ]] || {
        echo "--platform requires a value" >&2
        exit 2
      }
      platforms+=("$2")
      explicit_platforms=1
      shift 2
      ;;
    --linux-container)
      linux_container=1
      shift
      ;;
    --pg-major)
      [[ $# -ge 2 ]] || {
        echo "--pg-major requires a value" >&2
        exit 2
      }
      pg_major="$2"
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
    --merge-report)
      [[ $# -ge 2 ]] || {
        echo "--merge-report requires a value" >&2
        exit 2
      }
      merge_reports+=("$2")
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
case "${pg_major}" in
  15 | 16 | 17 | 18) ;;
  *)
    echo "--pg-major must be one of: 15, 16, 17, 18" >&2
    exit 2
    ;;
esac

host_platform() {
  if [[ -n "${PLATFORM_HOST_OVERRIDE:-}" ]]; then
    printf '%s\n' "${PLATFORM_HOST_OVERRIDE}"
    return 0
  fi
  case "$(uname -s)" in
    Darwin) printf 'macos\n' ;;
    Linux) printf 'linux\n' ;;
    *) printf 'unknown\n' ;;
  esac
}

host_platform="$(host_platform)"
if [[ "${#platforms[@]}" -eq 0 ]]; then
  platforms=("${host_platform}")
fi

if [[ ${#merge_reports[@]} -gt 0 && "${explicit_platforms}" -eq 1 ]]; then
  echo "--merge-report cannot be combined with explicit --platform rows" >&2
  exit 2
fi

if [[ ${#merge_reports[@]} -eq 0 ]]; then
  for ((i = 0; i < ${#platforms[@]}; i++)); do
    case "${platforms[$i]}" in
      macos | linux) ;;
      *)
        echo "unsupported platform: ${platforms[$i]}" >&2
        exit 2
        ;;
    esac
    for ((j = i + 1; j < ${#platforms[@]}; j++)); do
      if [[ "${platforms[$i]}" == "${platforms[$j]}" ]]; then
        echo "duplicate platform: ${platforms[$i]}" >&2
        exit 2
      fi
    done
  done
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
  echo "dirty worktree cannot produce release platform evidence; use --allow-dirty for diagnostic runs" >&2
  exit 1
fi

mkdir -p "${out_dir}"
summary_tsv="${out_dir}/summary.tsv"
report_md="${out_dir}/report.md"
git_sha="unknown"
if git -C "${REPO_ROOT}" rev-parse --verify HEAD >/dev/null 2>&1; then
  git_sha="$(git -C "${REPO_ROOT}" rev-parse --verify HEAD)"
fi
host_os="${PLATFORM_HOST_OS_OVERRIDE:-$(uname -srm)}"
rustc_version="$(rustc -V 2>/dev/null || printf 'unavailable')"
cargo_version="$(cargo -V 2>/dev/null || printf 'unavailable')"
started_all="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

{
  printf 'platform\tgate\tstatus\texit_code\tstarted_utc\tfinished_utc\thost\tlog\tcommand\tlog_bytes\n'
} >"${summary_tsv}"

{
  printf '# Platform Build Report\n\n'
  printf -- '- Commit: `%s`\n' "${git_sha}"
  printf -- '- Worktree: `%s`\n' "${worktree_state}"
  printf -- '- Host: `%s`\n' "${host_os}"
  printf -- '- Host platform: `%s`\n' "${host_platform}"
  printf -- '- Rust: `%s`\n' "${rustc_version}"
  printf -- '- Cargo: `%s`\n' "${cargo_version}"
  printf -- '- Started: `%s`\n' "${started_all}"
  printf -- '- Linux container diagnostic: `%s`\n' "${linux_container}"
  printf -- '- Linux container PostgreSQL: `pg%s`\n' "${pg_major}"
  printf -- '- Dirty override: `%s`\n' "${allow_dirty}"
  if [[ "${dry_run}" -eq 1 ]]; then
    printf -- '- Execution: `dry-run`\n'
  else
    printf -- '- Execution: `run`\n'
  fi
  if [[ ${#merge_reports[@]} -gt 0 ]]; then
    printf -- '- Merged reports: `%s`\n' "${#merge_reports[@]}"
  fi
  printf '\n| Platform | Gate | Status | Host | Log |\n'
  printf '|---|---|---|---|---|\n'
} >"${report_md}"

platform_gate_names=(
  fmt
  clippy-workspace
  workspace-tests
  docs
  parity-matrix
  parity-matrix-smoke
  benchmark-report-smoke
  release-artifact-report-smoke
  security-review-report-smoke
  postgres-matrix-report-smoke
  upgrade-matrix-staging-smoke
  source-hygiene
)
platform_gate_commands=(
  "cargo fmt --check"
  "cargo clippy --workspace --exclude context-pg --all-targets --all-features -- -D warnings"
  "cargo test --workspace --exclude context-pg --all-features"
  "cargo doc --workspace --exclude context-pg --no-deps"
  "scripts/check-parity-matrix.sh"
  "tests/shell/check_parity_matrix_smoke.sh"
  "tests/shell/run_benchmark_report_smoke.sh"
  "tests/shell/run_release_artifact_report_smoke.sh"
  "tests/shell/run_security_review_report_smoke.sh"
  "tests/shell/run_postgres_matrix_gates_smoke.sh"
  "tests/shell/upgrade_matrix_staging_smoke.sh"
  "scripts/check-source-hygiene.sh"
)

run_command_text() {
  local command="$1"
  (
    cd "${REPO_ROOT}"
    bash -c "${command}"
  )
}

append_result() {
  local platform="$1"
  local gate="$2"
  local status="$3"
  local exit_code="$4"
  local started="$5"
  local finished="$6"
  local host="$7"
  local log_file="$8"
  local command="$9"
  local log_bytes="${10}"
  local log_path

  log_path="$(repo_relative_path "${log_file}")"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${platform}" "${gate}" "${status}" "${exit_code}" "${started}" "${finished}" \
    "${host}" "${log_path}" "${command}" "${log_bytes}" >>"${summary_tsv}"
  printf '| `%s` | `%s` | `%s` | `%s` | `%s` |\n' \
    "${platform}" "${gate}" "${status}" "${host}" "${log_path}" >>"${report_md}"
}

summary_tsv_path_for_report() {
  local report="$1"
  local summary

  summary="$(sed -nE 's/^Summary TSV: `([^`]+)`$/\1/p' "${report}" | head -n 1)"
  if [[ -z "${summary}" ]]; then
    echo "merged platform report is missing Summary TSV reference: ${report}" >&2
    exit 2
  fi
  case "${summary}" in
    /* | ../* | */../* | . | .. | */..)
      echo "merged platform report summary TSV must be repo-relative: ${report}" >&2
      exit 2
      ;;
    *) printf '%s/%s\n' "${REPO_ROOT}" "${summary}" ;;
  esac
}

summary_log_path() {
  local log_path="$1"

  case "${log_path}" in
    /* | ../* | */../* | . | .. | */..)
      echo "merged platform report summary TSV has unsafe log path: ${log_path}" >&2
      exit 2
      ;;
    *) printf '%s/%s\n' "${REPO_ROOT}" "${log_path}" ;;
  esac
}

validate_merged_platform_logs() {
  local summary="$1"
  local platform
  local gate
  local _status
  local _exit_code
  local _started
  local _finished
  local _host
  local log_path
  local _command
  local expected_bytes
  local absolute_log
  local actual_bytes

  while IFS=$'\t' read -r platform gate _status _exit_code _started _finished _host log_path _command expected_bytes; do
    [[ -n "${platform}" ]] || continue
    absolute_log="$(summary_log_path "${log_path}")"
    if [[ -L "${absolute_log}" ]]; then
      echo "merged platform report log must not be a symlink for ${platform} ${gate}: ${log_path}" >&2
      exit 2
    fi
    if [[ ! -f "${absolute_log}" ]]; then
      echo "merged platform report log is missing for ${platform} ${gate}: ${log_path}" >&2
      exit 2
    fi
    actual_bytes="$(wc -c <"${absolute_log}" | tr -d ' ')"
    if [[ "${actual_bytes}" != "${expected_bytes}" ]]; then
      echo "merged platform report log-byte evidence does not match ${platform} ${gate}: ${log_path}" >&2
      exit 2
    fi
  done < <(tail -n +2 "${summary}")
}

merge_platform_reports() {
  local report
  local summary
  local title
  local rows

  for report in "${merge_reports[@]}"; do
    if [[ "${report}" != /* ]]; then
      report="${REPO_ROOT}/${report}"
    fi
    if [[ ! -f "${report}" ]]; then
      echo "merged platform report is missing: ${report}" >&2
      exit 2
    fi
    title="$(sed -n '1p' "${report}")"
    if [[ "${title}" != "# Platform Build Report" ]]; then
      echo "merged platform report has wrong title: ${report}" >&2
      exit 2
    fi
    summary="$(summary_tsv_path_for_report "${report}")"
    if [[ ! -f "${summary}" ]]; then
      echo "merged platform report summary TSV is missing: ${report}" >&2
      exit 2
    fi
    if [[ -L "${summary}" ]]; then
      echo "merged platform report summary TSV must not be a symlink: ${summary}" >&2
      exit 2
    fi
    if [[ "$(sed -n '1p' "${summary}")" != $'platform\tgate\tstatus\texit_code\tstarted_utc\tfinished_utc\thost\tlog\tcommand\tlog_bytes' ]]; then
      echo "merged platform report summary TSV has wrong header: ${summary}" >&2
      exit 2
    fi
    rows="$(tail -n +2 "${summary}" | wc -l | tr -d ' ')"
    if [[ "${rows}" == "0" ]]; then
      echo "merged platform report summary TSV has no rows: ${summary}" >&2
      exit 2
    fi
    if ! awk -F '\t' 'NR > 1 && $10 !~ /^[1-9][0-9]*$/ { bad = 1 } END { exit(bad ? 1 : 0) }' "${summary}"; then
      echo "merged platform report summary TSV has invalid log-byte evidence: ${summary}" >&2
      exit 2
    fi
    validate_merged_platform_logs "${summary}"
    tail -n +2 "${summary}" >>"${summary_tsv}"
  done

  if awk -F '\t' 'NR > 1 { key = $1 "\t" $2; seen[key]++ } END { for (key in seen) if (seen[key] > 1) exit 1 }' "${summary_tsv}"; then
    :
  else
    echo "merged platform reports contain duplicate platform/gate rows" >&2
    exit 2
  fi
  if [[ "$(tail -n +2 "${summary_tsv}" | wc -l | tr -d ' ')" != "$((2 * ${#platform_gate_names[@]}))" ]] ||
    ! has_expected_platform_scope; then
    echo "merged platform reports do not contain the expected platform gate command set" >&2
    exit 2
  fi

  while IFS=$'\t' read -r platform gate status _exit_code _started _finished host log_file _command _log_bytes; do
    [[ -n "${platform}" ]] || continue
    printf '| `%s` | `%s` | `%s` | `%s` | `%s` |\n' \
      "${platform}" "${gate}" "${status}" "${host}" "${log_file}" >>"${report_md}"
  done < <(tail -n +2 "${summary_tsv}")
}

has_expected_platform_scope() {
  local platform
  local gate_index
  local gate
  local command

  for platform in linux macos; do
    for ((gate_index = 0; gate_index < ${#platform_gate_names[@]}; gate_index++)); do
      gate="${platform_gate_names[$gate_index]}"
      command="${platform_gate_commands[$gate_index]}"
      if ! awk -F '\t' -v platform="${platform}" -v gate="${gate}" -v command="${command}" '
        $1 == platform && $2 == gate && $9 == command { found = 1 }
        END { exit(found ? 0 : 1) }
      ' "${summary_tsv}"; then
        return 1
      fi
    done
  done
}

run_platform_gate() {
  local platform="$1"
  local gate="$2"
  local command="$3"
  local log_file="${out_dir}/${platform}-${gate}.log"
  local started
  local finished
  local log_bytes
  local status="passed"
  local exit_code=0

  started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if [[ "${dry_run}" -eq 1 ]]; then
    status="dry-run"
    {
      printf 'dry run platform: %s\n' "${platform}"
      printf 'gate: %s\n' "${gate}"
      printf 'host platform: %s\n' "${host_platform}"
      printf 'command: %s\n' "${command}"
    } >"${log_file}"
  elif [[ "${platform}" != "${host_platform}" && "${PLATFORM_ALLOW_CROSS_EXECUTION:-0}" != "1" ]]; then
    status="skipped"
    {
      printf 'cannot run %s local fast gates on host platform %s\n' "${platform}" "${host_platform}"
      printf 'use the matching host to produce release platform evidence\n'
    } >"${log_file}"
  else
    {
      printf 'platform: %s\n' "${platform}"
      printf 'gate: %s\n' "${gate}"
      printf 'host platform: %s\n' "${host_platform}"
      printf 'command: %s\n\n' "${command}"
      run_command_text "${command}"
    } >"${log_file}" 2>&1 || {
      exit_code=$?
      status="failed"
    }
  fi
  finished="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  log_bytes="$(wc -c <"${log_file}" | tr -d ' ')"
  append_result "${platform}" "${gate}" "${status}" "${exit_code}" "${started}" "${finished}" "${host_os}" "${log_file}" "${command}" "${log_bytes}"
}

run_linux_container_diagnostic() {
  local log_file="${out_dir}/linux-linux-container.log"
  local started
  local finished
  local log_bytes
  local status="passed"
  local exit_code=0
  local command="PG_MAJOR=${pg_major} scripts/release-linux-container-gates.sh"

  started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if [[ "${dry_run}" -eq 1 ]]; then
    status="dry-run"
    printf 'dry run: %s\n' "${command}" >"${log_file}"
  else
    {
      printf 'diagnostic Linux container gate\n'
      printf 'command: %s\n\n' "${command}"
      run_command_text "${command}"
    } >"${log_file}" 2>&1 || {
      exit_code=$?
      status="failed"
    }
  fi
  finished="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  log_bytes="$(wc -c <"${log_file}" | tr -d ' ')"
  append_result "linux" "linux-container-diagnostic" "${status}" "${exit_code}" "${started}" "${finished}" "${host_os}" "${log_file}" "${command}" "${log_bytes}"
}

if [[ ${#merge_reports[@]} -gt 0 ]]; then
  merge_platform_reports
else
  for platform in "${platforms[@]}"; do
    for ((gate_index = 0; gate_index < ${#platform_gate_names[@]}; gate_index++)); do
      run_platform_gate "${platform}" "${platform_gate_names[$gate_index]}" "${platform_gate_commands[$gate_index]}"
    done
    if [[ "${platform}" == "linux" && "${linux_container}" -eq 1 ]]; then
      run_linux_container_diagnostic
    fi
  done
fi

total_rows="$(tail -n +2 "${summary_tsv}" | wc -l | tr -d ' ')"
passed_rows="$(awk -F '\t' 'NR > 1 && $3 == "passed" { count++ } END { print count + 0 }' "${summary_tsv}")"
dry_run_rows="$(awk -F '\t' 'NR > 1 && $3 == "dry-run" { count++ } END { print count + 0 }' "${summary_tsv}")"
skipped_rows="$(awk -F '\t' 'NR > 1 && $3 == "skipped" { count++ } END { print count + 0 }' "${summary_tsv}")"
failed_rows="$(awk -F '\t' 'NR > 1 && $3 == "failed" { count++ } END { print count + 0 }' "${summary_tsv}")"
full_release_scope=0
expected_rows="$((2 * ${#platform_gate_names[@]}))"
if [[ "${total_rows}" == "${expected_rows}" ]] &&
  awk -F '\t' 'NR > 1 { seen[$1] = 1 } END { exit(seen["macos"] && seen["linux"] ? 0 : 1) }' "${summary_tsv}" &&
  has_expected_platform_scope; then
  full_release_scope=1
fi

{
  printf '\n## Summary\n\n'
  printf -- '- Rows: `%s`\n' "${total_rows}"
  printf -- '- Passed: `%s`\n' "${passed_rows}"
  printf -- '- Dry-run: `%s`\n' "${dry_run_rows}"
  printf -- '- Skipped: `%s`\n' "${skipped_rows}"
  printf -- '- Failed: `%s`\n' "${failed_rows}"
  printf -- '- Missing platforms: `%s`\n' "$(
    awk -F '\t' 'NR > 1 { seen[$1] = 1 } END {
      missing = ""
      if (!seen["linux"]) missing = missing "linux,"
      if (!seen["macos"]) missing = missing "macos,"
      sub(/,$/, "", missing)
      if (missing == "") {
        print "none"
      } else {
        print missing
      }
    }' "${summary_tsv}"
  )"
  printf -- '- Full release scope: `%s`\n' "${full_release_scope}"
  if [[ "${full_release_scope}" -ne 1 ||
        "${passed_rows}" -ne "${total_rows}" ||
        "${dry_run_rows}" -gt 0 ||
        "${skipped_rows}" -gt 0 ||
        "${failed_rows}" -gt 0 ||
        "${worktree_state}" != "clean" ||
        "${allow_dirty}" -ne 0 ]]; then
    printf -- '- Approval: `incomplete`\n'
    printf -- '- Approval note: platform release evidence requires macOS and Linux rows, no dry-run/skipped/failed rows, and a clean worktree.\n'
  else
    printf -- '- Approval: `complete`\n'
  fi
  printf '\nSummary TSV: `%s`\n' "$(repo_relative_path "${summary_tsv}")"
} >>"${report_md}"

printf 'platform build report: %s\n' "${report_md}"
if [[ "${failed_rows}" -gt 0 ]]; then
  exit 1
fi
