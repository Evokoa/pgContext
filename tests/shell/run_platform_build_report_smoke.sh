#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TMPDIR="${TMPDIR:-${REPO_ROOT}/target/tmp}"
mkdir -p "${TMPDIR}"
work_dir="$(mktemp -d "${TMPDIR}/platform-build-report-test.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT

fixture_root="${work_dir}/fixture"
fake_bin="${work_dir}/bin"
mkdir -p \
  "${fixture_root}/scripts" \
  "${fixture_root}/tests/shell" \
  "${fake_bin}"

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

cat >"${fake_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >>"${FAKE_PLATFORM_LOG:-/dev/null}"

if [[ "$*" == "-V" ]]; then
  printf 'cargo 1.96.0-test\n'
  exit 0
fi

case "${FAKE_PLATFORM_FAIL_COMMAND:-}" in
  fmt)
    if [[ "${1:-}" == "fmt" ]]; then
      printf 'simulated fmt failure\n' >&2
      exit 31
    fi
    ;;
  test)
    if [[ "${1:-}" == "test" ]]; then
      printf 'simulated test failure\n' >&2
      exit 32
    fi
    ;;
esac

printf 'fake cargo %s\n' "$*"
SH
chmod +x "${fake_bin}/cargo"

cat >"${fake_bin}/rustc" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == "-V" ]]; then
  printf 'rustc 1.96.0-test\n'
  exit 0
fi
printf 'unexpected rustc invocation: %s\n' "$*" >&2
exit 127
SH
chmod +x "${fake_bin}/rustc"

cat >"${fake_bin}/bash" <<'SH'
#!/bin/bash
set -euo pipefail

printf '%s\n' "$*" >>"${FAKE_PLATFORM_BASH_LOG:-/dev/null}"
if [[ "${1:-}" == "-lc" ]]; then
  printf 'login shell invocation is not allowed in platform gates\n' >&2
  exit 88
fi
exec /bin/bash "$@"
SH
chmod +x "${fake_bin}/bash"

write_fake_script() {
  local path="$1"
  mkdir -p "$(dirname "${path}")"
  cat >"${path}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'fake %s\n' "$0"
SH
  chmod +x "${path}"
}

write_fake_script "${fixture_root}/scripts/check-parity-matrix.sh"
write_fake_script "${fixture_root}/scripts/check-source-hygiene.sh"
write_fake_script "${fixture_root}/scripts/release-linux-container-gates.sh"
write_fake_script "${fixture_root}/tests/shell/check_parity_matrix_smoke.sh"
write_fake_script "${fixture_root}/tests/shell/run_benchmark_report_smoke.sh"
write_fake_script "${fixture_root}/tests/shell/run_release_artifact_report_smoke.sh"
write_fake_script "${fixture_root}/tests/shell/run_security_review_report_smoke.sh"
write_fake_script "${fixture_root}/tests/shell/run_postgres_matrix_gates_smoke.sh"
write_fake_script "${fixture_root}/tests/shell/upgrade_matrix_staging_smoke.sh"
printf 'target/\n' >"${fixture_root}/.gitignore"

git -C "${fixture_root}" init -q
git -C "${fixture_root}" add .
git -C "${fixture_root}" \
  -c user.name='Platform Test' \
  -c user.email='platform-test@example.invalid' \
  commit -q -m 'initial clean fixture'

PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --dry-run \
    --platform macos \
    --platform linux \
    --out-dir "${work_dir}/dry-run"

summary="${work_dir}/dry-run/summary.tsv"
report="${work_dir}/dry-run/report.md"
assert_file_exists "${summary}"
assert_file_exists "${report}"
head -n 1 "${summary}" | grep -q $'platform\tgate\tstatus\texit_code\tstarted_utc\tfinished_utc\thost\tlog\tcommand\tlog_bytes'
assert_summary_row_count "${summary}" "24"
grep -q $'macos\tfmt\tdry-run\t0' "${summary}"
grep -q $'linux\tsource-hygiene\tdry-run\t0' "${summary}"
grep -q -- '- Rows: `24`' "${report}"
grep -q -- '- Dry-run: `24`' "${report}"
grep -q -- '- Full release scope: `1`' "${report}"
grep -q -- '- Approval: `incomplete`' "${report}"

PATH="${fake_bin}:${PATH}" \
  FAKE_PLATFORM_LOG="${work_dir}/success-cargo.log" \
  FAKE_PLATFORM_BASH_LOG="${work_dir}/success-bash.log" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  PLATFORM_ALLOW_CROSS_EXECUTION=1 \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --platform macos \
    --platform linux \
    --out-dir "${work_dir}/execute-success"

summary="${work_dir}/execute-success/summary.tsv"
report="${work_dir}/execute-success/report.md"
assert_summary_row_count "${summary}" "24"
grep -q $'macos\tfmt\tpassed\t0' "${summary}"
grep -q $'linux\tsource-hygiene\tpassed\t0' "${summary}"
awk -F '\t' '$1 == "macos" && $2 == "fmt" && $10 ~ /^[1-9][0-9]*$/ { found = 1 } END { exit(found ? 0 : 1) }' \
  "${summary}"
grep -q -- '- Rows: `24`' "${report}"
grep -q -- '- Passed: `24`' "${report}"
grep -q -- '- Dry-run: `0`' "${report}"
grep -q -- '- Skipped: `0`' "${report}"
grep -q -- '- Failed: `0`' "${report}"
grep -q -- '- Missing platforms: `none`' "${report}"
grep -q -- '- Full release scope: `1`' "${report}"
grep -q -- '- Approval: `complete`' "${report}"
grep -q -- 'fmt --check' "${work_dir}/success-cargo.log"
grep -q -- 'clippy --workspace --exclude context-pg' "${work_dir}/success-cargo.log"
grep -q -- '^-c ' "${work_dir}/success-bash.log"
if grep -q -- '^-lc ' "${work_dir}/success-bash.log"; then
  echo "platform runner should not use login shell gate execution" >&2
  exit 1
fi

repo_local_out="${fixture_root}/target/platform-local"
PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --platform macos \
    --out-dir "${repo_local_out}"
grep -q 'Summary TSV: `target/platform-local/summary.tsv`' \
  "${repo_local_out}/report.md"
awk -F '\t' '$1 == "macos" && $2 == "fmt" && $8 == "target/platform-local/macos-fmt.log" && $10 ~ /^[1-9][0-9]*$/ { found = 1 } END { exit(found ? 0 : 1) }' \
  "${repo_local_out}/summary.tsv"

macos_report_dir="${fixture_root}/target/platform-builds/macos-only"
linux_report_dir="${fixture_root}/target/platform-builds/linux-only"
PATH="${fake_bin}:${PATH}" \
  FAKE_PLATFORM_LOG="${work_dir}/macos-only-cargo.log" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  PLATFORM_HOST_OS_OVERRIDE='Darwin arm64 release-host' \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --platform macos \
    --out-dir "${macos_report_dir}"

PATH="${fake_bin}:${PATH}" \
  FAKE_PLATFORM_LOG="${work_dir}/linux-only-cargo.log" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=linux \
  PLATFORM_HOST_OS_OVERRIDE='Linux x86_64 release-host' \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --platform linux \
    --out-dir "${linux_report_dir}"

PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  PLATFORM_HOST_OS_OVERRIDE='Darwin arm64 merge-host' \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --merge-report "${macos_report_dir}/report.md" \
    --merge-report "${linux_report_dir}/report.md" \
    --out-dir "${work_dir}/merged"

summary="${work_dir}/merged/summary.tsv"
report="${work_dir}/merged/report.md"
assert_summary_row_count "${summary}" "24"
grep -q $'macos\tfmt\tpassed\t0' "${summary}"
grep -q $'linux\tsource-hygiene\tpassed\t0' "${summary}"
grep -q $'linux\tfmt\tpassed\t0\t' "${summary}"
grep -q 'Linux x86_64 release-host' "${summary}"
grep -q 'Darwin arm64 release-host' "${summary}"
grep -q -- '- Execution: `run`' "${report}"
grep -q -- '- Merged reports: `2`' "${report}"
grep -q -- '- Rows: `24`' "${report}"
grep -q -- '- Passed: `24`' "${report}"
grep -q -- '- Missing platforms: `none`' "${report}"
grep -q -- '- Full release scope: `1`' "${report}"
grep -q -- '- Approval: `complete`' "${report}"

absolute_summary_dir="${work_dir}/absolute-summary"
mkdir -p "${absolute_summary_dir}"
cp "${linux_report_dir}/report.md" "${absolute_summary_dir}/report.md"
cp "${linux_report_dir}/summary.tsv" "${absolute_summary_dir}/summary.tsv"
perl -0pi -e "s|target/platform-builds/linux-only/summary.tsv|${absolute_summary_dir}/summary.tsv|g" \
  "${absolute_summary_dir}/report.md"
if PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --merge-report "${absolute_summary_dir}/report.md" \
    --out-dir "${work_dir}/merged-absolute-summary" 2>"${work_dir}/merged-absolute-summary.err"; then
  echo "absolute-summary merged platform report should fail" >&2
  exit 1
fi
grep -q 'merged platform report summary TSV must be repo-relative' \
  "${work_dir}/merged-absolute-summary.err"

absolute_log_dir="${fixture_root}/target/platform-builds/absolute-log"
mkdir -p "${absolute_log_dir}"
cp -R "${linux_report_dir}/." "${absolute_log_dir}/"
perl -0pi -e 's|target/platform-builds/linux-only/linux-fmt.log|/tmp/linux-fmt.log|g' \
  "${absolute_log_dir}/summary.tsv"
perl -0pi -e 's|target/platform-builds/linux-only/summary.tsv|target/platform-builds/absolute-log/summary.tsv|g' \
  "${absolute_log_dir}/report.md"
if PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --merge-report "${absolute_log_dir}/report.md" \
    --out-dir "${work_dir}/merged-absolute-log" 2>"${work_dir}/merged-absolute-log.err"; then
  echo "absolute-log merged platform report should fail" >&2
  exit 1
fi
grep -q 'merged platform report summary TSV has unsafe log path: /tmp/linux-fmt.log' \
  "${work_dir}/merged-absolute-log.err"

symlink_summary_dir="${fixture_root}/target/platform-builds/symlink-summary"
mkdir -p "${symlink_summary_dir}"
cp "${linux_report_dir}/report.md" "${symlink_summary_dir}/report.md"
ln -s ../linux-only/summary.tsv "${symlink_summary_dir}/summary.tsv"
perl -0pi -e 's|target/platform-builds/linux-only/summary.tsv|target/platform-builds/symlink-summary/summary.tsv|g' \
  "${symlink_summary_dir}/report.md"
if PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --merge-report "${symlink_summary_dir}/report.md" \
    --out-dir "${work_dir}/merged-symlink-summary" 2>"${work_dir}/merged-symlink-summary.err"; then
  echo "symlink-summary merged platform report should fail" >&2
  exit 1
fi
grep -q 'merged platform report summary TSV must not be a symlink' \
  "${work_dir}/merged-symlink-summary.err"

symlink_log_dir="${fixture_root}/target/platform-builds/symlink-log"
mkdir -p "${symlink_log_dir}"
cp -R "${linux_report_dir}/." "${symlink_log_dir}/"
perl -0pi -e 's|target/platform-builds/linux-only/summary.tsv|target/platform-builds/symlink-log/summary.tsv|g' \
  "${symlink_log_dir}/report.md"
perl -0pi -e 's|target/platform-builds/linux-only/linux-fmt.log|target/platform-builds/symlink-log/linux-fmt.log|g' \
  "${symlink_log_dir}/summary.tsv"
rm "${symlink_log_dir}/linux-fmt.log"
ln -s ../linux-only/linux-fmt.log "${symlink_log_dir}/linux-fmt.log"
if PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --merge-report "${symlink_log_dir}/report.md" \
    --out-dir "${work_dir}/merged-symlink-log" 2>"${work_dir}/merged-symlink-log.err"; then
  echo "symlink-log merged platform report should fail" >&2
  exit 1
fi
grep -q 'merged platform report log must not be a symlink for linux fmt' \
  "${work_dir}/merged-symlink-log.err"

if PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --merge-report "${macos_report_dir}/report.md" \
    --merge-report "${macos_report_dir}/report.md" \
    --out-dir "${work_dir}/merged-duplicate" 2>"${work_dir}/merged-duplicate.err"; then
  echo "duplicate merged platform rows should fail" >&2
  exit 1
fi
grep -q 'merged platform reports contain duplicate platform/gate rows' \
  "${work_dir}/merged-duplicate.err"

wrong_gate_dir="${fixture_root}/target/platform-builds/wrong-gate"
mkdir -p "${wrong_gate_dir}"
cp "${linux_report_dir}/report.md" "${wrong_gate_dir}/report.md"
cp "${linux_report_dir}/summary.tsv" "${wrong_gate_dir}/summary.tsv"
perl -0pi -e 's/^linux\tfmt\tpassed\t0/linux\tformat\tpassed\t0/m' \
  "${wrong_gate_dir}/summary.tsv"
perl -0pi -e 's|target/platform-builds/linux-only/summary.tsv|target/platform-builds/wrong-gate/summary.tsv|g' \
  "${wrong_gate_dir}/report.md"
if PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --merge-report "${macos_report_dir}/report.md" \
    --merge-report "${wrong_gate_dir}/report.md" \
    --out-dir "${work_dir}/merged-wrong-gate" 2>"${work_dir}/merged-wrong-gate.err"; then
  echo "wrong-gate merged platform report should fail" >&2
  exit 1
fi
grep -q 'merged platform reports do not contain the expected platform gate command set' \
  "${work_dir}/merged-wrong-gate.err"

missing_gate_dir="${fixture_root}/target/platform-builds/missing-gate"
mkdir -p "${missing_gate_dir}"
cp "${linux_report_dir}/report.md" "${missing_gate_dir}/report.md"
cp "${linux_report_dir}/summary.tsv" "${missing_gate_dir}/summary.tsv"
perl -0pi -e 's|target/platform-builds/linux-only/summary.tsv|target/platform-builds/missing-gate/summary.tsv|g' \
  "${missing_gate_dir}/report.md"
perl -0pi -e 's/^linux\tupgrade-matrix-staging-smoke\t[^\n]*\n//m' \
  "${missing_gate_dir}/summary.tsv"
if PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --merge-report "${macos_report_dir}/report.md" \
    --merge-report "${missing_gate_dir}/report.md" \
    --out-dir "${work_dir}/merged-missing-gate" 2>"${work_dir}/merged-missing-gate.err"; then
  echo "missing-gate merged platform report should fail" >&2
  exit 1
fi
grep -q 'merged platform reports do not contain the expected platform gate command set' \
  "${work_dir}/merged-missing-gate.err"

wrong_title="${work_dir}/wrong-title-report.md"
printf '# Wrong Report\n\nSummary TSV: `%s`\n' "target/platform-builds/macos-only/summary.tsv" >"${wrong_title}"
if PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --merge-report "${wrong_title}" \
    --out-dir "${work_dir}/merged-wrong-title" 2>"${work_dir}/merged-wrong-title.err"; then
  echo "wrong-title merged platform report should fail" >&2
  exit 1
fi
grep -q 'merged platform report has wrong title' "${work_dir}/merged-wrong-title.err"

missing_log_bytes_dir="${fixture_root}/target/platform-builds/missing-log-bytes"
mkdir -p "${missing_log_bytes_dir}"
cp "${linux_report_dir}/report.md" "${missing_log_bytes_dir}/report.md"
cp "${linux_report_dir}/summary.tsv" "${missing_log_bytes_dir}/summary.tsv"
perl -0pi -e 's/\t[0-9]+\n$/\n/m' "${missing_log_bytes_dir}/summary.tsv"
perl -0pi -e 's|target/platform-builds/linux-only/summary.tsv|target/platform-builds/missing-log-bytes/summary.tsv|g' \
  "${missing_log_bytes_dir}/report.md"
if PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --merge-report "${macos_report_dir}/report.md" \
    --merge-report "${missing_log_bytes_dir}/report.md" \
    --out-dir "${work_dir}/merged-missing-log-bytes" 2>"${work_dir}/merged-missing-log-bytes.err"; then
  echo "missing log-byte merged platform report should fail" >&2
  exit 1
fi
grep -q 'merged platform report summary TSV has invalid log-byte evidence' \
  "${work_dir}/merged-missing-log-bytes.err"

missing_merged_log_dir="${fixture_root}/target/platform-builds/missing-merged-log"
mkdir -p "${missing_merged_log_dir}"
cp -R "${linux_report_dir}/." "${missing_merged_log_dir}/"
perl -0pi -e 's|target/platform-builds/linux-only/summary.tsv|target/platform-builds/missing-merged-log/summary.tsv|g' \
  "${missing_merged_log_dir}/report.md"
perl -0pi -e 's|target/platform-builds/linux-only/|target/platform-builds/missing-merged-log/|g' \
  "${missing_merged_log_dir}/summary.tsv"
missing_merged_log_path="${fixture_root}/$(awk -F '\t' '$1 == "linux" && $2 == "fmt" { print $8; exit }' "${missing_merged_log_dir}/summary.tsv")"
rm "${missing_merged_log_path}"
if PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --merge-report "${macos_report_dir}/report.md" \
    --merge-report "${missing_merged_log_dir}/report.md" \
    --out-dir "${work_dir}/merged-missing-log" 2>"${work_dir}/merged-missing-log.err"; then
  echo "missing merged platform log should fail" >&2
  exit 1
fi
grep -q 'merged platform report log is missing for linux fmt' \
  "${work_dir}/merged-missing-log.err"

tampered_merged_log_dir="${fixture_root}/target/platform-builds/tampered-merged-log"
mkdir -p "${tampered_merged_log_dir}"
cp -R "${linux_report_dir}/." "${tampered_merged_log_dir}/"
perl -0pi -e 's|target/platform-builds/linux-only/summary.tsv|target/platform-builds/tampered-merged-log/summary.tsv|g' \
  "${tampered_merged_log_dir}/report.md"
perl -0pi -e 's|target/platform-builds/linux-only/|target/platform-builds/tampered-merged-log/|g' \
  "${tampered_merged_log_dir}/summary.tsv"
tampered_merged_log_path="${fixture_root}/$(awk -F '\t' '$1 == "linux" && $2 == "source-hygiene" { print $8; exit }' "${tampered_merged_log_dir}/summary.tsv")"
printf 'tampered merged log\n' >"${tampered_merged_log_path}"
if PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --merge-report "${macos_report_dir}/report.md" \
    --merge-report "${tampered_merged_log_dir}/report.md" \
    --out-dir "${work_dir}/merged-tampered-log" 2>"${work_dir}/merged-tampered-log.err"; then
  echo "tampered merged platform log should fail" >&2
  exit 1
fi
grep -q 'merged platform report log-byte evidence does not match linux source-hygiene' \
  "${work_dir}/merged-tampered-log.err"

PATH="${fake_bin}:${PATH}" \
  FAKE_PLATFORM_LOG="${work_dir}/skip-cargo.log" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --platform macos \
    --platform linux \
    --out-dir "${work_dir}/execute-skipped"
grep -q $'linux\tfmt\tskipped\t0' "${work_dir}/execute-skipped/summary.tsv"
grep -q -- '- Skipped: `12`' "${work_dir}/execute-skipped/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/execute-skipped/report.md"

if PATH="${fake_bin}:${PATH}" \
  FAKE_PLATFORM_LOG="${work_dir}/failure-cargo.log" \
  FAKE_PLATFORM_FAIL_COMMAND=fmt \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --platform macos \
    --out-dir "${work_dir}/execute-failure"; then
  echo "failing platform gate should fail the runner" >&2
  exit 1
fi
grep -q $'macos\tfmt\tfailed\t31' "${work_dir}/execute-failure/summary.tsv"
grep -q -- '- Failed: `1`' "${work_dir}/execute-failure/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/execute-failure/report.md"
grep -q 'simulated fmt failure' "${work_dir}/execute-failure/macos-fmt.log"

printf 'dirty\n' >"${fixture_root}/dirty.txt"
if PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PLATFORM_HOST_OVERRIDE=macos \
  "${REPO_ROOT}/scripts/run-platform-build-report.sh" \
    --platform macos \
    --out-dir "${work_dir}/dirty" 2>"${work_dir}/dirty.err"; then
  echo "dirty platform report should fail without override" >&2
  exit 1
fi
grep -q 'dirty worktree cannot produce release platform evidence' "${work_dir}/dirty.err"

assert_fails() {
  local label="$1"
  local expected="$2"
  shift 2
  if REPO_ROOT="${fixture_root}" "${REPO_ROOT}/scripts/run-platform-build-report.sh" "$@" 2>"${work_dir}/${label}.err"; then
    echo "${label} should fail" >&2
    exit 1
  fi
  grep -q -- "${expected}" "${work_dir}/${label}.err"
}

assert_fails duplicate-platform 'duplicate platform: macos' \
  --dry-run --platform macos --platform macos --out-dir "${work_dir}/duplicate"
assert_fails unsupported-platform 'unsupported platform: windows' \
  --dry-run --platform windows --out-dir "${work_dir}/unsupported"
assert_fails bad-pg-major '--pg-major must be one of: 15, 16, 17, 18' \
  --dry-run --pg-major 14 --out-dir "${work_dir}/bad-pg"
assert_fails root-out-dir '--out-dir must be a non-root path' \
  --dry-run --out-dir /
assert_fails missing-platform-value '--platform requires a value' \
  --dry-run --platform
assert_fails missing-merge-report-value '--merge-report requires a value' \
  --merge-report
assert_fails merge-with-platform '--merge-report cannot be combined with explicit --platform rows' \
  --dry-run --platform macos --merge-report "${macos_report_dir}/report.md"
