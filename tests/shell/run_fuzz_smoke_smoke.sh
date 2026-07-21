#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TMPDIR="${TMPDIR:-${REPO_ROOT}/target/tmp}"
mkdir -p "${TMPDIR}"
work_dir="$(mktemp -d "${TMPDIR}/fuzz-smoke-test.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT

make_fixture() {
  local root="$1"
  rm -rf "${root}"
  mkdir -p "${root}/scripts" "${root}/fuzz/fuzz_targets" \
    "${root}/fuzz/corpus/alpha" "${root}/fuzz/corpus/beta" "${root}/fake-bin"
  cp "${REPO_ROOT}/scripts/run-fuzz-smoke.sh" "${root}/scripts/"
  chmod +x "${root}/scripts/run-fuzz-smoke.sh"
  cat >"${root}/fuzz/smoke-targets.txt" <<'EOF'
# target|max_len
alpha|128
beta|256
EOF
  printf '%s\n' '#![no_main]' >"${root}/fuzz/fuzz_targets/alpha.rs"
  printf '%s\n' '#![no_main]' >"${root}/fuzz/fuzz_targets/beta.rs"
  printf '%s\n' 'alpha seed' >"${root}/fuzz/corpus/alpha/seed"
  printf '%s\n' 'beta seed' >"${root}/fuzz/corpus/beta/seed"
  cat >"${root}/fake-bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${FAKE_CARGO_LOG}"
EOF
  chmod +x "${root}/fake-bin/cargo"
}

fixture="${work_dir}/fixture"
make_fixture "${fixture}"

REPO_ROOT="${fixture}" "${fixture}/scripts/run-fuzz-smoke.sh" --list \
  >"${work_dir}/targets.txt"
diff -u <(printf 'alpha\nbeta\n') "${work_dir}/targets.txt"

REPO_ROOT="${fixture}" "${fixture}/scripts/run-fuzz-smoke.sh" \
  --dry-run --runs 12 --seed 4242 >"${work_dir}/dry-run.txt"
grep -q 'fuzz run alpha' "${work_dir}/dry-run.txt"
grep -q -- '-runs=12 -seed=4242 -max_len=128 -timeout=5' "${work_dir}/dry-run.txt"
grep -q 'fuzz run beta' "${work_dir}/dry-run.txt"

FAKE_CARGO_LOG="${work_dir}/cargo.log" \
PATH="${fixture}/fake-bin:${PATH}" \
REPO_ROOT="${fixture}" \
  "${fixture}/scripts/run-fuzz-smoke.sh" --runs 7 --seed 99
[[ "$(wc -l <"${work_dir}/cargo.log" | tr -d ' ')" == "2" ]]
grep -q '+nightly fuzz run alpha' "${work_dir}/cargo.log"
grep -q -- '-runs=7 -seed=99 -max_len=128 -timeout=5' "${work_dir}/cargo.log"
[[ "$(find "${fixture}/fuzz/corpus/alpha" -type f | wc -l | tr -d ' ')" == "1" ]]
[[ -f "${fixture}/target/fuzz-smoke/corpus/alpha/seed" ]]

FAKE_CARGO_LOG="${work_dir}/single.log" \
PATH="${fixture}/fake-bin:${PATH}" \
REPO_ROOT="${fixture}" \
  "${fixture}/scripts/run-fuzz-smoke.sh" --target beta
[[ "$(wc -l <"${work_dir}/single.log" | tr -d ' ')" == "1" ]]
grep -q 'fuzz run beta' "${work_dir}/single.log"

if REPO_ROOT="${fixture}" "${fixture}/scripts/run-fuzz-smoke.sh" \
  --target missing --dry-run >"${work_dir}/missing.out" 2>"${work_dir}/missing.err"; then
  echo "unregistered fuzz target should fail" >&2
  exit 1
fi
grep -q 'fuzz smoke target is not registered: missing' "${work_dir}/missing.err"

rm -rf "${fixture}/fuzz/corpus/beta"
if REPO_ROOT="${fixture}" "${fixture}/scripts/run-fuzz-smoke.sh" \
  --target beta --dry-run >"${work_dir}/corpus.out" 2>"${work_dir}/corpus.err"; then
  echo "missing fuzz corpus should fail" >&2
  exit 1
fi
grep -q 'fuzz smoke corpus is missing: fuzz/corpus/beta' "${work_dir}/corpus.err"

for invalid in 0 0001 10001 18446744073709551680 not-a-number; do
  if REPO_ROOT="${fixture}" "${fixture}/scripts/run-fuzz-smoke.sh" \
    --runs "${invalid}" --dry-run >"${work_dir}/runs.out" 2>"${work_dir}/runs.err"; then
    echo "invalid fuzz run bound should fail: ${invalid}" >&2
    exit 1
  fi
  grep -q 'fuzz smoke runs must be an integer in 1..=10000' "${work_dir}/runs.err"
done

for invalid in -1 0001 4294967296 18446744073709551616 not-a-number; do
  if REPO_ROOT="${fixture}" "${fixture}/scripts/run-fuzz-smoke.sh" \
    --seed "${invalid}" --dry-run >"${work_dir}/seed.out" 2>"${work_dir}/seed.err"; then
    echo "invalid fuzz seed should fail: ${invalid}" >&2
    exit 1
  fi
  grep -q 'fuzz smoke seed must be an unsigned 32-bit integer' "${work_dir}/seed.err"
done

validation_fixture="${work_dir}/validation-fixture"
make_fixture "${validation_fixture}"
cat >>"${validation_fixture}/fuzz/smoke-targets.txt" <<'EOF'
alpha|512
EOF
if REPO_ROOT="${validation_fixture}" \
  "${validation_fixture}/scripts/run-fuzz-smoke.sh" --dry-run \
  >"${work_dir}/duplicate.out" 2>"${work_dir}/duplicate.err"; then
  echo "duplicate fuzz registry target should fail" >&2
  exit 1
fi
grep -q 'duplicate fuzz smoke target: alpha' "${work_dir}/duplicate.err"

make_fixture "${validation_fixture}"
printf '%s\n' 'alpha|0' >"${validation_fixture}/fuzz/smoke-targets.txt"
if REPO_ROOT="${validation_fixture}" \
  "${validation_fixture}/scripts/run-fuzz-smoke.sh" --dry-run \
  >"${work_dir}/max-len.out" 2>"${work_dir}/max-len.err"; then
  echo "invalid fuzz max_len should fail" >&2
  exit 1
fi
grep -q 'invalid fuzz smoke max_len for alpha: 0' "${work_dir}/max-len.err"

make_fixture "${validation_fixture}"
printf '%s\n' 'alpha|18446744073709551616' \
  >"${validation_fixture}/fuzz/smoke-targets.txt"
if REPO_ROOT="${validation_fixture}" \
  "${validation_fixture}/scripts/run-fuzz-smoke.sh" --dry-run \
  >"${work_dir}/huge-max-len.out" 2>"${work_dir}/huge-max-len.err"; then
  echo "overflowing fuzz max_len should fail" >&2
  exit 1
fi
grep -q 'invalid fuzz smoke max_len for alpha: 18446744073709551616' \
  "${work_dir}/huge-max-len.err"

make_fixture "${validation_fixture}"
printf '%s\n' 'alpha|128|unexpected' >"${validation_fixture}/fuzz/smoke-targets.txt"
if REPO_ROOT="${validation_fixture}" \
  "${validation_fixture}/scripts/run-fuzz-smoke.sh" --dry-run \
  >"${work_dir}/row.out" 2>"${work_dir}/row.err"; then
  echo "malformed fuzz registry row should fail" >&2
  exit 1
fi
grep -q 'invalid fuzz smoke registry row: alpha|128|unexpected' "${work_dir}/row.err"

make_fixture "${validation_fixture}"
rm "${validation_fixture}/fuzz/fuzz_targets/alpha.rs"
if REPO_ROOT="${validation_fixture}" \
  "${validation_fixture}/scripts/run-fuzz-smoke.sh" --target alpha --dry-run \
  >"${work_dir}/source.out" 2>"${work_dir}/source.err"; then
  echo "missing fuzz target source should fail" >&2
  exit 1
fi
grep -q 'fuzz smoke target source is missing: fuzz/fuzz_targets/alpha.rs' \
  "${work_dir}/source.err"

make_fixture "${validation_fixture}"
mv "${validation_fixture}/fuzz/fuzz_targets/alpha.rs" \
  "${validation_fixture}/fuzz/fuzz_targets/real-alpha.rs"
ln -s "${validation_fixture}/fuzz/fuzz_targets/real-alpha.rs" \
  "${validation_fixture}/fuzz/fuzz_targets/alpha.rs"
if REPO_ROOT="${validation_fixture}" \
  "${validation_fixture}/scripts/run-fuzz-smoke.sh" --target alpha --dry-run \
  >"${work_dir}/source-symlink.out" 2>"${work_dir}/source-symlink.err"; then
  echo "symlinked fuzz target source should fail" >&2
  exit 1
fi
grep -q 'fuzz smoke target source must not be a symlink: fuzz/fuzz_targets/alpha.rs' \
  "${work_dir}/source-symlink.err"

make_fixture "${validation_fixture}"
rm "${validation_fixture}/fuzz/corpus/alpha/seed"
if REPO_ROOT="${validation_fixture}" \
  "${validation_fixture}/scripts/run-fuzz-smoke.sh" --target alpha --dry-run \
  >"${work_dir}/empty.out" 2>"${work_dir}/empty.err"; then
  echo "empty fuzz corpus should fail" >&2
  exit 1
fi
grep -q 'fuzz smoke corpus is empty: fuzz/corpus/alpha' "${work_dir}/empty.err"

make_fixture "${validation_fixture}"
printf '%s\n' 'external seed' >"${work_dir}/external-seed"
ln -s "${work_dir}/external-seed" \
  "${validation_fixture}/fuzz/corpus/alpha/external-seed"
if REPO_ROOT="${validation_fixture}" \
  "${validation_fixture}/scripts/run-fuzz-smoke.sh" --target alpha --dry-run \
  >"${work_dir}/symlink.out" 2>"${work_dir}/symlink.err"; then
  echo "symlinked fuzz corpus entry should fail" >&2
  exit 1
fi
grep -q 'fuzz smoke corpus contains a symlink: fuzz/corpus/alpha' \
  "${work_dir}/symlink.err"

make_fixture "${validation_fixture}"
cat >"${validation_fixture}/fake-bin/cargo" <<'EOF'
#!/usr/bin/env bash
exit 42
EOF
chmod +x "${validation_fixture}/fake-bin/cargo"
if PATH="${validation_fixture}/fake-bin:${PATH}" \
  REPO_ROOT="${validation_fixture}" \
  "${validation_fixture}/scripts/run-fuzz-smoke.sh" --target alpha \
  >"${work_dir}/failure.out" 2>"${work_dir}/failure.err"; then
  echo "failing fuzz target should fail the smoke runner" >&2
  exit 1
fi
