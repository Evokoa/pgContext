#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
BENCHMARKS=(
  exact_search_baseline
  hnsw_baseline
  quantized_baseline
  filtered_ann_baseline
  hybrid_baseline
  late_interaction_ann_baseline
)
RECALL_GATE="recall_gates"

out_dir="${BENCHMARK_REPORT_DIR:-}"
out_dir_explicit=0
dry_run=0
baseline="${BENCHMARK_BASELINE:-unset}"
sample_settings="${BENCHMARK_SAMPLE_SETTINGS:-harness=false bounded runners}"
postgres_version="${BENCHMARK_POSTGRES_VERSION:-not-applicable}"
feature_flags="${BENCHMARK_FEATURE_FLAGS:-default context-test bench profile}"
waiver_note="${BENCHMARK_WAIVER_NOTE:-none}"
selected=()
include_recall=1

usage() {
  cat <<'USAGE'
Usage: scripts/run-benchmark-report.sh [options]

Run release benchmark and recall commands, writing auditable logs.

Options:
  --bench NAME       Run one benchmark. May be repeated. Defaults to all.
  --no-recall        Do not run the context-test recall gate.
  --baseline NAME    Baseline commit or release candidate label.
  --samples TEXT     Warmup/sample settings note for the report.
  --postgres TEXT    PostgreSQL version note for the report.
  --features TEXT    Feature flags note for the report.
  --waiver TEXT      Threshold waiver note. Defaults to none.
  --out-dir PATH     Report/log directory. Defaults under target/benchmark-reports.
  --dry-run          Write the report plan without executing cargo commands.
  -h, --help         Show this help text.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bench)
      [[ $# -ge 2 ]] || {
        echo "--bench requires a value" >&2
        exit 2
      }
      selected+=("$2")
      shift 2
      ;;
    --no-recall)
      include_recall=0
      shift
      ;;
    --baseline)
      [[ $# -ge 2 ]] || {
        echo "--baseline requires a value" >&2
        exit 2
      }
      baseline="$2"
      shift 2
      ;;
    --samples)
      [[ $# -ge 2 ]] || {
        echo "--samples requires a value" >&2
        exit 2
      }
      sample_settings="$2"
      shift 2
      ;;
    --postgres)
      [[ $# -ge 2 ]] || {
        echo "--postgres requires a value" >&2
        exit 2
      }
      postgres_version="$2"
      shift 2
      ;;
    --features)
      [[ $# -ge 2 ]] || {
        echo "--features requires a value" >&2
        exit 2
      }
      feature_flags="$2"
      shift 2
      ;;
    --waiver)
      [[ $# -ge 2 ]] || {
        echo "--waiver requires a value" >&2
        exit 2
      }
      waiver_note="$2"
      shift 2
      ;;
    --out-dir)
      [[ $# -ge 2 ]] || {
        echo "--out-dir requires a value" >&2
        exit 2
      }
      out_dir="$2"
      out_dir_explicit=1
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

# Resolved after argument parsing so the HEAD lookup only runs when no
# explicit destination was given: callers that pass --out-dir must work in a
# repository without commits (the smoke suite drives a freshly `git init`ed
# fixture), where `rev-parse HEAD` fails.
if [[ "${out_dir_explicit}" -eq 0 && -z "${out_dir}" ]]; then
  out_dir="${REPO_ROOT}/target/benchmark-reports/$(git -C "${REPO_ROOT}" rev-parse HEAD)"
fi
if [[ -z "${out_dir}" || "${out_dir}" == "/" ]]; then
  echo "--out-dir must be a non-root path" >&2
  exit 2
fi
if [[ "${out_dir}" != /* ]]; then
  out_dir="${REPO_ROOT}/${out_dir}"
fi

if [[ ${#selected[@]} -eq 0 ]]; then
  selected=("${BENCHMARKS[@]}")
fi

known_benchmark() {
  local candidate="$1"
  local known
  for known in "${BENCHMARKS[@]}"; do
    [[ "${candidate}" == "${known}" ]] && return 0
  done
  return 1
}

benchmark_boundary() {
  case "$1" in
    exact_search_baseline)
      printf '%s\n' "exact-search latency, memory, and top-k baseline"
      ;;
    hnsw_baseline)
      printf '%s\n' "HNSW build/search latency, memory, and exact recall"
      ;;
    quantized_baseline)
      printf '%s\n' "quantized candidate latency, codebook bytes, and reranked recall"
      ;;
    filtered_ann_baseline)
      printf '%s\n' "filtered ANN selectivity, bitmap memory, search time, and recall"
      ;;
    hybrid_baseline)
      printf '%s\n' "hybrid dense/text/sparse branch fan-in and fused output"
      ;;
    late_interaction_ann_baseline)
      printf '%s\n' "late-interaction ANN candidate latency, memory, and exact MaxSim recall"
      ;;
    recall_gates)
      printf '%s\n' "release recall thresholds for HNSW, quantized, and late-interaction candidates"
      ;;
    *)
      printf '%s\n' "unknown"
      ;;
  esac
}

repo_relative_path() {
  local path="$1"

  case "${path}" in
    "${REPO_ROOT}"/*) printf '%s\n' "${path#"${REPO_ROOT}/"}" ;;
    *) printf '%s\n' "${path}" ;;
  esac
}

validate_benchmark_log() {
  local target="$1"
  local log_file="$2"

  case "${target}" in
    exact_search_baseline)
      grep -Eq 'dataset=Small rows=[1-9][0-9]* dimensions=[1-9][0-9]* seed=0x[0-9a-f]+ vector_bytes=[1-9][0-9]* build_ms=[0-9]+ search_ms=[0-9]+ top_point_id=[1-9][0-9]*' "${log_file}" &&
        grep -Eq 'dataset=Medium rows=[1-9][0-9]* dimensions=[1-9][0-9]* seed=0x[0-9a-f]+ vector_bytes=[1-9][0-9]* build_ms=[0-9]+ search_ms=[0-9]+ top_point_id=[1-9][0-9]*' "${log_file}"
      ;;
    hnsw_baseline)
      grep -Eq 'dataset=Small rows=[1-9][0-9]* dimensions=[1-9][0-9]* m=32 ef_construction=128 ef_search=64 build_ms=[0-9]+ search_ms=[0-9]+ vector_bytes=[1-9][0-9]* graph_bytes=[1-9][0-9]* bytes_per_vector=[1-9][0-9]* recall=(0\.[0-9]+|1\.000000) intersection=[0-9]+ exact_count=[1-9][0-9]* candidate_count=[1-9][0-9]*' "${log_file}"
      ;;
    quantized_baseline)
      grep -Eq 'dataset=Small mode=binary rows=[1-9][0-9]* dimensions=[1-9][0-9]* candidate_budget=[1-9][0-9]* codebook_bytes=0 elapsed_ms=[0-9]+ recall=(0\.[0-9]+|1\.000000) intersection=[0-9]+ exact_count=[1-9][0-9]* candidate_count=[1-9][0-9]*' "${log_file}" &&
        grep -Eq 'dataset=Small mode=scalar_sq8 rows=[1-9][0-9]* dimensions=[1-9][0-9]* candidate_budget=[1-9][0-9]* codebook_bytes=[1-9][0-9]* elapsed_ms=[0-9]+ recall=(0\.[0-9]+|1\.000000) intersection=[0-9]+ exact_count=[1-9][0-9]* candidate_count=[1-9][0-9]*' "${log_file}" &&
        grep -Eq 'dataset=Small mode=product_quantized rows=[1-9][0-9]* dimensions=[1-9][0-9]* candidate_budget=[1-9][0-9]* codebook_bytes=[1-9][0-9]* elapsed_ms=[0-9]+ recall=(0\.[0-9]+|1\.000000) intersection=[0-9]+ exact_count=[1-9][0-9]* candidate_count=[1-9][0-9]*' "${log_file}"
      ;;
    filtered_ann_baseline)
      grep -Eq 'dataset=Small filter=narrow rows=[1-9][0-9]* allowed=[1-9][0-9]* survival_rate=(0\.[0-9]+|1\.000000) filter_ms=[0-9]+ bitmap_bytes=[1-9][0-9]* search_ms=[0-9]+ recall=(0\.[0-9]+|1\.000000) intersection=[0-9]+ exact_count=[1-9][0-9]* candidate_count=[1-9][0-9]*' "${log_file}" &&
        grep -Eq 'dataset=Small filter=medium rows=[1-9][0-9]* allowed=[1-9][0-9]* survival_rate=(0\.[0-9]+|1\.000000) filter_ms=[0-9]+ bitmap_bytes=[1-9][0-9]* search_ms=[0-9]+ recall=(0\.[0-9]+|1\.000000) intersection=[0-9]+ exact_count=[1-9][0-9]* candidate_count=[1-9][0-9]*' "${log_file}" &&
        grep -Eq 'dataset=Small filter=broad rows=[1-9][0-9]* allowed=[1-9][0-9]* survival_rate=(0\.[0-9]+|1\.000000) filter_ms=[0-9]+ bitmap_bytes=[1-9][0-9]* search_ms=[0-9]+ recall=(0\.[0-9]+|1\.000000) intersection=[0-9]+ exact_count=[1-9][0-9]* candidate_count=[1-9][0-9]*' "${log_file}" &&
        grep -Eq 'dataset=Small filter=empty rows=[1-9][0-9]* allowed=0 survival_rate=0\.000000 filter_ms=[0-9]+ bitmap_bytes=[1-9][0-9]* search_ms=[0-9]+ recall=1\.000000 intersection=0 exact_count=0 candidate_count=0' "${log_file}"
      ;;
    hybrid_baseline)
      grep -Eq 'dataset=Small case=dense_only branches=[1-9][0-9]* non_empty_branches=[1-9][0-9]* input_candidates=[1-9][0-9]* output=[1-9][0-9]* elapsed_ns=[1-9][0-9]* top_point_id=[1-9][0-9]*' "${log_file}" &&
        grep -Eq 'dataset=Small case=text_only branches=[1-9][0-9]* non_empty_branches=[1-9][0-9]* input_candidates=[1-9][0-9]* output=[1-9][0-9]* elapsed_ns=[1-9][0-9]* top_point_id=[1-9][0-9]*' "${log_file}" &&
        grep -Eq 'dataset=Small case=fused_dense_text branches=[1-9][0-9]* non_empty_branches=[1-9][0-9]* input_candidates=[1-9][0-9]* output=[1-9][0-9]* elapsed_ns=[1-9][0-9]* top_point_id=[1-9][0-9]*' "${log_file}" &&
        grep -Eq 'dataset=Small case=fully_empty branches=[1-9][0-9]* non_empty_branches=0 input_candidates=0 output=0 elapsed_ns=[1-9][0-9]* top_point_id=0' "${log_file}"
      ;;
    late_interaction_ann_baseline)
      grep -Eq 'dataset=Small points=[1-9][0-9]* vectors_per_point=[1-9][0-9]* token_vectors=[1-9][0-9]* candidates_per_query=[1-9][0-9]* candidate_source_keys=[1-9][0-9]* output=[1-9][0-9]* exact_ns=[1-9][0-9]* ann_candidate_ns=[1-9][0-9]* rerank_ns=[1-9][0-9]* vector_bytes=[1-9][0-9]* token_graph_bytes=[1-9][0-9]* bytes_per_token_vector=[1-9][0-9]* projected_comparisons=[1-9][0-9]* recall=(0\.[0-9]+|1\.000000) exact_top_point_id=[1-9][0-9]* ann_top_point_id=[1-9][0-9]*' "${log_file}"
      ;;
    *)
      return 1
      ;;
  esac
}

validate_recall_log() {
  local log_file="$1"

  validate_recall_row "${log_file}" hnsw_m32_ef64 0.950000 &&
    validate_recall_row "${log_file}" scalar_sq8_rerank 0.950000 &&
    validate_recall_row "${log_file}" binary_rerank 0.750000 &&
    validate_recall_row "${log_file}" late_interaction_ann_rerank 0.950000
}

validate_recall_row() {
  local log_file="$1"
  local name="$2"
  local min_recall="$3"

  awk -v expected_name="${name}" -v expected_min="${min_recall}" '
    $1 == "recall_gate" {
      delete field
      for (i = 1; i <= NF; i++) {
        split($i, pair, "=")
        field[pair[1]] = pair[2]
      }

      if (field["name"] != expected_name || field["min"] != expected_min) {
        next
      }
      if (field["recall"] !~ /^(0\.[0-9]+|1\.000000)$/) {
        next
      }
      if ((field["recall"] + 0) < (expected_min + 0)) {
        next
      }
      if ((field["intersection"] + 0) < 1 ||
          (field["exact_count"] + 0) < 1 ||
          (field["candidate_count"] + 0) < 1) {
        next
      }
      found = 1
    }
    END { exit found ? 0 : 1 }
  ' "${log_file}"
}

for bench in "${selected[@]}"; do
  if ! known_benchmark "${bench}"; then
    echo "unknown benchmark: ${bench}" >&2
    exit 2
  fi
done

if [[ "${dry_run}" -eq 0 ]] && ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required to run benchmark reports" >&2
  exit 2
fi

git_sha="unknown"
if git -C "${REPO_ROOT}" rev-parse --verify HEAD >/dev/null 2>&1; then
  git_sha="$(git -C "${REPO_ROOT}" rev-parse --verify HEAD)"
fi

worktree_state="unknown"
if git -C "${REPO_ROOT}" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  if [[ -z "$(git -C "${REPO_ROOT}" status --short)" ]]; then
    worktree_state="clean"
  else
    worktree_state="dirty"
  fi
fi

host_os="$(uname -srm)"
cpu_model="$(sysctl -n machdep.cpu.brand_string 2>/dev/null || awk -F: '/model name/ {print $2; exit}' /proc/cpuinfo 2>/dev/null | sed 's/^ *//' || printf 'unavailable')"
rustc_version="$(rustc -V 2>/dev/null || printf 'unavailable')"
cargo_version="$(cargo -V 2>/dev/null || printf 'unavailable')"
started_all="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

mkdir -p "${out_dir}"
summary_tsv="${out_dir}/summary.tsv"
report_md="${out_dir}/report.md"

{
  printf 'target\tkind\tstatus\texit_code\tstarted_utc\tfinished_utc\tlog\tboundary\tcommand\n'
} >"${summary_tsv}"

{
  printf '# Benchmark And Recall Report\n\n'
  printf -- '- Commit: `%s`\n' "${git_sha}"
  printf -- '- Worktree: `%s`\n' "${worktree_state}"
  printf -- '- Host: `%s`\n' "${host_os}"
  printf -- '- CPU: `%s`\n' "${cpu_model}"
  printf -- '- Rust: `%s`\n' "${rustc_version}"
  printf -- '- Cargo: `%s`\n' "${cargo_version}"
  printf -- '- Started: `%s`\n' "${started_all}"
  printf -- '- PostgreSQL: `%s`\n' "${postgres_version}"
  printf -- '- Feature flags: `%s`\n' "${feature_flags}"
  printf -- '- Baseline: `%s`\n' "${baseline}"
  printf -- '- Warmup/sample settings: `%s`\n' "${sample_settings}"
  printf -- '- Threshold waiver note: `%s`\n' "${waiver_note}"
  if [[ "${dry_run}" -eq 1 ]]; then
    printf -- '- Mode: `dry-run`\n'
  else
    printf -- '- Mode: `execute`\n'
  fi
  printf '\n'
  printf '| Target | Kind | Boundary | Status | Log |\n'
  printf '|---|---|---|---|---|\n'
} >"${report_md}"

append_result() {
  local target="$1"
  local kind="$2"
  local status="$3"
  local exit_code="$4"
  local started="$5"
  local finished="$6"
  local log_file="$7"
  local boundary="$8"
  local command="$9"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${target}" "${kind}" "${status}" "${exit_code}" "${started}" "${finished}" \
    "$(repo_relative_path "${log_file}")" "${boundary}" "${command}" >>"${summary_tsv}"
  printf '| `%s` | `%s` | %s | `%s` | `%s` |\n' \
    "${target}" "${kind}" "${boundary}" "${status}" "$(repo_relative_path "${log_file}")" >>"${report_md}"
}

overall_status=0
total_rows=0
passed_rows=0
dry_run_rows=0
failed_rows=0
run_entry() {
  local target="$1"
  local kind="$2"
  local command="$3"
  local log_file="$4"
  shift 4

  local started
  local finished
  local status="passed"
  local exit_code=0
  local boundary
  boundary="$(benchmark_boundary "${target}")"
  started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  if [[ "${dry_run}" -eq 1 ]]; then
    status="dry-run"
    {
      printf 'dry run: %s\n' "${command}"
      printf 'boundary: %s\n' "${boundary}"
      printf 'baseline: %s\n' "${baseline}"
      printf 'postgres: %s\n' "${postgres_version}"
      printf 'feature flags: %s\n' "${feature_flags}"
      printf 'sample settings: %s\n' "${sample_settings}"
      printf 'threshold waiver note: %s\n' "${waiver_note}"
    } >"${log_file}"
  else
    (
      cd "${REPO_ROOT}"
      "$@"
    ) >"${log_file}" 2>&1 || {
      exit_code=$?
      status="failed"
      overall_status=1
    }
    if [[ "${status}" == "passed" && "${kind}" == "benchmark" ]] &&
      ! validate_benchmark_log "${target}" "${log_file}"; then
      {
        printf '\nbenchmark evidence validation failed for %s\n' "${target}"
        printf 'expected benchmark output contract for: %s\n' "$(benchmark_boundary "${target}")"
      } >>"${log_file}"
      exit_code=65
      status="failed"
      overall_status=1
    elif [[ "${status}" == "passed" && "${kind}" == "recall" ]] &&
      ! validate_recall_log "${log_file}"; then
      {
        printf '\nrecall evidence validation failed for %s\n' "${target}"
        printf 'expected recall_gate rows for HNSW, SQ8 rerank, binary rerank, and late-interaction ANN rerank\n'
      } >>"${log_file}"
      exit_code=65
      status="failed"
      overall_status=1
    fi
  fi

  total_rows=$((total_rows + 1))
  case "${status}" in
    passed)
      passed_rows=$((passed_rows + 1))
      ;;
    dry-run)
      dry_run_rows=$((dry_run_rows + 1))
      ;;
    failed)
      failed_rows=$((failed_rows + 1))
      ;;
  esac

  finished="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  append_result "${target}" "${kind}" "${status}" "${exit_code}" "${started}" "${finished}" "${log_file}" "${boundary}" "${command}"
}

for bench in "${selected[@]}"; do
  log_file="${out_dir}/${bench}.log"
  command="cargo bench -p context-test --bench ${bench}"
  run_entry "${bench}" "benchmark" "${command}" "${log_file}" \
    cargo bench -p context-test --bench "${bench}"
done

if [[ "${include_recall}" -eq 1 ]]; then
  log_file="${out_dir}/${RECALL_GATE}.log"
  command="cargo test -p context-test --test ${RECALL_GATE} -- --nocapture"
  run_entry "${RECALL_GATE}" "recall" "${command}" "${log_file}" \
    cargo test -p context-test --test "${RECALL_GATE}" -- --nocapture
fi

{
  printf '\n## Summary\n\n'
  printf -- '- Rows: `%s`\n' "${total_rows}"
  printf -- '- Passed: `%s`\n' "${passed_rows}"
  printf -- '- Dry-run: `%s`\n' "${dry_run_rows}"
  printf -- '- Failed: `%s`\n' "${failed_rows}"
  if [[ "${total_rows}" -eq "$((${#BENCHMARKS[@]} + 1))" &&
        "${passed_rows}" -eq "${total_rows}" &&
        "${dry_run_rows}" -eq 0 &&
        "${failed_rows}" -eq 0 &&
        "${include_recall}" -eq 1 &&
        "${worktree_state}" == "clean" &&
        "${baseline}" != "unset" &&
        "${waiver_note}" == "none" ]]; then
    printf -- '- Approval: `complete`\n'
  else
    printf -- '- Approval: `incomplete`\n'
    printf -- '- Approval note: release benchmark evidence requires all default benchmarks, recall gates, no dry-run rows, no failures, a clean worktree, a named baseline, and no threshold waiver.\n'
  fi
  printf '\n## Summary TSV\n\n'
  printf 'See `%s`.\n' "$(repo_relative_path "${summary_tsv}")"
} >>"${report_md}"

printf 'benchmark report: %s\n' "${report_md}"
exit "${overall_status}"
