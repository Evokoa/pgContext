#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TMPDIR="${TMPDIR:-${REPO_ROOT}/target/tmp}"
mkdir -p "${TMPDIR}"
work_dir="$(mktemp -d "${TMPDIR}/supply-chain-report-test.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT

fixture_root="${work_dir}/fixture"
fake_bin="${work_dir}/bin"
mkdir -p "${fixture_root}" "${fake_bin}"

assert_file_exists() {
  local path="$1"
  if [[ ! -f "${path}" ]]; then
    echo "expected file to exist: ${path}" >&2
    exit 1
  fi
}

cat >"${fake_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >>"${FAKE_SUPPLY_CHAIN_LOG:-/dev/null}"

if [[ "$*" == "-V" ]]; then
  printf 'cargo 1.96.0-test\n'
  exit 0
fi

if [[ "${FAKE_SUPPLY_CHAIN_FAIL_GATE:-}" == "audit" && "${1:-}" == "audit" ]]; then
  printf 'simulated cargo audit failure\n' >&2
  exit 41
fi
if [[ "${FAKE_SUPPLY_CHAIN_FAIL_GATE:-}" == "deny" && "${1:-}" == "deny" && "${2:-}" == "check" ]]; then
  printf 'simulated cargo deny failure\n' >&2
  exit 42
fi
if [[ "${FAKE_SUPPLY_CHAIN_EMPTY_GATE:-}" == "audit" && "${1:-}" == "audit" ]]; then
  exit 0
fi
if [[ "${FAKE_SUPPLY_CHAIN_EMPTY_GATE:-}" == "deny" && "${1:-}" == "deny" && "${2:-}" == "check" ]]; then
  exit 0
fi
if [[ "${FAKE_SUPPLY_CHAIN_NO_NEWLINE_GATE:-}" == "audit" && "${1:-}" == "audit" ]]; then
  printf 'fake cargo audit passed without trailing newline'
  exit 0
fi

case "${1:-}" in
  audit)
    printf 'fake cargo audit passed\n'
    exit 0
    ;;
  deny)
    if [[ "${2:-}" == "check" ]]; then
      printf 'fake cargo deny check passed\n'
      exit 0
    fi
    ;;
esac

printf 'unexpected cargo invocation: %s\n' "$*" >&2
exit 127
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

printf '%s\n' "$*" >>"${FAKE_SUPPLY_CHAIN_BASH_LOG:-/dev/null}"
if [[ "${1:-}" == "-lc" ]]; then
  printf 'login shell invocation is not allowed in supply-chain gates\n' >&2
  exit 88
fi
exec /bin/bash "$@"
SH
chmod +x "${fake_bin}/bash"

printf '[workspace]\n' >"${fixture_root}/Cargo.toml"
git -C "${fixture_root}" init -q
git -C "${fixture_root}" add .
git -C "${fixture_root}" \
  -c user.name='Supply Chain Test' \
  -c user.email='supply-chain-test@example.invalid' \
  commit -q -m 'initial clean fixture'

PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-supply-chain-report.sh" \
    --dry-run \
    --out-dir "${work_dir}/dry-run"

summary="${work_dir}/dry-run/summary.tsv"
report="${work_dir}/dry-run/report.md"
assert_file_exists "${summary}"
assert_file_exists "${report}"
head -n 1 "${summary}" | grep -q $'gate\tstatus\texit_code\tstarted_utc\tfinished_utc\tlog\tcommand\tlog_bytes'
grep -q $'cargo-audit\tdry-run\t0' "${summary}"
grep -q $'cargo-deny\tdry-run\t0' "${summary}"
grep -q -- '- Rows: `2`' "${report}"
grep -q -- '- Dry-run: `2`' "${report}"
grep -q -- '- Evidence directory: `external`' "${report}"
grep -q -- '- Approval: `incomplete`' "${report}"

PATH="${fake_bin}:${PATH}" \
  FAKE_SUPPLY_CHAIN_LOG="${work_dir}/success-cargo.log" \
  FAKE_SUPPLY_CHAIN_BASH_LOG="${work_dir}/success-bash.log" \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-supply-chain-report.sh" \
    --out-dir "${work_dir}/execute-success"

summary="${work_dir}/execute-success/summary.tsv"
report="${work_dir}/execute-success/report.md"
grep -q $'cargo-audit\tpassed\t0' "${summary}"
grep -q $'cargo-deny\tpassed\t0' "${summary}"
awk -F '\t' '$1 == "cargo-audit" && $8 > 0 { found = 1 } END { exit(found ? 0 : 1) }' "${summary}"
awk -F '\t' '$1 == "cargo-deny" && $8 > 0 { found = 1 } END { exit(found ? 0 : 1) }' "${summary}"
grep -q -- '- Passed: `2`' "${report}"
grep -q -- '- Dry-run: `0`' "${report}"
grep -q -- '- Failed: `0`' "${report}"
grep -q -- '- Evidence directory: `external`' "${report}"
grep -q -- '- Approval: `incomplete`' "${report}"
grep -q -- 'repo-contained evidence paths' "${report}"
grep -q '^supply-chain gate: cargo-audit$' "${work_dir}/execute-success/cargo-audit.log"
grep -q '^command: cargo audit --db target/cargo-audit-advisory-db$' "${work_dir}/execute-success/cargo-audit.log"
grep -q '^supply-chain gate status: passed$' "${work_dir}/execute-success/cargo-audit.log"
grep -q '^supply-chain gate exit code: 0$' "${work_dir}/execute-success/cargo-audit.log"
grep -q '^supply-chain raw output bytes: [1-9][0-9]*$' "${work_dir}/execute-success/cargo-audit.log"
grep -q '^supply-chain command output begin$' "${work_dir}/execute-success/cargo-audit.log"
grep -q '^fake cargo audit passed$' "${work_dir}/execute-success/cargo-audit.log"
grep -q '^supply-chain command output end$' "${work_dir}/execute-success/cargo-audit.log"
grep -q '^supply-chain gate: cargo-deny$' "${work_dir}/execute-success/cargo-deny.log"
grep -q '^command: cargo deny check$' "${work_dir}/execute-success/cargo-deny.log"
grep -q '^fake cargo deny check passed$' "${work_dir}/execute-success/cargo-deny.log"
grep -q '^audit --db target/cargo-audit-advisory-db$' "${work_dir}/success-cargo.log"
grep -q '^deny check$' "${work_dir}/success-cargo.log"
grep -q -- '^-c ' "${work_dir}/success-bash.log"
if grep -q -- '^-lc ' "${work_dir}/success-bash.log"; then
  echo "supply-chain runner should not use login shell gate execution" >&2
  exit 1
fi

PATH="${fake_bin}:${PATH}" \
  FAKE_SUPPLY_CHAIN_NO_NEWLINE_GATE=audit \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-supply-chain-report.sh" \
    --out-dir "${work_dir}/execute-no-newline"
grep -q $'cargo-audit\tpassed\t0' "${work_dir}/execute-no-newline/summary.tsv"
grep -q '^fake cargo audit passed without trailing newline$' \
  "${work_dir}/execute-no-newline/cargo-audit.log"
grep -q '^supply-chain command output end$' \
  "${work_dir}/execute-no-newline/cargo-audit.log"

repo_local_out="${fixture_root}/target/supply-chain-repo-local"
PATH="${fake_bin}:${PATH}" \
  FAKE_SUPPLY_CHAIN_LOG="${work_dir}/repo-local-cargo.log" \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-supply-chain-report.sh" \
    --out-dir "${repo_local_out}"
repo_local_summary="${repo_local_out}/summary.tsv"
repo_local_report="${repo_local_out}/report.md"
grep -q 'Summary TSV: `target/supply-chain-repo-local/summary.tsv`' \
  "${repo_local_report}"
grep -q -- '- Evidence directory: `repo`' "${repo_local_report}"
grep -q -- '- Approval: `complete`' "${repo_local_report}"
if grep -qF -- "${fixture_root}" "${repo_local_summary}" "${repo_local_report}"; then
  echo "repo-local supply-chain evidence paths should be repo-relative" >&2
  exit 1
fi
awk -F '\t' '
  NR > 1 && $6 ~ /^\// { bad = 1 }
  END { exit(bad ? 1 : 0) }
' "${repo_local_summary}"
rm -rf "${fixture_root}/target"

if PATH="${fake_bin}:${PATH}" \
  FAKE_SUPPLY_CHAIN_LOG="${work_dir}/failure-cargo.log" \
  FAKE_SUPPLY_CHAIN_FAIL_GATE=deny \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-supply-chain-report.sh" \
    --out-dir "${work_dir}/execute-failure"; then
  echo "failing supply-chain gate should fail the runner" >&2
  exit 1
fi
grep -q $'cargo-deny\tfailed\t42' "${work_dir}/execute-failure/summary.tsv"
grep -q -- '- Failed: `1`' "${work_dir}/execute-failure/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/execute-failure/report.md"
grep -q 'simulated cargo deny failure' "${work_dir}/execute-failure/cargo-deny.log"

if PATH="${fake_bin}:${PATH}" \
  FAKE_SUPPLY_CHAIN_LOG="${work_dir}/empty-cargo.log" \
  FAKE_SUPPLY_CHAIN_EMPTY_GATE=audit \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-supply-chain-report.sh" \
    --out-dir "${work_dir}/execute-empty"; then
  echo "empty supply-chain evidence should fail the runner" >&2
  exit 1
fi
grep -q $'cargo-audit\tfailed\t91' "${work_dir}/execute-empty/summary.tsv"
grep -q 'expected non-empty supply-chain evidence output for cargo-audit' \
  "${work_dir}/execute-empty/cargo-audit.log"
grep -q -- '- Approval: `incomplete`' "${work_dir}/execute-empty/report.md"

if PATH="${fake_bin}:${PATH}" \
  FAKE_SUPPLY_CHAIN_LOG="${work_dir}/empty-deny-cargo.log" \
  FAKE_SUPPLY_CHAIN_EMPTY_GATE=deny \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-supply-chain-report.sh" \
    --out-dir "${work_dir}/execute-empty-deny"; then
  echo "empty cargo deny evidence should fail the runner" >&2
  exit 1
fi
grep -q $'cargo-deny\tfailed\t91' "${work_dir}/execute-empty-deny/summary.tsv"
grep -q 'expected non-empty supply-chain evidence output for cargo-deny' \
  "${work_dir}/execute-empty-deny/cargo-deny.log"

printf 'dirty\n' >"${fixture_root}/dirty.txt"
if PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-supply-chain-report.sh" \
    --out-dir "${work_dir}/dirty" 2>"${work_dir}/dirty.err"; then
  echo "dirty supply-chain report should fail without override" >&2
  exit 1
fi
grep -q 'dirty worktree cannot produce release supply-chain evidence' "${work_dir}/dirty.err"

PATH="${fake_bin}:${PATH}" \
  FAKE_SUPPLY_CHAIN_LOG="${work_dir}/dirty-override-cargo.log" \
  REPO_ROOT="${fixture_root}" \
  "${REPO_ROOT}/scripts/run-supply-chain-report.sh" \
    --allow-dirty \
    --out-dir "${work_dir}/dirty-override"
grep -q -- '- Dirty override: `1`' "${work_dir}/dirty-override/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/dirty-override/report.md"

assert_fails() {
  local label="$1"
  local expected="$2"
  shift 2
  if REPO_ROOT="${fixture_root}" "${REPO_ROOT}/scripts/run-supply-chain-report.sh" "$@" 2>"${work_dir}/${label}.err"; then
    echo "${label} should fail" >&2
    exit 1
  fi
  grep -q -- "${expected}" "${work_dir}/${label}.err"
}

assert_fails unknown-option 'unknown argument: --wat' --dry-run --wat
assert_fails root-out-dir '--out-dir must be a non-root path' --dry-run --out-dir /
