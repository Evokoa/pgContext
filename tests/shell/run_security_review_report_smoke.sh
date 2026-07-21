#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TMPDIR="${TMPDIR:-${REPO_ROOT}/target/tmp}"
mkdir -p "${TMPDIR}"
work_dir="$(mktemp -d "${TMPDIR}/security-review-test.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT

assert_file_exists() {
  local path="$1"
  if [[ ! -f "${path}" ]]; then
    echo "expected file to exist: ${path}" >&2
    exit 1
  fi
}

assert_summary_row_count() {
  local summary_path="$1"
  local expected_rows="$2"
  local actual_rows

  actual_rows="$(tail -n +2 "${summary_path}" | wc -l | tr -d ' ')"
  if [[ "${actual_rows}" != "${expected_rows}" ]]; then
    echo "expected ${expected_rows} summary rows in ${summary_path}, got ${actual_rows}" >&2
    exit 1
  fi
}

"${REPO_ROOT}/scripts/run-security-review-report.sh" \
  --dry-run \
  --pg-major 17 \
  --out-dir "${work_dir}/report"

summary="${work_dir}/report/summary.tsv"
report="${work_dir}/report/report.md"

assert_file_exists "${summary}"
assert_file_exists "${report}"
assert_summary_row_count "${summary}" "9"
head -n 1 "${summary}" | grep -q $'gate\tkind\tstatus\texit_code\tstarted_utc\tfinished_utc\tlog\tboundary\tcommand\tselected_tests\tmin_selected_tests\tlog_bytes'
grep -q -- '- Worktree: `' "${report}"
grep -q -- '- PostgreSQL major: `17`' "${report}"
grep -q -- '- Rows: `9`' "${report}"
grep -q -- '- Dry-run: `9`' "${report}"
grep -q -- '- Failed: `0`' "${report}"
grep -q -- '- Approval: `incomplete`' "${report}"

assert_gate() {
  local gate="$1"
  local kind="$2"
  local command="$3"
  local boundary="$4"
  local min_tests="$5"
  local log_file="${work_dir}/report/${gate}.log"
  local row_prefix

  row_prefix="${gate}"$'\t'"${kind}"$'\t''dry-run'$'\t''0'
  grep -qF "${row_prefix}" "${summary}"
  grep -qF "${command}" "${summary}"
  grep -qF "${command}"$'\t''0'$'\t'"${min_tests}"$'\t' "${summary}"
  grep -qF "| \`${gate}\` | \`${kind}\` | ${boundary} | \`dry-run\` | \`${log_file}\` |" "${report}"
  awk -F '\t' -v gate="${gate}" '$1 == gate && $12 ~ /^[1-9][0-9]*$/ { found = 1 } END { exit(found ? 0 : 1) }' \
    "${summary}"
  assert_file_exists "${log_file}"
  grep -qF "dry run: ${command}" "${log_file}"
  grep -qF "boundary: ${boundary}" "${log_file}"
  grep -qF "postgres major: 17" "${log_file}"
  grep -qF "minimum selected tests: ${min_tests}" "${log_file}"
  grep -qF "selected tests: 0" "${log_file}"
  grep -qF "security gate status: dry-run" "${log_file}"
  grep -qF "security gate exit code: 0" "${log_file}"
}

assert_gate \
  "pgrx-search-path" \
  "pgrx" \
  "cargo pgrx test --release -p context-pg pg17 security_definer" \
  "hostile search_path and shadow-catalog pg_tests" \
  2
assert_gate \
  "pgrx-telemetry-privacy" \
  "pgrx" \
  "cargo pgrx test --release -p context-pg pg17 telemetry_surfaces_do_not_store" \
  "telemetry privacy pg_test rejects vector, payload, filter, and query-text storage" \
  1
assert_gate \
  "pgrx-acl-denial" \
  "pgrx" \
  "cargo pgrx test --release -p context-pg pg17 denies" \
  "source-table ACL and collection ownership denial pg_tests" \
  10
assert_gate \
  "pgrx-point-mutation-acl" \
  "pgrx" \
  "cargo pgrx test --release -p context-pg pg17 point_mutations_deny" \
  "point mutation ACL denial pg_test" \
  1
assert_gate \
  "pgrx-rls-acl" \
  "pgrx" \
  "cargo pgrx test --release -p context-pg pg17 rls" \
  "source-table RLS and split-owner ACL pg_tests" \
  2
assert_gate \
  "pgrx-sqlstate-contract" \
  "pgrx" \
  "cargo pgrx test --release -p context-pg pg17 sqlstate_contract" \
  "SQLSTATE contract for documented bad paths" \
  4
assert_gate \
  "unsafe-comments" \
  "static" \
  "scripts/check-unsafe-safety-comments.sh" \
  "unsafe blocks must carry SAFETY comments" \
  0
assert_gate \
  "heavy-rls-acl-boundary" \
  "heavy" \
  "PG_VERSION=pg17 PG_FEATURE=pg17 tests/heavy/rls_acl_boundary.sh" \
  "RLS and ACL behavior across source-table boundaries" \
  0
assert_gate \
  "heavy-sqlstate-contract" \
  "heavy" \
  "PG_VERSION=pg17 PG_FEATURE=pg17 tests/heavy/sqlstate_contract.sh" \
  "heavy-wrapper SQLSTATE contract using the configured PostgreSQL major" \
  4

grep -q 'hostile search_path' "${report}"
grep -q 'telemetry privacy' "${report}"
grep -q 'source-table ACL' "${report}"
grep -q 'point mutation ACL' "${report}"
grep -q 'split-owner ACL' "${report}"
grep -q 'SAFETY comments' "${report}"

repo_local_out="${work_dir}/fixture/target/security-local"
mkdir -p "${work_dir}/fixture"
cp -R "${REPO_ROOT}/scripts" "${work_dir}/fixture/scripts"
REPO_ROOT="${work_dir}/fixture" \
  "${REPO_ROOT}/scripts/run-security-review-report.sh" \
    --dry-run \
    --pg-major 17 \
    --out-dir "${repo_local_out}"
grep -q 'See `target/security-local/summary.tsv`.' \
  "${repo_local_out}/report.md"
awk -F '\t' '$1 == "pgrx-search-path" && $7 == "target/security-local/pgrx-search-path.log" && $12 ~ /^[1-9][0-9]*$/ { found = 1 } END { exit(found ? 0 : 1) }' \
  "${repo_local_out}/summary.tsv"

assert_fails() {
  local label="$1"
  local expected="$2"
  shift 2
  if "${REPO_ROOT}/scripts/run-security-review-report.sh" "$@" 2>"${work_dir}/${label}.err"; then
    echo "${label} should fail" >&2
    exit 1
  fi
  grep -q -- "${expected}" "${work_dir}/${label}.err"
}

assert_fails invalid-pg '--pg-major must be a PostgreSQL major number' \
  --dry-run --pg-major invalid --out-dir "${work_dir}/bad"
assert_fails missing-pg-value '--pg-major requires a value' \
  --dry-run --pg-major
assert_fails unknown-option 'unknown argument: --wat' \
  --dry-run --wat
assert_fails root-out-dir '--out-dir must be a non-root path' \
  --dry-run --out-dir /
