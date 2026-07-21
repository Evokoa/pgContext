#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TMPDIR="${TMPDIR:-${REPO_ROOT}/target/tmp}"
mkdir -p "${TMPDIR}"
work_dir="$(mktemp -d "${TMPDIR}/fuzz-campaign-test.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT
fake_bin="${work_dir}/bin"
fake_release_bin="${work_dir}/release-bin"
mkdir -p "${fake_bin}" "${fake_release_bin}"

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

printf '%s\n' "$*" >>"${FAKE_CARGO_LOG}"

if [[ "$*" == "-V" ]]; then
  printf 'cargo 1.96.0-test\n'
  exit 0
fi

if [[ "$*" == "+nightly fuzz --version" ]]; then
  printf 'cargo-fuzz 0.99.0-test\n'
  exit 0
fi

if [[ "${1:-}" == "+nightly" && "${2:-}" == "fuzz" && "${3:-}" == "run" ]]; then
  target="${4:-}"
  printf 'fake fuzz run for %s\n' "${target}"
  artifact_prefix=""
  for arg in "$@"; do
    case "${arg}" in
      -artifact_prefix=*)
        artifact_prefix="${arg#-artifact_prefix=}"
        ;;
    esac
  done
  if [[ "${FAKE_FUZZ_SYMLINK_ARTIFACT_TARGET:-}" == "${target}" ]]; then
    mkdir -p "${artifact_prefix}"
    ln -s "${artifact_prefix}/missing-${target}" "${artifact_prefix}/crash-${target}"
  fi
  if [[ "${FAKE_FUZZ_FAIL_TARGET:-}" == "${target}" ]]; then
    mkdir -p "${artifact_prefix}"
    printf 'crash artifact for %s\n' "${target}" >"${artifact_prefix}/crash-${target}"
    printf 'simulated fuzz failure for %s\n' "${target}" >&2
    exit 42
  fi
  exit 0
fi

printf 'unexpected cargo invocation: %s\n' "$*" >&2
exit 127
SH
chmod +x "${fake_bin}/cargo"

cp "${fake_bin}/cargo" "${fake_release_bin}/cargo"
cat >"${fake_release_bin}/date" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

state_file="${FAKE_DATE_STATE}"
if [[ "${1:-}" == "-u" && "${2:-}" == "+%s" ]]; then
  current="$(cat "${state_file}")"
  printf '%s\n' "${current}"
  printf '%s\n' "$((current + 86400))" >"${state_file}"
  exit 0
fi

if [[ "${1:-}" == "-u" && "${2:-}" == "-r" ]]; then
  epoch="${3:-0}"
  if [[ "${4:-}" == "+%Y-%m-%dT%H:%M:%SZ" ]]; then
    printf '2026-07-04T%02d:00:00Z\n' "$(((epoch / 3600) % 24))"
    exit 0
  fi
fi

if [[ "${1:-}" == "-u" && "${2:-}" == "+%Y-%m-%dT%H:%M:%SZ" ]]; then
  current="$(cat "${state_file}")"
  printf '2026-07-04T%02d:00:00Z\n' "$(((current / 3600) % 24))"
  exit 0
fi

exec /bin/date "$@"
SH
chmod +x "${fake_release_bin}/date"

"${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
  --dry-run \
  --duration 7 \
  --out-dir "${work_dir}/all"

summary="${work_dir}/all/summary.tsv"
report="${work_dir}/all/report.md"

assert_file_exists "${summary}"
assert_file_exists "${report}"
assert_summary_row_count "${summary}" "5"
head -n 1 "${summary}" | grep -q $'target\tstatus\texit_code\tstarted_utc\tfinished_utc\trequested_duration_seconds\telapsed_seconds\tartifact_count\tcorpus\tartifacts\tlog\tboundary\trelease_boundaries\tcommand\tlog_bytes'
grep -q -- '- Worktree: `' "${report}"
grep -q -- '- Rust: `' "${report}"
grep -q -- '- Cargo: `' "${report}"
grep -q -- '- Cargo fuzz: `not-run`' "${report}"
grep -q -- '- Required release duration: `86400` seconds per target' "${report}"
grep -q -- '- Max concurrent targets: `1`' "${report}"
grep -q -- '- Rows: `5`' "${report}"
grep -q -- '- Dry-run: `5`' "${report}"
grep -q -- '- Failed: `0`' "${report}"
grep -q -- '- Short elapsed rows: `0`' "${report}"
grep -q -- '- Rows with artifacts: `0`' "${report}"
grep -q -- '- Rows with corpus symlinks: `0`' "${report}"
grep -q -- '- Approval: `incomplete`' "${report}"
grep -q -- 'release fuzz evidence requires all default targets' "${report}"
grep -q '| JSONB path handling | `filter_json` |' "${report}"
grep -q '| mmap views | `segment_loader` |' "${report}"
grep -q $'filter_json\tdry-run\t0' "${summary}"
grep -q $'sql_predicate\tdry-run\t0' "${summary}"
grep -q $'vector_text\tdry-run\t0' "${summary}"
grep -q $'segment_loader\tdry-run\t0' "${summary}"
grep -q $'candidate_mask\tdry-run\t0' "${summary}"
grep -q 'JSONB path handling' "${report}"
grep -q 'mmap validation' "${report}"
grep -q 'artifacts/vector_text' "${summary}"

"${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
  --dry-run \
  --target vector_text \
  --duration 3 \
  --out-dir "${work_dir}/one"

assert_summary_row_count "${work_dir}/one/summary.tsv" "1"
grep -q $'vector_text\tdry-run\t0' "${work_dir}/one/summary.tsv"
grep -q -- '- Rows: `1`' "${work_dir}/one/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/one/report.md"

if "${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
  --dry-run \
  --target not_a_target \
  --out-dir "${work_dir}/bad-target" 2>"${work_dir}/bad-target.err"; then
  echo "unknown target should fail" >&2
  exit 1
fi
grep -q 'unknown fuzz target: not_a_target' "${work_dir}/bad-target.err"

if "${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
  --dry-run \
  --target vector_text \
  --target vector_text \
  --out-dir "${work_dir}/duplicate-target" 2>"${work_dir}/duplicate-target.err"; then
  echo "duplicate target should fail" >&2
  exit 1
fi
grep -q 'duplicate fuzz target: vector_text' "${work_dir}/duplicate-target.err"

if PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/dirty-cargo.log" \
  "${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
  --target vector_text \
  --duration 1 \
  --out-dir "${work_dir}/dirty" 2>"${work_dir}/dirty.err"; then
  echo "dirty execute run without override should fail" >&2
  exit 1
fi
grep -q 'dirty worktree cannot produce release fuzz evidence' "${work_dir}/dirty.err"

PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/success-cargo.log" \
  "${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
  --allow-dirty \
  --target vector_text \
  --duration 1 \
  --out-dir "${work_dir}/execute-success"

grep -q $'vector_text\tpassed\t0' "${work_dir}/execute-success/summary.tsv"
grep -q $'\t1\t' "${work_dir}/execute-success/summary.tsv"
grep -q -- '- Cargo fuzz: `cargo-fuzz 0.99.0-test`' "${work_dir}/execute-success/report.md"
grep -q -- '- Dirty override: `1`' "${work_dir}/execute-success/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/execute-success/report.md"
grep -q 'fake fuzz run for vector_text' "${work_dir}/execute-success/vector_text.log"
grep -q 'target: vector_text' "${work_dir}/execute-success/vector_text.log"
grep -q 'fuzz command output begin' "${work_dir}/execute-success/vector_text.log"
grep -q 'fuzz command output end' "${work_dir}/execute-success/vector_text.log"
grep -Eq '^fuzz raw output bytes: [1-9][0-9]*$' "${work_dir}/execute-success/vector_text.log"
grep -q 'fuzz_target_exercised: vector_text' "${work_dir}/execute-success/vector_text.log"
grep -q 'fuzz campaign status: passed' "${work_dir}/execute-success/vector_text.log"
awk -F '\t' '$1 == "vector_text" && $15 ~ /^[1-9][0-9]*$/ { found = 1 } END { exit(found ? 0 : 1) }' \
  "${work_dir}/execute-success/summary.tsv"
grep -q '+nightly fuzz run vector_text' "${work_dir}/success-cargo.log"

PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/symlink-artifact-cargo.log" FAKE_FUZZ_SYMLINK_ARTIFACT_TARGET=vector_text \
  "${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
  --allow-dirty \
  --target vector_text \
  --duration 1 \
  --out-dir "${work_dir}/execute-symlink-artifact"

grep -q $'vector_text\tpassed\t0' "${work_dir}/execute-symlink-artifact/summary.tsv"
grep -q -- '- Rows with artifacts: `1`' "${work_dir}/execute-symlink-artifact/report.md"
grep -q -- '- Rows with corpus symlinks: `0`' "${work_dir}/execute-symlink-artifact/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/execute-symlink-artifact/report.md"
grep -q 'artifact count: 1' "${work_dir}/execute-symlink-artifact/vector_text.log"
if [[ ! -L "${work_dir}/execute-symlink-artifact/artifacts/vector_text/crash-vector_text" ]]; then
  echo "expected symlink crash artifact fixture" >&2
  exit 1
fi

PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/short-cargo.log" \
  "${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
  --allow-dirty \
  --target vector_text \
  --duration 86400 \
  --out-dir "${work_dir}/execute-short"

grep -q $'vector_text\tpassed\t0' "${work_dir}/execute-short/summary.tsv"
grep -q -- '- Short elapsed rows: `1`' "${work_dir}/execute-short/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/execute-short/report.md"

PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/parallel-cargo.log" \
  "${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
  --allow-dirty \
  --jobs 3 \
  --duration 1 \
  --out-dir "${work_dir}/execute-parallel"

grep -q -- '- Max concurrent targets: `3`' "${work_dir}/execute-parallel/report.md"
grep -q -- '- Rows: `5`' "${work_dir}/execute-parallel/report.md"
grep -q -- '- Passed: `5`' "${work_dir}/execute-parallel/report.md"
grep -q -- '- Failed: `0`' "${work_dir}/execute-parallel/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/execute-parallel/report.md"
for target in filter_json sql_predicate vector_text segment_loader candidate_mask; do
  grep -q "^${target}"$'\tpassed\t0' "${work_dir}/execute-parallel/summary.tsv"
  grep -q "fuzz_target_exercised: ${target}" "${work_dir}/execute-parallel/${target}.log"
  grep -q "+nightly fuzz run ${target}" "${work_dir}/parallel-cargo.log"
done

if PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/failure-cargo.log" FAKE_FUZZ_FAIL_TARGET=candidate_mask \
  "${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
  --allow-dirty \
  --jobs 2 \
  --target candidate_mask \
  --duration 1 \
  --out-dir "${work_dir}/execute-failure"; then
  echo "failing fuzz target should fail the runner" >&2
  exit 1
fi
grep -q $'candidate_mask\tfailed\t42' "${work_dir}/execute-failure/summary.tsv"
grep -q -- '- Failed: `1`' "${work_dir}/execute-failure/report.md"
grep -q -- '- Rows with artifacts: `1`' "${work_dir}/execute-failure/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/execute-failure/report.md"
grep -q 'simulated fuzz failure for candidate_mask' "${work_dir}/execute-failure/candidate_mask.log"
grep -q 'fuzz command output begin' "${work_dir}/execute-failure/candidate_mask.log"
grep -q 'fuzz command output end' "${work_dir}/execute-failure/candidate_mask.log"
grep -Eq '^fuzz raw output bytes: [1-9][0-9]*$' "${work_dir}/execute-failure/candidate_mask.log"
grep -q 'fuzz campaign status: failed' "${work_dir}/execute-failure/candidate_mask.log"
grep -q 'fuzz campaign exit code: 42' "${work_dir}/execute-failure/candidate_mask.log"
assert_file_exists "${work_dir}/execute-failure/artifacts/candidate_mask/crash-candidate_mask"

if PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/parallel-failure-cargo.log" FAKE_FUZZ_FAIL_TARGET=segment_loader \
  "${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
  --allow-dirty \
  --jobs 3 \
  --duration 1 \
  --out-dir "${work_dir}/execute-parallel-failure"; then
  echo "one failing parallel fuzz target should fail the runner" >&2
  exit 1
fi
grep -q $'segment_loader\tfailed\t42' "${work_dir}/execute-parallel-failure/summary.tsv"
grep -q $'filter_json\tpassed\t0' "${work_dir}/execute-parallel-failure/summary.tsv"
grep -q $'candidate_mask\tpassed\t0' "${work_dir}/execute-parallel-failure/summary.tsv"
grep -q -- '- Rows: `5`' "${work_dir}/execute-parallel-failure/report.md"
grep -q -- '- Passed: `4`' "${work_dir}/execute-parallel-failure/report.md"
grep -q -- '- Failed: `1`' "${work_dir}/execute-parallel-failure/report.md"
grep -q -- '- Rows with artifacts: `1`' "${work_dir}/execute-parallel-failure/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/execute-parallel-failure/report.md"
grep -q 'fuzz campaign status: failed' "${work_dir}/execute-parallel-failure/segment_loader.log"
assert_file_exists "${work_dir}/execute-parallel-failure/artifacts/segment_loader/crash-segment_loader"

release_root="${work_dir}/release-root"
mkdir -p "${release_root}/fuzz/fuzz_targets" "${release_root}/fuzz/corpus"
for target in filter_json sql_predicate vector_text segment_loader candidate_mask; do
  printf 'fuzz target %s\n' "${target}" >"${release_root}/fuzz/fuzz_targets/${target}.rs"
  mkdir -p "${release_root}/fuzz/corpus/${target}"
  printf 'seed %s\n' "${target}" >"${release_root}/fuzz/corpus/${target}/seed"
done
printf 'target/\n' >"${release_root}/.gitignore"
git -C "${release_root}" init -q
git -C "${release_root}" add .
git -C "${release_root}" \
  -c user.name='Fuzz Test' \
  -c user.email='fuzz-test@example.invalid' \
  commit -q -m 'initial clean fuzz fixture'
printf '1000000\n' >"${work_dir}/fake-date-state"

PATH="${fake_release_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/release-cargo.log" \
  FAKE_DATE_STATE="${work_dir}/fake-date-state" \
  REPO_ROOT="${release_root}" \
  "${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
    --out-dir "${release_root}/target/fuzz-campaigns/release-complete"
release_complete_report="${release_root}/target/fuzz-campaigns/release-complete/report.md"
release_complete_summary="${release_root}/target/fuzz-campaigns/release-complete/summary.tsv"
grep -q -- '- Worktree: `clean`' "${release_complete_report}"
grep -q -- '- Evidence directory: `repo`' "${release_complete_report}"
grep -q -- '- Per-target duration: `86400` seconds' "${release_complete_report}"
grep -q -- '- Rows: `5`' "${release_complete_report}"
grep -q -- '- Passed: `5`' "${release_complete_report}"
grep -q -- '- Short elapsed rows: `0`' "${release_complete_report}"
grep -q -- '- Rows with artifacts: `0`' "${release_complete_report}"
grep -q -- '- Rows with corpus symlinks: `0`' "${release_complete_report}"
grep -q -- '- Approval: `complete`' "${release_complete_report}"
awk -F '\t' 'NR > 1 && $7 < 86340 { exit 1 }' "${release_complete_summary}"
awk -F '\t' 'NR > 1 && $15 !~ /^[1-9][0-9]*$/ { exit 1 }' "${release_complete_summary}"
grep -q 'fuzz_target_exercised: filter_json' "${release_root}/target/fuzz-campaigns/release-complete/filter_json.log"
grep -q 'fuzz command output begin' "${release_root}/target/fuzz-campaigns/release-complete/filter_json.log"
grep -q 'fuzz command output end' "${release_root}/target/fuzz-campaigns/release-complete/filter_json.log"
grep -Eq '^fuzz raw output bytes: [1-9][0-9]*$' "${release_root}/target/fuzz-campaigns/release-complete/filter_json.log"
grep -q 'fuzz campaign status: passed' "${release_root}/target/fuzz-campaigns/release-complete/candidate_mask.log"
grep -q '+nightly fuzz run filter_json' "${work_dir}/release-cargo.log"
grep -q '+nightly fuzz run candidate_mask' "${work_dir}/release-cargo.log"

printf '2000000\n' >"${work_dir}/fake-date-state"
PATH="${fake_release_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/external-release-cargo.log" \
  FAKE_DATE_STATE="${work_dir}/fake-date-state" \
  REPO_ROOT="${release_root}" \
  "${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
    --out-dir "${work_dir}/external-release-complete"
grep -q -- '- Worktree: `clean`' "${work_dir}/external-release-complete/report.md"
grep -q -- '- Evidence directory: `external`' "${work_dir}/external-release-complete/report.md"
grep -q -- '- Rows: `5`' "${work_dir}/external-release-complete/report.md"
grep -q -- '- Passed: `5`' "${work_dir}/external-release-complete/report.md"
grep -q -- '- Short elapsed rows: `0`' "${work_dir}/external-release-complete/report.md"
grep -q -- '- Rows with artifacts: `0`' "${work_dir}/external-release-complete/report.md"
grep -q -- '- Rows with corpus symlinks: `0`' "${work_dir}/external-release-complete/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/external-release-complete/report.md"

release_symlink_corpus_root="${work_dir}/release-symlink-corpus-root"
mkdir -p "${release_symlink_corpus_root}/fuzz/fuzz_targets" "${release_symlink_corpus_root}/fuzz/corpus"
for target in filter_json sql_predicate vector_text segment_loader candidate_mask; do
  printf 'fuzz target %s\n' "${target}" >"${release_symlink_corpus_root}/fuzz/fuzz_targets/${target}.rs"
  mkdir -p "${release_symlink_corpus_root}/fuzz/corpus/${target}"
  printf 'seed %s\n' "${target}" >"${release_symlink_corpus_root}/fuzz/corpus/${target}/seed"
done
ln -s "${work_dir}/external-seed" "${release_symlink_corpus_root}/fuzz/corpus/vector_text/external-seed"
printf 'target/\n' >"${release_symlink_corpus_root}/.gitignore"
git -C "${release_symlink_corpus_root}" init -q
git -C "${release_symlink_corpus_root}" add .
git -C "${release_symlink_corpus_root}" \
  -c user.name='Fuzz Test' \
  -c user.email='fuzz-test@example.invalid' \
  commit -q -m 'initial symlink corpus fixture'
printf '2500000\n' >"${work_dir}/fake-date-state"
PATH="${fake_release_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/symlink-corpus-release-cargo.log" \
  FAKE_DATE_STATE="${work_dir}/fake-date-state" \
  REPO_ROOT="${release_symlink_corpus_root}" \
  "${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
    --out-dir "${release_symlink_corpus_root}/target/fuzz-campaigns/symlink-corpus"
grep -q -- '- Worktree: `clean`' "${release_symlink_corpus_root}/target/fuzz-campaigns/symlink-corpus/report.md"
grep -q -- '- Evidence directory: `repo`' "${release_symlink_corpus_root}/target/fuzz-campaigns/symlink-corpus/report.md"
grep -q -- '- Rows: `5`' "${release_symlink_corpus_root}/target/fuzz-campaigns/symlink-corpus/report.md"
grep -q -- '- Passed: `5`' "${release_symlink_corpus_root}/target/fuzz-campaigns/symlink-corpus/report.md"
grep -q -- '- Rows with corpus symlinks: `1`' "${release_symlink_corpus_root}/target/fuzz-campaigns/symlink-corpus/report.md"
grep -q -- '- Approval: `incomplete`' "${release_symlink_corpus_root}/target/fuzz-campaigns/symlink-corpus/report.md"
grep -q 'corpus symlink count: 1' "${release_symlink_corpus_root}/target/fuzz-campaigns/symlink-corpus/vector_text.log"

release_root_link="${work_dir}/release-root-link"
ln -s "${release_root}" "${release_root_link}"
printf '3000000\n' >"${work_dir}/fake-date-state"
PATH="${fake_release_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/symlink-root-release-cargo.log" \
  FAKE_DATE_STATE="${work_dir}/fake-date-state" \
  REPO_ROOT="${release_root_link}" \
  "${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
    --out-dir "${release_root}/target/fuzz-campaigns/symlink-root-complete"
symlink_root_report="${release_root}/target/fuzz-campaigns/symlink-root-complete/report.md"
symlink_root_summary="${release_root}/target/fuzz-campaigns/symlink-root-complete/summary.tsv"
grep -q -- '- Evidence directory: `repo`' "${symlink_root_report}"
grep -q -- '- Rows with corpus symlinks: `0`' "${symlink_root_report}"
grep -q -- '- Approval: `complete`' "${symlink_root_report}"
if grep -qF -- "${release_root}" "${symlink_root_report}" "${symlink_root_summary}" ||
  grep -qF -- "${release_root_link}" "${symlink_root_report}" "${symlink_root_summary}"; then
  echo "symlink-root fuzz evidence paths should be repo-relative" >&2
  exit 1
fi

if "${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
  --dry-run \
  --duration 0 \
  --out-dir "${work_dir}/bad-duration" 2>"${work_dir}/bad-duration.err"; then
  echo "zero duration should fail" >&2
  exit 1
fi
grep -q -- '--duration must be a positive integer' "${work_dir}/bad-duration.err"

if "${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
  --dry-run \
  --jobs 0 \
  --out-dir "${work_dir}/bad-jobs" 2>"${work_dir}/bad-jobs.err"; then
  echo "zero jobs should fail" >&2
  exit 1
fi
grep -q -- '--jobs must be a positive integer' "${work_dir}/bad-jobs.err"

if "${REPO_ROOT}/scripts/run-fuzz-campaigns.sh" \
  --dry-run \
  --out-dir / 2>"${work_dir}/bad-out-dir.err"; then
  echo "root out-dir should fail" >&2
  exit 1
fi
grep -q -- '--out-dir must be a non-root path' "${work_dir}/bad-out-dir.err"
