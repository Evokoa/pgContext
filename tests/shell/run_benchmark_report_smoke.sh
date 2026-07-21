#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TMPDIR="${TMPDIR:-${REPO_ROOT}/target/tmp}"
mkdir -p "${TMPDIR}"
work_dir="$(mktemp -d "${TMPDIR}/benchmark-report-test.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT
fake_bin="${work_dir}/bin"
mkdir -p "${fake_bin}"

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

if [[ "${1:-}" == "bench" && "${2:-}" == "-p" && "${3:-}" == "context-test" && "${4:-}" == "--bench" ]]; then
  bench="${5:-}"
  if [[ "${FAKE_BENCH_FAIL_TARGET:-}" == "${bench}" ]]; then
    printf 'simulated benchmark failure for %s\n' "${bench}" >&2
    exit 31
  fi
  if [[ "${FAKE_BENCH_BAD_LOG_TARGET:-}" == "${bench}" ]]; then
    printf 'malformed benchmark evidence for %s\n' "${bench}"
    exit 0
  fi
  case "${bench}" in
    exact_search_baseline)
      printf 'dataset=Small rows=1000 dimensions=32 seed=0x706763745f736d6c vector_bytes=128000 build_ms=1 search_ms=1 top_point_id=42\n'
      printf 'dataset=Medium rows=100000 dimensions=64 seed=0x706763745f6d6564 vector_bytes=25600000 build_ms=2 search_ms=2 top_point_id=4242\n'
      ;;
    hnsw_baseline)
      printf 'dataset=Small rows=1000 dimensions=32 m=8 ef_construction=32 ef_search=16 build_ms=1 search_ms=1 vector_bytes=128000 graph_bytes=160000 bytes_per_vector=160 recall=0.900000 intersection=9 exact_count=10 candidate_count=10\n'
      printf 'dataset=Small rows=1000 dimensions=32 m=16 ef_construction=64 ef_search=32 build_ms=1 search_ms=1 vector_bytes=128000 graph_bytes=192000 bytes_per_vector=192 recall=0.950000 intersection=10 exact_count=10 candidate_count=10\n'
      printf 'dataset=Small rows=1000 dimensions=32 m=32 ef_construction=128 ef_search=64 build_ms=1 search_ms=1 vector_bytes=128000 graph_bytes=256000 bytes_per_vector=256 recall=1.000000 intersection=10 exact_count=10 candidate_count=10\n'
      ;;
    quantized_baseline)
      printf 'dataset=Small mode=binary rows=1000 dimensions=32 candidate_budget=64 codebook_bytes=0 elapsed_ms=1 recall=0.800000 intersection=8 exact_count=10 candidate_count=10\n'
      printf 'dataset=Small mode=scalar_sq8 rows=1000 dimensions=32 candidate_budget=64 codebook_bytes=1024 elapsed_ms=1 recall=0.950000 intersection=10 exact_count=10 candidate_count=10\n'
      printf 'dataset=Small mode=product_quantized rows=1000 dimensions=32 candidate_budget=64 codebook_bytes=2048 elapsed_ms=1 recall=0.950000 intersection=10 exact_count=10 candidate_count=10\n'
      ;;
    filtered_ann_baseline)
      printf 'dataset=Small filter=narrow rows=1000 allowed=100 survival_rate=0.100000 filter_ms=1 bitmap_bytes=128 search_ms=1 recall=0.950000 intersection=10 exact_count=10 candidate_count=10\n'
      printf 'dataset=Small filter=medium rows=1000 allowed=500 survival_rate=0.500000 filter_ms=1 bitmap_bytes=128 search_ms=1 recall=0.950000 intersection=10 exact_count=10 candidate_count=10\n'
      printf 'dataset=Small filter=broad rows=1000 allowed=900 survival_rate=0.900000 filter_ms=1 bitmap_bytes=128 search_ms=1 recall=0.950000 intersection=10 exact_count=10 candidate_count=10\n'
      printf 'dataset=Small filter=empty rows=1000 allowed=0 survival_rate=0.000000 filter_ms=1 bitmap_bytes=128 search_ms=1 recall=1.000000 intersection=0 exact_count=0 candidate_count=0\n'
      ;;
    hybrid_baseline)
      printf 'dataset=Small case=dense_only branches=1 non_empty_branches=1 input_candidates=64 output=10 elapsed_ns=1000 top_point_id=10\n'
      printf 'dataset=Small case=text_only branches=1 non_empty_branches=1 input_candidates=64 output=10 elapsed_ns=1000 top_point_id=11\n'
      printf 'dataset=Small case=fused_dense_text branches=2 non_empty_branches=2 input_candidates=128 output=10 elapsed_ns=1000 top_point_id=12\n'
      printf 'dataset=Small case=fully_empty branches=1 non_empty_branches=0 input_candidates=0 output=0 elapsed_ns=100 top_point_id=0\n'
      ;;
    late_interaction_ann_baseline)
      if [[ "${FAKE_ZERO_LATE_TIMING:-}" == "1" ]]; then
        printf 'dataset=Small points=1000 vectors_per_point=2 token_vectors=2000 candidates_per_query=64 candidate_source_keys=100 output=10 exact_ns=0 ann_candidate_ns=2000 rerank_ns=3000 vector_bytes=256000 token_graph_bytes=512000 bytes_per_token_vector=256 projected_comparisons=400 recall=1.000000 exact_top_point_id=5 ann_top_point_id=5\n'
      else
        printf 'dataset=Small points=1000 vectors_per_point=2 token_vectors=2000 candidates_per_query=64 candidate_source_keys=100 output=10 exact_ns=1000 ann_candidate_ns=2000 rerank_ns=3000 vector_bytes=256000 token_graph_bytes=512000 bytes_per_token_vector=256 projected_comparisons=400 recall=1.000000 exact_top_point_id=5 ann_top_point_id=5\n'
      fi
      ;;
    *)
      printf 'unexpected benchmark target: %s\n' "${bench}" >&2
      exit 127
      ;;
  esac
  exit 0
fi

if [[ "${1:-}" == "test" && "${2:-}" == "-p" && "${3:-}" == "context-test" && "${4:-}" == "--test" ]]; then
  test_name="${5:-}"
  if [[ "${FAKE_BENCH_FAIL_TARGET:-}" == "${test_name}" ]]; then
    printf 'simulated recall failure for %s\n' "${test_name}" >&2
    exit 32
  fi
  printf 'fake recall run for %s\n' "${test_name}"
  if [[ "${FAKE_BAD_RECALL_LOG:-}" != "1" ]]; then
    if [[ "${FAKE_LOW_RECALL_LOG:-}" == "1" ]]; then
      printf 'recall_gate name=hnsw_m32_ef64 recall=0.900000 min=0.950000 intersection=9 exact_count=10 candidate_count=10\n'
    else
      printf 'recall_gate name=hnsw_m32_ef64 recall=1.000000 min=0.950000 intersection=10 exact_count=10 candidate_count=10\n'
    fi
    printf 'recall_gate name=scalar_sq8_rerank recall=1.000000 min=0.950000 intersection=10 exact_count=10 candidate_count=10\n'
    printf 'recall_gate name=binary_rerank recall=0.800000 min=0.750000 intersection=8 exact_count=10 candidate_count=10\n'
    printf 'recall_gate name=late_interaction_ann_rerank recall=1.000000 min=0.950000 intersection=10 exact_count=10 candidate_count=10\n'
  fi
  exit 0
fi

printf 'unexpected cargo invocation: %s\n' "$*" >&2
exit 127
SH
chmod +x "${fake_bin}/cargo"

"${REPO_ROOT}/scripts/run-benchmark-report.sh" \
  --dry-run \
  --baseline pre-release-candidate \
  --samples "smoke sample settings" \
  --postgres "PostgreSQL 17 smoke" \
  --features "pg17 smoke features" \
  --waiver "none for smoke" \
  --out-dir "${work_dir}/all"

summary="${work_dir}/all/summary.tsv"
report="${work_dir}/all/report.md"

assert_file_exists "${summary}"
assert_file_exists "${report}"
assert_summary_row_count "${summary}" "7"
head -n 1 "${summary}" | grep -q $'target\tkind\tstatus\texit_code\tstarted_utc\tfinished_utc\tlog\tboundary\tcommand'
grep -q -- '- Worktree: `' "${report}"
grep -q -- '- CPU: `' "${report}"
grep -q -- '- PostgreSQL: `PostgreSQL 17 smoke`' "${report}"
grep -q -- '- Feature flags: `pg17 smoke features`' "${report}"
grep -q -- '- Baseline: `pre-release-candidate`' "${report}"
grep -q -- '- Warmup/sample settings: `smoke sample settings`' "${report}"
grep -q -- '- Threshold waiver note: `none for smoke`' "${report}"
grep -q -- '- Rows: `7`' "${report}"
grep -q -- '- Dry-run: `7`' "${report}"
grep -q -- '- Failed: `0`' "${report}"
grep -q -- '- Approval: `incomplete`' "${report}"
grep -q 'release benchmark evidence requires all default benchmarks' "${report}"
grep -q $'exact_search_baseline\tbenchmark\tdry-run\t0' "${summary}"
grep -q $'hnsw_baseline\tbenchmark\tdry-run\t0' "${summary}"
grep -q $'quantized_baseline\tbenchmark\tdry-run\t0' "${summary}"
grep -q $'filtered_ann_baseline\tbenchmark\tdry-run\t0' "${summary}"
grep -q $'hybrid_baseline\tbenchmark\tdry-run\t0' "${summary}"
grep -q $'late_interaction_ann_baseline\tbenchmark\tdry-run\t0' "${summary}"
grep -q $'recall_gates\trecall\tdry-run\t0' "${summary}"
grep -q 'exact-search latency, memory, and top-k baseline' "${report}"
grep -q 'late-interaction ANN candidate latency, memory, and exact MaxSim recall' "${report}"
grep -q 'release recall thresholds for HNSW, quantized, and late-interaction candidates' "${report}"

"${REPO_ROOT}/scripts/run-benchmark-report.sh" \
  --dry-run \
  --bench hybrid_baseline \
  --no-recall \
  --out-dir "${work_dir}/one"

assert_summary_row_count "${work_dir}/one/summary.tsv" "1"
grep -q $'hybrid_baseline\tbenchmark\tdry-run\t0' "${work_dir}/one/summary.tsv"
grep -q -- '- Rows: `1`' "${work_dir}/one/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/one/report.md"

PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/execute-cargo.log" \
  "${REPO_ROOT}/scripts/run-benchmark-report.sh" \
  --baseline pre-release-candidate \
  --samples "fake execute sample settings" \
  --postgres "PostgreSQL 17 fake" \
  --features "pg17 fake features" \
  --waiver "none" \
  --out-dir "${work_dir}/execute-success"

grep -q -- '- Cargo: `cargo 1.96.0-test`' "${work_dir}/execute-success/report.md"
grep -q -- '- Rows: `7`' "${work_dir}/execute-success/report.md"
grep -q -- '- Passed: `7`' "${work_dir}/execute-success/report.md"
grep -q -- '- Dry-run: `0`' "${work_dir}/execute-success/report.md"
grep -q -- '- Failed: `0`' "${work_dir}/execute-success/report.md"
if [[ -z "$(git -C "${REPO_ROOT}" status --short)" ]]; then
  grep -q -- '- Worktree: `clean`' "${work_dir}/execute-success/report.md"
  grep -q -- '- Approval: `complete`' "${work_dir}/execute-success/report.md"
else
  grep -q -- '- Worktree: `dirty`' "${work_dir}/execute-success/report.md"
  grep -q -- '- Approval: `incomplete`' "${work_dir}/execute-success/report.md"
fi
grep -q 'dataset=Small rows=1000 dimensions=32 seed=0x706763745f736d6c' "${work_dir}/execute-success/exact_search_baseline.log"
grep -q 'late_interaction_ann_baseline' "${work_dir}/execute-cargo.log"
grep -q 'exact_ns=1000 ann_candidate_ns=2000 rerank_ns=3000' "${work_dir}/execute-success/late_interaction_ann_baseline.log"
grep -q 'fake recall run for recall_gates' "${work_dir}/execute-success/recall_gates.log"
grep -q 'recall_gate name=hnsw_m32_ef64 recall=1.000000' "${work_dir}/execute-success/recall_gates.log"
grep -q 'recall_gate name=late_interaction_ann_rerank recall=1.000000' "${work_dir}/execute-success/recall_gates.log"
grep -q -- '--bench exact_search_baseline' "${work_dir}/execute-cargo.log"
grep -q -- '--bench late_interaction_ann_baseline' "${work_dir}/execute-cargo.log"
grep -q -- '--test recall_gates -- --nocapture' "${work_dir}/execute-cargo.log"

repo_local_out="${REPO_ROOT}/target/tmp/benchmark-report-repo-local"
rm -rf "${repo_local_out}"
PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/repo-local-cargo.log" \
  "${REPO_ROOT}/scripts/run-benchmark-report.sh" \
  --baseline pre-release-candidate \
  --bench late_interaction_ann_baseline \
  --no-recall \
  --out-dir "${repo_local_out}"
repo_local_summary="${repo_local_out}/summary.tsv"
repo_local_report="${repo_local_out}/report.md"
grep -q 'See `target/tmp/benchmark-report-repo-local/summary.tsv`.' \
  "${repo_local_report}"
if grep -qF -- "${REPO_ROOT}" "${repo_local_summary}" "${repo_local_report}"; then
  echo "repo-local benchmark evidence paths should be repo-relative" >&2
  exit 1
fi
awk -F '\t' '
  NR > 1 && ($7 ~ /^\// || $9 !~ /^cargo bench -p context-test --bench late_interaction_ann_baseline$/) { bad = 1 }
  END { exit(bad ? 1 : 0) }
' "${repo_local_summary}"

clean_repo="${work_dir}/clean-repo"
mkdir -p "${clean_repo}"
git -C "${clean_repo}" init -q
PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/clean-repo-cargo.log" \
  REPO_ROOT="${clean_repo}" \
  "${REPO_ROOT}/scripts/run-benchmark-report.sh" \
  --baseline pre-release-candidate \
  --out-dir "${clean_repo}/target/benchmark-report-clean-repo"
clean_repo_report="${clean_repo}/target/benchmark-report-clean-repo/report.md"
clean_repo_summary="${clean_repo}/target/benchmark-report-clean-repo/summary.tsv"
grep -q -- '- Worktree: `clean`' "${clean_repo_report}"
grep -q -- '- Approval: `complete`' "${clean_repo_report}"
grep -q 'See `target/benchmark-report-clean-repo/summary.tsv`.' \
  "${clean_repo_report}"
if grep -qF -- "${clean_repo}" "${clean_repo_summary}" "${clean_repo_report}"; then
  echo "clean repo benchmark evidence paths should be repo-relative" >&2
  exit 1
fi

if PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/failure-cargo.log" FAKE_BENCH_FAIL_TARGET=hnsw_baseline \
  "${REPO_ROOT}/scripts/run-benchmark-report.sh" \
  --baseline pre-release-candidate \
  --out-dir "${work_dir}/execute-failure"; then
  echo "failing benchmark target should fail the runner" >&2
  exit 1
fi
grep -q $'hnsw_baseline\tbenchmark\tfailed\t31' "${work_dir}/execute-failure/summary.tsv"
grep -q -- '- Failed: `1`' "${work_dir}/execute-failure/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/execute-failure/report.md"
grep -q 'simulated benchmark failure for hnsw_baseline' "${work_dir}/execute-failure/hnsw_baseline.log"

if PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/bad-log-cargo.log" FAKE_BENCH_BAD_LOG_TARGET=quantized_baseline \
  "${REPO_ROOT}/scripts/run-benchmark-report.sh" \
  --baseline pre-release-candidate \
  --out-dir "${work_dir}/execute-bad-log"; then
  echo "malformed benchmark evidence should fail the runner" >&2
  exit 1
fi
grep -q $'quantized_baseline\tbenchmark\tfailed\t65' "${work_dir}/execute-bad-log/summary.tsv"
grep -q 'benchmark evidence validation failed for quantized_baseline' "${work_dir}/execute-bad-log/quantized_baseline.log"

if PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/zero-late-cargo.log" FAKE_ZERO_LATE_TIMING=1 \
  "${REPO_ROOT}/scripts/run-benchmark-report.sh" \
  --baseline pre-release-candidate \
  --bench late_interaction_ann_baseline \
  --no-recall \
  --out-dir "${work_dir}/execute-zero-late"; then
  echo "zero late-interaction timing evidence should fail the runner" >&2
  exit 1
fi
grep -q $'late_interaction_ann_baseline\tbenchmark\tfailed\t65' "${work_dir}/execute-zero-late/summary.tsv"
grep -q 'benchmark evidence validation failed for late_interaction_ann_baseline' "${work_dir}/execute-zero-late/late_interaction_ann_baseline.log"

if PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/bad-recall-cargo.log" FAKE_BAD_RECALL_LOG=1 \
  "${REPO_ROOT}/scripts/run-benchmark-report.sh" \
  --baseline pre-release-candidate \
  --bench exact_search_baseline \
  --out-dir "${work_dir}/execute-bad-recall"; then
  echo "malformed recall evidence should fail the runner" >&2
  exit 1
fi
grep -q $'recall_gates\trecall\tfailed\t65' "${work_dir}/execute-bad-recall/summary.tsv"
grep -q 'recall evidence validation failed for recall_gates' "${work_dir}/execute-bad-recall/recall_gates.log"

if PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/low-recall-cargo.log" FAKE_LOW_RECALL_LOG=1 \
  "${REPO_ROOT}/scripts/run-benchmark-report.sh" \
  --baseline pre-release-candidate \
  --bench exact_search_baseline \
  --out-dir "${work_dir}/execute-low-recall"; then
  echo "below-threshold recall evidence should fail the runner" >&2
  exit 1
fi
grep -q $'recall_gates\trecall\tfailed\t65' "${work_dir}/execute-low-recall/summary.tsv"
grep -q 'recall_gate name=hnsw_m32_ef64 recall=0.900000 min=0.950000' "${work_dir}/execute-low-recall/recall_gates.log"
grep -q 'recall evidence validation failed for recall_gates' "${work_dir}/execute-low-recall/recall_gates.log"

if "${REPO_ROOT}/scripts/run-benchmark-report.sh" \
  --dry-run \
  --bench not_a_bench \
  --out-dir "${work_dir}/bad-bench" 2>"${work_dir}/bad-bench.err"; then
  echo "unknown benchmark should fail" >&2
  exit 1
fi
grep -q 'unknown benchmark: not_a_bench' "${work_dir}/bad-bench.err"

if "${REPO_ROOT}/scripts/run-benchmark-report.sh" \
  --dry-run \
  --out-dir / 2>"${work_dir}/bad-out-dir.err"; then
  echo "root out-dir should fail" >&2
  exit 1
fi
grep -q -- '--out-dir must be a non-root path' "${work_dir}/bad-out-dir.err"
