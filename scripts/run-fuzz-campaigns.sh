#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
repo_physical="$(cd -P "${REPO_ROOT}" && pwd -P)"
DEFAULT_TARGETS=(
  filter_json
  sql_predicate
  vector_text
  segment_loader
  candidate_mask
)
DEFAULT_DURATION_SECONDS=86400
MIN_RELEASE_ELAPSED_SECONDS=86340
FUZZ_TARGETS_DIR="${REPO_ROOT}/fuzz/fuzz_targets"

duration_seconds="${FUZZ_DURATION_SECONDS:-${DEFAULT_DURATION_SECONDS}}"
out_dir="${FUZZ_REPORT_DIR:-${REPO_ROOT}/target/fuzz-campaigns/$(git -C "${REPO_ROOT}" rev-parse HEAD)}"
dry_run=0
allow_dirty=0
jobs=1
targets=()

usage() {
  cat <<'USAGE'
Usage: scripts/run-fuzz-campaigns.sh [options]

Run release-candidate cargo-fuzz campaigns and write auditable logs.

Options:
  --target NAME       Run one target. May be repeated. Defaults to all targets.
  --duration SECONDS  Per-target max_total_time. Defaults to 86400.
  --out-dir PATH      Report/log directory. Defaults under target/fuzz-campaigns.
  --jobs N            Run up to N fuzz targets concurrently. Defaults to 1.
  --dry-run           Write the report plan without executing cargo fuzz.
  --allow-dirty       Run on a dirty worktree, but keep approval incomplete.
  -h, --help          Show this help text.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      [[ $# -ge 2 ]] || {
        echo "--target requires a value" >&2
        exit 2
      }
      targets+=("$2")
      shift 2
      ;;
    --duration)
      [[ $# -ge 2 ]] || {
        echo "--duration requires a value" >&2
        exit 2
      }
      duration_seconds="$2"
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
    --jobs)
      [[ $# -ge 2 ]] || {
        echo "--jobs requires a value" >&2
        exit 2
      }
      jobs="$2"
      shift 2
      ;;
    --dry-run)
      dry_run=1
      shift
      ;;
    --allow-dirty)
      allow_dirty=1
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

if [[ ! "${duration_seconds}" =~ ^[1-9][0-9]*$ ]]; then
  echo "--duration must be a positive integer number of seconds" >&2
  exit 2
fi
if [[ ! "${jobs}" =~ ^[1-9][0-9]*$ ]]; then
  echo "--jobs must be a positive integer" >&2
  exit 2
fi

if [[ -z "${out_dir}" || "${out_dir}" == "/" ]]; then
  echo "--out-dir must be a non-root path" >&2
  exit 2
fi
if [[ "${out_dir}" != /* ]]; then
  out_dir="${REPO_ROOT}/${out_dir}"
fi

if [[ ${#targets[@]} -eq 0 ]]; then
  targets=("${DEFAULT_TARGETS[@]}")
fi

for ((i = 0; i < ${#targets[@]}; i++)); do
  for ((j = i + 1; j < ${#targets[@]}; j++)); do
    if [[ "${targets[$i]}" == "${targets[$j]}" ]]; then
      echo "duplicate fuzz target: ${targets[$i]}" >&2
      exit 2
    fi
  done
done

known_target() {
  local target="$1"
  local known
  for known in "${DEFAULT_TARGETS[@]}"; do
    [[ "${target}" == "${known}" ]] && return 0
  done
  return 1
}

boundary_for_target() {
  case "$1" in
    filter_json)
      printf '%s\n' "filter JSON parsing, filter budgets, and JSONB path handling"
      ;;
    sql_predicate)
      printf '%s\n' "SQL predicate rendering and placeholder accounting"
      ;;
    vector_text)
      printf '%s\n' "dense, half, sparse, and bit vector text parsing"
      ;;
    segment_loader)
      printf '%s\n' "segment loading, mmap validation, and encode/decode checks"
      ;;
    candidate_mask)
      printf '%s\n' "candidate mask decoding and budget validation"
      ;;
    *)
      printf '%s\n' "unknown"
      ;;
  esac
}

release_boundaries_for_target() {
  case "$1" in
    filter_json)
      printf '%s\n' "filter JSON; JSONB path handling"
      ;;
    sql_predicate)
      printf '%s\n' "SQL predicate rendering"
      ;;
    vector_text)
      printf '%s\n' "vector text"
      ;;
    segment_loader)
      printf '%s\n' "segment loading; mmap views"
      ;;
    candidate_mask)
      printf '%s\n' "candidate masks"
      ;;
    *)
      printf '%s\n' "unknown"
      ;;
  esac
}

iso8601_from_epoch() {
  local epoch="$1"
  if date -u -r "${epoch}" +%Y-%m-%dT%H:%M:%SZ >/dev/null 2>&1; then
    date -u -r "${epoch}" +%Y-%m-%dT%H:%M:%SZ
  else
    date -u -d "@${epoch}" +%Y-%m-%dT%H:%M:%SZ
  fi
}

repo_relative_path() {
  local path="$1"

  case "${path}" in
    "${REPO_ROOT}"/*) printf '%s\n' "${path#"${REPO_ROOT}/"}" ;;
    "${repo_physical}"/*) printf '%s\n' "${path#"${repo_physical}/"}" ;;
    *) printf '%s\n' "${path}" ;;
  esac
}

for target in "${targets[@]}"; do
  if ! known_target "${target}"; then
    echo "unknown fuzz target: ${target}" >&2
    exit 2
  fi
  if [[ ! -f "${FUZZ_TARGETS_DIR}/${target}.rs" ]]; then
    echo "fuzz target source is missing: fuzz/fuzz_targets/${target}.rs" >&2
    exit 2
  fi
  if [[ ! -d "${REPO_ROOT}/fuzz/corpus/${target}" ]]; then
    echo "fuzz seed corpus is missing: fuzz/corpus/${target}" >&2
    exit 2
  fi
done

if [[ "${dry_run}" -eq 0 ]] && ! command -v cargo >/dev/null 2>&1; then
  echo "cargo is required to run fuzz campaigns" >&2
  exit 2
fi

mkdir -p "${out_dir}"
out_dir_repo_contained=0
out_dir_physical="$(cd -P "${out_dir}" && pwd -P)"
out_dir="${out_dir_physical}"
case "${out_dir_physical}" in
  "${repo_physical}" | "${repo_physical}"/*) out_dir_repo_contained=1 ;;
esac

summary_tsv="${out_dir}/summary.tsv"
report_md="${out_dir}/report.md"
work_corpus_root="${out_dir}/corpus"
artifact_root="${out_dir}/artifacts"

git_sha="unknown"
if git -C "${REPO_ROOT}" rev-parse --verify HEAD >/dev/null 2>&1; then
  git_sha="$(git -C "${REPO_ROOT}" rev-parse --verify HEAD)"
fi

host_os="$(uname -srm)"
rustc_version="$(rustc -V 2>/dev/null || printf 'unavailable')"
cargo_version="$(cargo -V 2>/dev/null || printf 'unavailable')"
cargo_fuzz_version="not-run"
if [[ "${dry_run}" -eq 0 ]]; then
  cargo_fuzz_version="$(cargo +nightly fuzz --version 2>/dev/null || printf 'unavailable')"
fi
worktree_state="unknown"
if git -C "${REPO_ROOT}" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  if [[ -z "$(git -C "${REPO_ROOT}" status --short)" ]]; then
    worktree_state="clean"
  else
    worktree_state="dirty"
  fi
fi
if [[ "${dry_run}" -eq 0 && "${worktree_state}" == "dirty" && "${allow_dirty}" -eq 0 ]]; then
  echo "dirty worktree cannot produce release fuzz evidence; use --allow-dirty for diagnostic runs" >&2
  exit 2
fi
started_all="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

{
  printf 'target\tstatus\texit_code\tstarted_utc\tfinished_utc\trequested_duration_seconds\telapsed_seconds\tartifact_count\tcorpus\tartifacts\tlog\tboundary\trelease_boundaries\tcommand\tlog_bytes\n'
} >"${summary_tsv}"

{
  printf '# Fuzz Campaign Report\n\n'
  printf -- '- Commit: `%s`\n' "${git_sha}"
  printf -- '- Worktree: `%s`\n' "${worktree_state}"
  printf -- '- Host: `%s`\n' "${host_os}"
  printf -- '- Rust: `%s`\n' "${rustc_version}"
  printf -- '- Cargo: `%s`\n' "${cargo_version}"
  printf -- '- Cargo fuzz: `%s`\n' "${cargo_fuzz_version}"
  printf -- '- Started: `%s`\n' "${started_all}"
  printf -- '- Per-target duration: `%s` seconds\n' "${duration_seconds}"
  printf -- '- Max concurrent targets: `%s`\n' "${jobs}"
  if [[ "${out_dir_repo_contained}" -eq 1 ]]; then
    printf -- '- Evidence directory: `repo`\n'
  else
    printf -- '- Evidence directory: `external`\n'
  fi
  printf -- '- Dirty override: `%s`\n' "${allow_dirty}"
  if [[ "${dry_run}" -eq 1 ]]; then
    printf -- '- Mode: `dry-run`\n'
  else
    printf -- '- Mode: `execute`\n'
  fi
  printf -- '- Required release duration: `%s` seconds per target\n' "${DEFAULT_DURATION_SECONDS}"
  printf '\n'
  printf '## Release Boundary Coverage\n\n'
  printf '| Release boundary | Fuzz target |\n'
  printf '|---|---|\n'
  for target in "${DEFAULT_TARGETS[@]}"; do
    release_boundaries="$(release_boundaries_for_target "${target}")"
    IFS=';' read -ra boundary_parts <<<"${release_boundaries}"
    for boundary_part in "${boundary_parts[@]}"; do
      boundary_part="${boundary_part#"${boundary_part%%[![:space:]]*}"}"
      boundary_part="${boundary_part%"${boundary_part##*[![:space:]]}"}"
      printf '| %s | `%s` |\n' "${boundary_part}" "${target}"
    done
  done
  printf '\n'
  printf '| Target | Boundary | Status | Exit | Duration | Log |\n'
  printf '|---|---|---|---:|---:|---|\n'
} >"${report_md}"

run_one_target() {
  local target="$1"
  local target_passed_rows=0
  local target_dry_run_rows=0
  local target_failed_rows=0
  local target_short_rows=0
  local target_artifact_rows=0
  local target_corpus_symlink_rows=0

  seed_corpus="fuzz/corpus/${target}"
  work_corpus="${work_corpus_root}/${target}"
  artifact_dir="${artifact_root}/${target}"
  log_file="${out_dir}/${target}.log"
  raw_output_file="${out_dir}/${target}.raw.log"
  boundary="$(boundary_for_target "${target}")"
  release_boundaries="$(release_boundaries_for_target "${target}")"
  command_text="cargo +nightly fuzz run ${target} ${work_corpus} -- -max_total_time=${duration_seconds} -artifact_prefix=${artifact_dir}/"
  command_text_relative="cargo +nightly fuzz run ${target} $(repo_relative_path "${work_corpus}") -- -max_total_time=${duration_seconds} -artifact_prefix=$(repo_relative_path "${artifact_dir}")/"
  started_epoch="$(date -u +%s)"
  started="$(iso8601_from_epoch "${started_epoch}")"
  status="passed"
  exit_code=0

  mkdir -p "${work_corpus}" "${artifact_dir}"
  if [[ -d "${REPO_ROOT}/${seed_corpus}" ]]; then
    cp -R "${REPO_ROOT}/${seed_corpus}/." "${work_corpus}/"
  fi
  corpus_symlink_count="$(find "${work_corpus}" -type l | wc -l | tr -d ' ')"

  if [[ "${dry_run}" -eq 1 ]]; then
    status="dry-run"
    raw_output_bytes=0
    {
      printf 'target: %s\n' "${target}"
      printf 'boundary: %s\n' "${boundary}"
      printf 'release boundaries: %s\n' "${release_boundaries}"
      printf 'requested duration seconds: %s\n' "${duration_seconds}"
      printf 'command: %s\n' "${command_text_relative}"
      printf 'dry run: %s\n' "${command_text}"
      printf 'seed corpus: %s\n' "${seed_corpus}"
      printf 'work corpus: %s\n' "$(repo_relative_path "${work_corpus}")"
      printf 'artifact dir: %s\n' "$(repo_relative_path "${artifact_dir}")"
    } >"${log_file}"
  else
    {
      printf 'target: %s\n' "${target}"
      printf 'boundary: %s\n' "${boundary}"
      printf 'release boundaries: %s\n' "${release_boundaries}"
      printf 'requested duration seconds: %s\n' "${duration_seconds}"
      printf 'command: %s\n' "${command_text_relative}"
      printf 'fuzz command output begin\n'
    } >"${log_file}"
    (
      cd "${REPO_ROOT}/fuzz"
      cargo +nightly fuzz run "${target}" "${work_corpus}" -- \
        -max_total_time="${duration_seconds}" \
        -artifact_prefix="${artifact_dir}/"
    ) >"${raw_output_file}" 2>&1 || {
      exit_code=$?
      status="failed"
      overall_status=1
    }
    raw_output_bytes="$(wc -c <"${raw_output_file}" | tr -d ' ')"
    cat "${raw_output_file}" >>"${log_file}"
    {
      printf 'fuzz command output end\n'
      printf 'fuzz raw output bytes: %s\n' "${raw_output_bytes}"
    } >>"${log_file}"
    rm -f "${raw_output_file}"
  fi

  case "${status}" in
    passed)
      target_passed_rows=1
      ;;
    dry-run)
      target_dry_run_rows=1
      ;;
    failed)
      target_failed_rows=1
      ;;
  esac

  finished_epoch="$(date -u +%s)"
  finished="$(iso8601_from_epoch "${finished_epoch}")"
  elapsed_seconds=$((finished_epoch - started_epoch))
  artifact_count="$(find "${artifact_dir}" -mindepth 1 | wc -l | tr -d ' ')"
  if [[ "${status}" == "passed" && "${duration_seconds}" -ge "${DEFAULT_DURATION_SECONDS}" && "${elapsed_seconds}" -lt "${MIN_RELEASE_ELAPSED_SECONDS}" ]]; then
    target_short_rows=1
  fi
  if [[ "${artifact_count}" -gt 0 ]]; then
    target_artifact_rows=1
  fi
  if [[ "${corpus_symlink_count}" -gt 0 ]]; then
    target_corpus_symlink_rows=1
  fi
  {
    if [[ "${status}" == "passed" ]]; then
      printf 'fuzz_target_exercised: %s\n' "${target}"
    fi
    printf 'elapsed seconds: %s\n' "${elapsed_seconds}"
    printf 'artifact count: %s\n' "${artifact_count}"
    printf 'corpus symlink count: %s\n' "${corpus_symlink_count}"
    printf 'fuzz campaign status: %s\n' "${status}"
    printf 'fuzz campaign exit code: %s\n' "${exit_code}"
  } >>"${log_file}"
  log_bytes="$(wc -c <"${log_file}" | tr -d ' ')"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${target}" \
    "${status}" \
    "${exit_code}" \
    "${started}" \
    "${finished}" \
    "${duration_seconds}" \
    "${elapsed_seconds}" \
    "${artifact_count}" \
    "$(repo_relative_path "${work_corpus}")" \
    "$(repo_relative_path "${artifact_dir}")" \
    "$(repo_relative_path "${log_file}")" \
    "${boundary}" \
    "${release_boundaries}" \
    "${command_text_relative}" \
    "${log_bytes}" >"${out_dir}/${target}.summary-row.tsv"
  printf '| `%s` | %s | `%s` | `%s` | `%s/%s` | `%s` |\n' \
    "${target}" \
    "${boundary}" \
    "${status}" \
    "${exit_code}" \
    "${elapsed_seconds}" \
    "${duration_seconds}" \
    "$(repo_relative_path "${log_file}")" >"${out_dir}/${target}.report-row.md"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${status}" "${target_passed_rows}" "${target_dry_run_rows}" "${target_failed_rows}" "${target_short_rows}" "${target_artifact_rows}" "${target_corpus_symlink_rows}" \
    >"${out_dir}/${target}.stats.tsv"
}

wait_for_next_target() {
  local pid="${active_pids[0]}"

  wait "${pid}" || true
  active_pids=("${active_pids[@]:1}")
}

overall_status=0
active_pids=()
if [[ "${jobs}" -le 1 ]]; then
  for target in "${targets[@]}"; do
    run_one_target "${target}"
  done
else
  for target in "${targets[@]}"; do
    while [[ "${#active_pids[@]}" -ge "${jobs}" ]]; do
      wait_for_next_target
    done
    run_one_target "${target}" &
    active_pids+=("$!")
  done
  for pid in "${active_pids[@]}"; do
    wait "${pid}" || true
  done
fi

total_rows=0
passed_rows=0
dry_run_rows=0
failed_rows=0
short_rows=0
artifact_rows=0
corpus_symlink_rows=0
for target in "${targets[@]}"; do
  if [[ ! -f "${out_dir}/${target}.summary-row.tsv" ||
        ! -f "${out_dir}/${target}.report-row.md" ||
        ! -f "${out_dir}/${target}.stats.tsv" ]]; then
    printf 'missing fuzz target result for %s\n' "${target}" >"${out_dir}/${target}.log"
    printf '%s\tfailed\t127\tunknown\tunknown\t%s\t0\t0\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
      "${target}" \
      "${duration_seconds}" \
      "$(repo_relative_path "${work_corpus_root}/${target}")" \
      "$(repo_relative_path "${artifact_root}/${target}")" \
      "$(repo_relative_path "${out_dir}/${target}.log")" \
      "$(boundary_for_target "${target}")" \
      "$(release_boundaries_for_target "${target}")" \
      "cargo +nightly fuzz run ${target}" \
      "0" >>"${summary_tsv}"
    printf '| `%s` | %s | `failed` | `127` | `0/%s` | `%s` |\n' \
      "${target}" \
      "$(boundary_for_target "${target}")" \
      "${duration_seconds}" \
      "$(repo_relative_path "${out_dir}/${target}.log")" >>"${report_md}"
    failed_rows=$((failed_rows + 1))
    total_rows=$((total_rows + 1))
    overall_status=1
    continue
  fi

  cat "${out_dir}/${target}.summary-row.tsv" >>"${summary_tsv}"
  cat "${out_dir}/${target}.report-row.md" >>"${report_md}"
  IFS=$'\t' read -r status target_passed target_dry_run target_failed target_short target_artifact target_corpus_symlink <"${out_dir}/${target}.stats.tsv"
  total_rows=$((total_rows + 1))
  passed_rows=$((passed_rows + target_passed))
  dry_run_rows=$((dry_run_rows + target_dry_run))
  failed_rows=$((failed_rows + target_failed))
  short_rows=$((short_rows + target_short))
  artifact_rows=$((artifact_rows + target_artifact))
  corpus_symlink_rows=$((corpus_symlink_rows + target_corpus_symlink))
  if [[ "${status}" == "failed" ]]; then
    overall_status=1
  fi
done

{
  printf '\n## Summary\n\n'
  printf -- '- Rows: `%s`\n' "${total_rows}"
  printf -- '- Passed: `%s`\n' "${passed_rows}"
  printf -- '- Dry-run: `%s`\n' "${dry_run_rows}"
  printf -- '- Failed: `%s`\n' "${failed_rows}"
  printf -- '- Short elapsed rows: `%s`\n' "${short_rows}"
  printf -- '- Rows with artifacts: `%s`\n' "${artifact_rows}"
  printf -- '- Rows with corpus symlinks: `%s`\n' "${corpus_symlink_rows}"
  if [[ "${total_rows}" -eq "${#DEFAULT_TARGETS[@]}" &&
        "${passed_rows}" -eq "${total_rows}" &&
        "${dry_run_rows}" -eq 0 &&
        "${failed_rows}" -eq 0 &&
        "${short_rows}" -eq 0 &&
        "${artifact_rows}" -eq 0 &&
        "${corpus_symlink_rows}" -eq 0 &&
        "${duration_seconds}" -ge "${DEFAULT_DURATION_SECONDS}" &&
        "${worktree_state}" == "clean" &&
        "${out_dir_repo_contained}" -eq 1 &&
        "${allow_dirty}" -eq 0 ]]; then
    printf -- '- Approval: `complete`\n'
  else
    printf -- '- Approval: `incomplete`\n'
    printf -- '- Approval note: release fuzz evidence requires all default targets, no dry-run rows, no failures, no short elapsed rows, no crash artifacts, no corpus symlinks, a clean worktree, repo-contained evidence paths, and at least `%s` seconds per target.\n' "${DEFAULT_DURATION_SECONDS}"
  fi
  printf '\n## Summary TSV\n\n'
  printf 'See `%s`.\n' "$(repo_relative_path "${summary_tsv}")"
} >>"${report_md}"

printf 'fuzz campaign report: %s\n' "${report_md}"
exit "${overall_status}"
