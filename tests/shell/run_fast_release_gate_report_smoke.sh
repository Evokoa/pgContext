#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TMPDIR="${TMPDIR:-${REPO_ROOT}/target/tmp}"
export PGRX_TEST_PLATFORM=Linux
mkdir -p "${TMPDIR}"
work_dir="$(mktemp -d "${TMPDIR}/fast-release-gate-report-test.XXXXXX")"
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

fixture_root="${work_dir}/fixture"
fake_bin="${work_dir}/bin"
fake_no_pgrx_bin="${work_dir}/bin-no-pgrx"
fake_wrong_pg_bin="${work_dir}/bin-wrong-pg"
mkdir -p "${fixture_root}/scripts" "${fake_bin}" "${fake_no_pgrx_bin}" "${fake_wrong_pg_bin}"
cp "${REPO_ROOT}/scripts/run-v1-pgrx-tests.sh" "${fixture_root}/scripts/run-v1-pgrx-tests.sh"

cat >"${fixture_root}/Cargo.toml" <<'DOC'
[workspace.metadata.pgcontext]
primary-postgres-version = "17"
supported-postgres-versions = ["15", "16", "17", "18"]
DOC
printf 'target/\n' >"${fixture_root}/.gitignore"

cat >"${fake_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >>"${FAKE_FAST_GATE_LOG:-/dev/null}"

if [[ "$*" == "-V" ]]; then
  printf 'cargo 1.96.0-test\n'
  exit 0
fi
if [[ "${1:-}" == "pgrx" && "${2:-}" == "--version" ]]; then
  printf 'cargo-pgrx 0.19.1-test\n'
  exit 0
fi
if [[ "${1:-}" == "pgrx" && "${2:-}" == "info" && "${3:-}" == "pg-config" && "${4:-}" == "pg17" ]]; then
  printf '%s/pg_config\n' "$(dirname "$0")"
  exit 0
fi

gate=""
case "$*" in
  "fmt --check") gate="fmt" ;;
  "clippy --workspace --exclude context-pg --all-targets --all-features -- -D warnings") gate="clippy-workspace" ;;
  "clippy -p context-pg --all-targets --features pg17 -- -D warnings") gate="clippy-context-pg" ;;
  "test --workspace --exclude context-pg --all-features") gate="workspace-tests" ;;
  "check -p context-pg --features pg17") gate="context-pg-check" ;;
  "pgrx test --release -p context-pg pg17") gate="context-pg-pgrx" ;;
  "doc --workspace --no-deps") gate="docs" ;;
  "audit --db target/cargo-audit-advisory-db") gate="cargo-audit" ;;
  "deny check") gate="cargo-deny" ;;
esac

if [[ -z "${gate}" ]]; then
  printf 'unexpected cargo invocation: %s\n' "$*" >&2
  exit 127
fi
if [[ "${FAKE_FAST_GATE_FAIL_GATE:-}" == "${gate}" ]]; then
  printf 'simulated %s failure\n' "${gate}" >&2
  exit 42
fi

printf 'fake %s passed\n' "${gate}"
SH
chmod +x "${fake_bin}/cargo"

cat >"${fake_no_pgrx_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${FAKE_FAST_GATE_LOG:-/dev/null}"
if [[ "$*" == "-V" ]]; then
  printf 'cargo 1.96.0-test\n'
  exit 0
fi
if [[ "${1:-}" == "pgrx" && "${2:-}" == "--version" ]]; then
  printf 'cargo-pgrx unavailable\n' >&2
  exit 127
fi
if [[ "${1:-}" == "pgrx" ]]; then
  printf 'cargo-pgrx unavailable\n' >&2
  exit 127
fi
printf 'fake non-pgrx cargo passed: %s\n' "$*"
SH
chmod +x "${fake_no_pgrx_bin}/cargo"

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

cat >"${fake_bin}/pg_config" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == "--version" ]]; then
  printf 'PostgreSQL 17.0-test\n'
  exit 0
fi
printf 'unexpected pg_config invocation: %s\n' "$*" >&2
exit 127
SH
chmod +x "${fake_bin}/pg_config"

for script in \
  check-parity-matrix.sh \
  check-source-hygiene.sh
do
  cat >"${fixture_root}/scripts/${script}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
gate="$(basename "$0" .sh)"
gate="${gate#check-}"
printf '%s\n' "${gate}" >>"${FAKE_FAST_GATE_LOG:-/dev/null}"
case "$(basename "$0")" in
  check-parity-matrix.sh) gate_name="parity-matrix" ;;
  check-source-hygiene.sh) gate_name="source-hygiene" ;;
  *) gate_name="unknown" ;;
esac
if [[ "${FAKE_FAST_GATE_FAIL_GATE:-}" == "${gate_name}" ]]; then
  printf 'simulated %s failure\n' "${gate_name}" >&2
  exit 43
fi
printf 'fake %s passed\n' "${gate_name}"
SH
  chmod +x "${fixture_root}/scripts/${script}"
done

git -C "${fixture_root}" init -q
git -C "${fixture_root}" add .
git -C "${fixture_root}" \
  -c user.name='Fast Gate Test' \
  -c user.email='fast-gate-test@example.invalid' \
  commit -q -m 'initial clean fixture'

PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-fast-release-gate-report.sh" \
    --dry-run \
    --out-dir "${work_dir}/dry-run"

summary="${work_dir}/dry-run/summary.tsv"
report="${work_dir}/dry-run/report.md"
assert_file_exists "${summary}"
assert_file_exists "${report}"
head -n 1 "${summary}" | grep -q $'gate\tstatus\texit_code\tstarted_utc\tfinished_utc\tlog\tcommand\tlog_bytes'
assert_summary_row_count "${summary}" "11"
grep -q $'fmt\tdry-run\t0' "${summary}"
grep -q $'cargo-deny\tdry-run\t0' "${summary}"
grep -q -- '- Rows: `11`' "${report}"
grep -q -- '- Dry-run: `11`' "${report}"
grep -q -- '- Approval: `incomplete`' "${report}"

PATH="${fake_bin}:${PATH}" \
  FAKE_FAST_GATE_LOG="${work_dir}/success.log" \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-fast-release-gate-report.sh" \
    --out-dir "${work_dir}/success"

summary="${work_dir}/success/summary.tsv"
report="${work_dir}/success/report.md"
grep -q -- '- Worktree: `clean`' "${report}"
grep -q -- '- Execution: `run`' "${report}"
grep -q -- '- PG config matches major: `1`' "${report}"
grep -q -- '- Rows: `11`' "${report}"
grep -q -- '- Passed: `11`' "${report}"
grep -q -- '- Failed: `0`' "${report}"
grep -q -- '- Missing logs: `0`' "${report}"
grep -q -- '- Approval: `complete`' "${report}"
for gate in \
  fmt \
  clippy-workspace \
  clippy-context-pg \
  workspace-tests \
  context-pg-check \
  context-pg-pgrx \
  docs \
  parity-matrix \
  source-hygiene \
  cargo-audit \
  cargo-deny
do
  awk -F '\t' -v gate="${gate}" '
    $1 == gate && $2 == "passed" && $3 == "0" && $8 ~ /^[1-9][0-9]*$/ { found = 1 }
    END { exit(found ? 0 : 1) }
  ' "${summary}"
done
grep -q '^pgrx test --release -p context-pg pg17$' "${work_dir}/success.log"
grep -q '^audit --db target/cargo-audit-advisory-db$' "${work_dir}/success.log"
grep -q '^deny check$' "${work_dir}/success.log"

PATH="${fake_bin}:${PATH}" \
  FAKE_FAST_GATE_LOG="${work_dir}/repo-relative.log" \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-fast-release-gate-report.sh" \
    --out-dir "${fixture_root}/target/release-evidence/fast-gates"
summary="${fixture_root}/target/release-evidence/fast-gates/summary.tsv"
report="${fixture_root}/target/release-evidence/fast-gates/report.md"
grep -q 'Summary TSV: `target/release-evidence/fast-gates/summary.tsv`' "${report}"
if grep -qF -- "${fixture_root}" "${summary}" "${report}"; then
  echo "repo-local fast gate evidence paths should be repo-relative" >&2
  exit 1
fi
awk -F '\t' '
  NR > 1 && ($6 ~ /^\// || $8 !~ /^[1-9][0-9]*$/) { bad = 1 }
  END { exit(bad ? 1 : 0) }
' "${summary}"

cp "${fake_bin}/cargo" "${fake_wrong_pg_bin}/cargo"
cp "${fake_bin}/rustc" "${fake_wrong_pg_bin}/rustc"
cat >"${fake_wrong_pg_bin}/pg_config" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == "--version" ]]; then
  printf 'PostgreSQL 16.99-wrong-major\n'
  exit 0
fi
printf 'unexpected pg_config invocation: %s\n' "$*" >&2
exit 127
SH
chmod +x "${fake_wrong_pg_bin}/pg_config"

cat >"${fake_wrong_pg_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "pgrx" && "${2:-}" == "info" && "${3:-}" == "pg-config" && "${4:-}" == "pg17" ]]; then
  printf '%s/pg_config\n' "$(dirname "$0")"
  exit 0
fi
exec "${FAKE_FAST_GATE_FALLBACK_CARGO}" "$@"
SH
chmod +x "${fake_wrong_pg_bin}/cargo"

PATH="${fake_wrong_pg_bin}:${PATH}" \
  FAKE_FAST_GATE_FALLBACK_CARGO="${fake_bin}/cargo" \
  FAKE_FAST_GATE_LOG="${work_dir}/wrong-pg-config.log" \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-fast-release-gate-report.sh" \
    --out-dir "${work_dir}/wrong-pg-config"
grep -q -- '- PG config: `PostgreSQL 16.99-wrong-major`' "${work_dir}/wrong-pg-config/report.md"
grep -q -- '- PG config matches major: `0`' "${work_dir}/wrong-pg-config/report.md"
grep -q -- '- Passed: `11`' "${work_dir}/wrong-pg-config/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/wrong-pg-config/report.md"
grep -q 'pg_config matching the selected PostgreSQL major' \
  "${work_dir}/wrong-pg-config/report.md"

if PATH="${fake_bin}:${PATH}" \
  FAKE_FAST_GATE_LOG="${work_dir}/failure.log" \
  FAKE_FAST_GATE_FAIL_GATE=context-pg-pgrx \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-fast-release-gate-report.sh" \
    --out-dir "${work_dir}/failure"; then
  echo "failing fast gate should fail the runner" >&2
  exit 1
fi
grep -q $'context-pg-pgrx\tfailed\t42' "${work_dir}/failure/summary.tsv"
grep -q -- '- Failed: `1`' "${work_dir}/failure/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/failure/report.md"
grep -q 'simulated context-pg-pgrx failure' "${work_dir}/failure/context-pg-pgrx.log"

cp "${fake_bin}/rustc" "${fake_no_pgrx_bin}/rustc"
cp "${fake_bin}/pg_config" "${fake_no_pgrx_bin}/pg_config"
if PATH="${fake_no_pgrx_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-fast-release-gate-report.sh" \
    --out-dir "${work_dir}/missing-pgrx"; then
  echo "missing cargo-pgrx should fail the runner" >&2
  exit 1
fi
grep -q $'context-pg-pgrx\tfailed' "${work_dir}/missing-pgrx/summary.tsv"
grep -q -- '- Cargo pgrx: `unavailable`' "${work_dir}/missing-pgrx/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/missing-pgrx/report.md"

printf 'dirty\n' >"${fixture_root}/dirty.txt"
if PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-fast-release-gate-report.sh" \
    --out-dir "${work_dir}/dirty" 2>"${work_dir}/dirty.err"; then
  echo "dirty fast release-gate report should fail without override" >&2
  exit 1
fi
grep -q 'dirty worktree cannot produce fast release-gate evidence' "${work_dir}/dirty.err"

PATH="${fake_bin}:${PATH}" \
  FAKE_FAST_GATE_LOG="${work_dir}/dirty-override.log" \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-fast-release-gate-report.sh" \
    --allow-dirty \
    --out-dir "${work_dir}/dirty-override"
grep -q -- '- Dirty override: `1`' "${work_dir}/dirty-override/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/dirty-override/report.md"

assert_fails() {
  local label="$1"
  local expected="$2"
  shift 2
  if PATH="${fake_bin}:${PATH}" REPO_ROOT="${fixture_root}" \
    "${REPO_ROOT}/scripts/run-fast-release-gate-report.sh" "$@" \
    2>"${work_dir}/${label}.err"; then
    echo "${label} should fail" >&2
    exit 1
  fi
  grep -q -- "${expected}" "${work_dir}/${label}.err"
}

assert_fails unknown-option 'unknown argument: --wat' --dry-run --wat
assert_fails root-out-dir '--out-dir must be a non-root path' --dry-run --out-dir /
assert_fails unsupported-pg '--pg-major must be one of supported-postgres-versions' --dry-run --pg-major 14
