#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
SUPPORTED_MAJORS=(15 16 17 18)
HEAVY_GATES=(
  tests/heavy/fresh_install_smoke.sh
  tests/heavy/drop_extension_survival.sh
  tests/heavy/upgrade_matrix.sh
  tests/heavy/backup_restore.sh
  tests/heavy/cross_version_import.sh
  tests/heavy/physical_backup_wal_replay.sh
  tests/heavy/crash_restart_hnsw.sh
  tests/heavy/mapped_hnsw_lifecycle_cleanup.sh
  tests/heavy/mmap_hnsw_artifact_restart.sh
  tests/heavy/hnsw_vacuum.sh
  tests/heavy/concurrent_read_write.sh
  tests/heavy/filtered_ann_recall.sh
  tests/heavy/late_interaction_ann_serving.sh
  tests/heavy/build_job_resumability.sh
  tests/heavy/artifact_publication_rollback.sh
  tests/heavy/rls_acl_boundary.sh
  tests/heavy/large_exact_search.sh
  tests/heavy/partitioned_collections.sh
  tests/heavy/low_memory_build.sh
  tests/heavy/corrupt_artifact_detection.sh
  tests/heavy/sqlstate_contract.sh
)
MMAP_HNSW_RESTART_MARKERS=(
  "mmap_artifact_serving_ready: before_restart"
  "mmap_artifact_source_recheck: before_restart"
  "mmap_artifact_budget_rejected: before_restart"
  "mmap_artifact_serving_ready: after_restart"
  "mmap_artifact_source_recheck: after_restart"
  "mmap_artifact_budget_rejected: after_restart"
  "mmap_artifact_vacuum_recheck: after_restart"
)
HNSW_VACUUM_MARKERS=(
  "hnsw_vacuum_nearest_rechecked"
  "hnsw_vacuum_deleted_rows_pruned"
  "hnsw_vacuum_index_ready"
  "hnsw_vacuum_advice_present"
  "hnsw_reindex_ready"
)
FILTERED_ANN_RECALL_MARKERS=(
  "filtered_ann_exact_oracle_seqscan"
  "filtered_ann_candidate_indexscan"
  "filtered_ann_exact_candidates_match"
  "filtered_ann_recall_passing"
  "filtered_ann_tenant_recheck_passing"
  "filtered_ann_no_match_empty"
)
LATE_INTERACTION_ANN_SERVING_MARKERS=(
  "late_interaction_ann_candidates_deduped"
  "late_interaction_ann_exact_rerank_scores"
  "late_interaction_ann_source_recheck"
  "late_interaction_ann_deleted_recheck"
  "late_interaction_ann_budget_rejected"
)
BUILD_JOB_RESUMABILITY_MARKERS=(
  "build_job_progress_preserved"
  "build_job_source_recheck_after_update"
  "build_job_abandoned_owner_recovered"
  "build_job_final_serving_ready_view_a"
  "build_job_final_serving_ready_view_b"
  "build_job_source_recheck_after_restart"
  "build_job_vacuum_source_recheck"
)
HNSW_RESTART_MARKERS=(
  "hnsw_restart_nearest_rechecked: before_restart"
  "hnsw_restart_index_scan: before_restart"
  "hnsw_mapped_attach: before_restart"
  "hnsw_restart_nearest_rechecked: after_restart"
  "hnsw_restart_index_scan: after_restart"
  "hnsw_mapped_attach: after_restart"
)
MAPPED_HNSW_LIFECYCLE_MARKERS=(
  "mapped_hnsw_drop_rollback_preserved"
  "mapped_hnsw_drop_index_reclaimed"
  "mapped_hnsw_drop_table_reclaimed"
  "mapped_hnsw_prepared_commit_reconciled"
  "mapped_hnsw_prepared_abort_preserved"
  "mapped_hnsw_drop_marker_restart_preserved"
  "mapped_hnsw_concurrent_cursor_progressed"
  "mapped_hnsw_current_temps_reclaimed"
  "mapped_hnsw_stale_temps_do_not_starve"
  "mapped_hnsw_reconcile_window_advanced"
  "mapped_hnsw_temp_drop_reclaimed"
  "mapped_hnsw_temp_teardown_reclaimed"
  "mapped_hnsw_drop_database_reclaimed"
)
CONCURRENT_READ_WRITE_MARKERS=(
  "concurrent_hnsw_writer_completed"
  "concurrent_hnsw_reader_completed"
  "concurrent_hnsw_row_count_verified"
  "concurrent_hnsw_insert_visible"
)
LARGE_EXACT_SEARCH_MARKERS=(
  "NOTICE:  large_exact_rows_loaded"
  "NOTICE:  large_exact_oracle_match"
  "NOTICE:  large_exact_missing_filter_empty"
  "NOTICE:  large_exact_dimension_mismatch_rejected"
  "NOTICE:  large_exact_unknown_filter_rejected"
)
PARTITIONED_COLLECTIONS_MARKERS=(
  "NOTICE:  partitioned_exact_order_verified"
  "NOTICE:  partitioned_tenant_filter_verified"
  "NOTICE:  partitioned_count_verified"
  "NOTICE:  partitioned_facet_verified"
  "NOTICE:  partitioned_delete_visibility_verified"
  "NOTICE:  partitioned_drop_recheck_verified"
  "NOTICE:  partitioned_unknown_filter_rejected"
)
LOW_MEMORY_BUILD_MARKERS=(
  "NOTICE:  low_memory_rejected_bad_build"
  "NOTICE:  low_memory_failed_build_cleaned"
  "NOTICE:  low_memory_index_order_verified"
  "NOTICE:  low_memory_reltuples_verified"
)
BACKUP_RESTORE_MARKERS=(
  "backup_restore_dump_created"
  "backup_restore_restore_completed"
  "NOTICE:  backup_restore_nearest_verified"
  "NOTICE:  backup_restore_filter_verified"
  "NOTICE:  backup_restore_jsonb_facet_verified"
  "NOTICE:  backup_restore_scroll_verified"
  "NOTICE:  backup_restore_model_versions_verified"
  "NOTICE:  backup_restore_migration_verified"
  "NOTICE:  backup_restore_telemetry_verified"
  "NOTICE:  backup_restore_query_stats_verified"
  "NOTICE:  backup_restore_hnsw_ready"
)
PHYSICAL_BACKUP_WAL_REPLAY_MARKERS=(
  "physical_backup_basebackup_created"
  "NOTICE:  physical_backup_exact_nearest_verified: before_replay"
  "NOTICE:  physical_backup_indexed_nearest_verified: before_replay"
  "NOTICE:  physical_backup_exact_scroll_verified: before_replay"
  "NOTICE:  physical_backup_indexed_scroll_verified: before_replay"
  "NOTICE:  physical_backup_hnsw_ready: before_replay"
  "physical_backup_restarted_after_writes"
  "NOTICE:  physical_backup_exact_nearest_verified: after_replay"
  "NOTICE:  physical_backup_indexed_nearest_verified: after_replay"
  "NOTICE:  physical_backup_exact_scroll_verified: after_replay"
  "NOTICE:  physical_backup_indexed_scroll_verified: after_replay"
  "NOTICE:  physical_backup_hnsw_ready: after_replay"
)

physical_backup_wal_replay_marker_order_failure() {
  local log_file="$1"
  local previous_line=0
  local marker
  local marker_line

  for marker in "${PHYSICAL_BACKUP_WAL_REPLAY_MARKERS[@]}"; do
    marker_line="$(awk -v marker="${marker}" -v previous_line="${previous_line}" '
      NR > previous_line && $0 == marker {
        print NR
        exit
      }
    ' "${log_file}")"
    if [[ -z "${marker_line}" ]]; then
      printf '%s\n' "${marker}"
      return 1
    fi
    previous_line="${marker_line}"
  done
  return 0
}

mode="${PG_MATRIX_MODE:-all}"
default_git_sha="$(git -C "${REPO_ROOT}" rev-parse --verify HEAD 2>/dev/null || printf 'unknown')"
out_dir="${PG_MATRIX_REPORT_DIR:-${REPO_ROOT}/target/postgres-matrix/${default_git_sha}}"
dry_run=0
allow_missing=0
majors=()

usage() {
  cat <<'USAGE'
Usage: scripts/run-postgres-matrix-gates.sh [options]

Run PostgreSQL-version release gates and write a TSV/Markdown report.

Options:
  --major N          Run one supported major. May be repeated.
  --mode MODE        fast, schema, pgrx, heavy, or all. Defaults to all.
  --out-dir PATH     Report/log directory. Defaults under target/postgres-matrix.
  --allow-missing    Report missing pg_config as skipped instead of failing.
  --dry-run          Write the report plan without executing cargo commands.
  -h, --help         Show this help text.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --major)
      [[ $# -ge 2 ]] || {
        echo "--major requires a value" >&2
        exit 2
      }
      majors+=("$2")
      shift 2
      ;;
    --mode)
      [[ $# -ge 2 ]] || {
        echo "--mode requires a value" >&2
        exit 2
      }
      mode="$2"
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
    --allow-missing)
      allow_missing=1
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

case "${mode}" in
  fast | schema | pgrx | heavy | all) ;;
  *)
    echo "--mode must be fast, schema, pgrx, heavy, or all" >&2
    exit 2
    ;;
esac

if [[ -z "${out_dir}" || "${out_dir}" == "/" ]]; then
  echo "--out-dir must be a non-root path" >&2
  exit 2
fi
if [[ "${out_dir}" != /* ]]; then
  out_dir="${REPO_ROOT}/${out_dir}"
fi

if [[ ${#majors[@]} -eq 0 ]]; then
  majors=("${SUPPORTED_MAJORS[@]}")
fi

is_supported_major() {
  local major="$1"
  local supported
  for supported in "${SUPPORTED_MAJORS[@]}"; do
    [[ "${major}" == "${supported}" ]] && return 0
  done
  return 1
}

pg_config_for_major() {
  local major="$1"
  local override="PG${major}_CONFIG"
  local candidate="${!override:-}"
  if pg_config_is_usable_for_major "${candidate}" "${major}"; then
    printf '%s\n' "${candidate}"
    return 0
  fi

  candidate="$(command -v pg_config 2>/dev/null || true)"
  if pg_config_is_usable_for_major "${candidate}" "${major}"; then
    printf '%s\n' "${candidate}"
    return 0
  fi

  for candidate in \
    "/opt/homebrew/opt/postgresql@${major}/bin/pg_config" \
    "/usr/local/opt/postgresql@${major}/bin/pg_config" \
    "/usr/lib/postgresql/${major}/bin/pg_config" \
    "/opt/homebrew/opt/libpq/bin/pg_config" \
    "/usr/local/opt/libpq/bin/pg_config"
  do
    if pg_config_is_usable_for_major "${candidate}" "${major}"; then
      printf '%s\n' "${candidate}"
      return 0
    fi
  done

  return 1
}

pg_config_is_usable_for_major() {
  local candidate="$1"
  local major="$2"
  local version
  local found_major
  local bindir

  [[ -n "${candidate}" && -x "${candidate}" ]] || return 1
  version="$("${candidate}" --version 2>/dev/null || true)"
  found_major="$(printf '%s\n' "${version}" | sed -E 's/.*PostgreSQL ([0-9]+).*/\1/')"
  [[ "${found_major}" == "${major}" ]] || return 1

  bindir="$("${candidate}" --bindir 2>/dev/null || true)"
  [[ -n "${bindir}" && -x "${bindir}/postgres" && -x "${bindir}/initdb" ]]
}

repo_relative_path() {
  local path="$1"

  case "${path}" in
    "${REPO_ROOT}"/*) printf '%s\n' "${path#"${REPO_ROOT}/"}" ;;
    *) printf '%s\n' "${path}" ;;
  esac
}

append_result() {
  local major="$1"
  local gate="$2"
  local status="$3"
  local exit_code="$4"
  local started="$5"
  local finished="$6"
  local log_file="$7"
  local command="$8"
  local log_bytes=0
  local log_path

  if [[ -f "${log_file}" ]]; then
    log_bytes="$(wc -c <"${log_file}" | tr -d ' ')"
  fi
  log_path="$(repo_relative_path "${log_file}")"
  printf 'pg%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${major}" "${gate}" "${status}" "${exit_code}" "${started}" "${finished}" \
    "${pg_config_version:-unknown}" "${log_path}" "${command}" "${log_bytes}" >>"${summary_tsv}"
  printf '| pg%s | `%s` | `%s` | `%s` | `%s` | `%s` |\n' \
    "${major}" "${gate}" "${status}" "${exit_code}" \
    "${pg_config_version:-unknown}" "${log_path}" >>"${report_md}"

  total_rows=$((total_rows + 1))
  case "${status}" in
    passed) passed_rows=$((passed_rows + 1)) ;;
    dry-run) dry_run_rows=$((dry_run_rows + 1)) ;;
    skipped) skipped_rows=$((skipped_rows + 1)) ;;
    failed) failed_rows=$((failed_rows + 1)) ;;
    missing) missing_rows=$((missing_rows + 1)) ;;
  esac
}

run_gate() {
  local major="$1"
  local gate="$2"
  local command="$3"
  local log_file="$4"
  shift 4

  local started
  local finished
  local status="passed"
  local exit_code=0

  started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if [[ "${dry_run}" -eq 1 ]]; then
    status="dry-run"
    {
      printf 'major: pg%s\n' "${major}"
      printf 'gate: %s\n' "${gate}"
      printf 'pg_config: %s\n' "${pg_config}"
      printf 'pg_config version: %s\n' "${pg_config_version}"
      printf 'command: %s\n' "${command}"
      printf 'dry run: %s\n' "${command}"
    } >"${log_file}"
  else
    {
      printf 'major: pg%s\n' "${major}"
      printf 'gate: %s\n' "${gate}"
      printf 'pg_config: %s\n' "${pg_config}"
      printf 'pg_config version: %s\n' "${pg_config_version}"
      printf 'command: %s\n\n' "${command}"
      cd "${REPO_ROOT}"
      "$@"
    } >"${log_file}" 2>&1 || {
      exit_code=$?
      status="failed"
      overall_status=1
    }
    if [[ "${status}" == "passed" && "${gate}" == "heavy:upgrade_matrix" ]]; then
      if grep -q "No previous pgcontext SQL versions are present" "${log_file}"; then
        status="skipped"
        printf '\nupgrade_from_previous: skipped; no previous SQL versions are present\n' >>"${log_file}"
      elif ! grep -Eq '^upgrade_path_exercised: [^[:space:]]+ -> [^[:space:]]+$' "${log_file}"; then
        status="failed"
        exit_code=1
        overall_status=1
        printf '\nupgrade_from_previous: failed; no upgrade_path_exercised marker found\n' >>"${log_file}"
      elif ! grep -qxF 'rollback_path_exercised: failed_update_probe -> current_catalog_validated' "${log_file}"; then
        status="failed"
        exit_code=1
        overall_status=1
        printf '\nupgrade_from_previous: failed; no rollback_path_exercised marker found\n' >>"${log_file}"
      fi
    fi
    if [[ "${status}" == "passed" && "${gate}" == "heavy:mmap_hnsw_artifact_restart" ]]; then
      local marker
      for marker in "${MMAP_HNSW_RESTART_MARKERS[@]}"; do
        if ! grep -qxF "${marker}" "${log_file}"; then
          status="failed"
          exit_code=1
          overall_status=1
          printf '\nmmap_hnsw_artifact_restart: failed; missing evidence marker: %s\n' "${marker}" >>"${log_file}"
          break
        fi
      done
    fi
    if [[ "${status}" == "passed" && "${gate}" == "heavy:hnsw_vacuum" ]]; then
      local marker
      for marker in "${HNSW_VACUUM_MARKERS[@]}"; do
        if ! grep -qxF "${marker}" "${log_file}"; then
          status="failed"
          exit_code=1
          overall_status=1
          printf '\nhnsw_vacuum: failed; missing evidence marker: %s\n' "${marker}" >>"${log_file}"
          break
        fi
      done
    fi
    if [[ "${status}" == "passed" && "${gate}" == "heavy:filtered_ann_recall" ]]; then
      local marker
      for marker in "${FILTERED_ANN_RECALL_MARKERS[@]}"; do
        if ! grep -qxF "${marker}" "${log_file}"; then
          status="failed"
          exit_code=1
          overall_status=1
          printf '\nfiltered_ann_recall: failed; missing evidence marker: %s\n' "${marker}" >>"${log_file}"
          break
        fi
      done
    fi
    if [[ "${status}" == "passed" && "${gate}" == "heavy:late_interaction_ann_serving" ]]; then
      local marker
      for marker in "${LATE_INTERACTION_ANN_SERVING_MARKERS[@]}"; do
        if ! grep -qxF "${marker}" "${log_file}"; then
          status="failed"
          exit_code=1
          overall_status=1
          printf '\nlate_interaction_ann_serving: failed; missing evidence marker: %s\n' "${marker}" >>"${log_file}"
          break
        fi
      done
    fi
    if [[ "${status}" == "passed" && "${gate}" == "heavy:build_job_resumability" ]]; then
      local marker
      for marker in "${BUILD_JOB_RESUMABILITY_MARKERS[@]}"; do
        if ! grep -qxF "${marker}" "${log_file}"; then
          status="failed"
          exit_code=1
          overall_status=1
          printf '\nbuild_job_resumability: failed; missing evidence marker: %s\n' "${marker}" >>"${log_file}"
          break
        fi
      done
    fi
    if [[ "${status}" == "passed" && "${gate}" == "heavy:crash_restart_hnsw" ]]; then
      local marker
      for marker in "${HNSW_RESTART_MARKERS[@]}"; do
        if ! grep -qxF "${marker}" "${log_file}"; then
          status="failed"
          exit_code=1
          overall_status=1
          printf '\ncrash_restart_hnsw: failed; missing evidence marker: %s\n' "${marker}" >>"${log_file}"
          break
        fi
      done
    fi
    if [[ "${status}" == "passed" && "${gate}" == "heavy:mapped_hnsw_lifecycle_cleanup" ]]; then
      local marker
      for marker in "${MAPPED_HNSW_LIFECYCLE_MARKERS[@]}"; do
        if ! grep -qxF "${marker}" "${log_file}"; then
          status="failed"
          exit_code=1
          overall_status=1
          printf '\nmapped_hnsw_lifecycle_cleanup: failed; missing evidence marker: %s\n' "${marker}" >>"${log_file}"
          break
        fi
      done
    fi
    if [[ "${status}" == "passed" && "${gate}" == "heavy:concurrent_read_write" ]]; then
      local marker
      for marker in "${CONCURRENT_READ_WRITE_MARKERS[@]}"; do
        if ! grep -qxF "${marker}" "${log_file}"; then
          status="failed"
          exit_code=1
          overall_status=1
          printf '\nconcurrent_read_write: failed; missing evidence marker: %s\n' "${marker}" >>"${log_file}"
          break
        fi
      done
    fi
    if [[ "${status}" == "passed" && "${gate}" == "heavy:large_exact_search" ]]; then
      local marker
      for marker in "${LARGE_EXACT_SEARCH_MARKERS[@]}"; do
        if ! grep -qxF "${marker}" "${log_file}"; then
          status="failed"
          exit_code=1
          overall_status=1
          printf '\nlarge_exact_search: failed; missing evidence marker: %s\n' "${marker}" >>"${log_file}"
          break
        fi
      done
    fi
    if [[ "${status}" == "passed" && "${gate}" == "heavy:partitioned_collections" ]]; then
      local marker
      for marker in "${PARTITIONED_COLLECTIONS_MARKERS[@]}"; do
        if ! grep -qxF "${marker}" "${log_file}"; then
          status="failed"
          exit_code=1
          overall_status=1
          printf '\npartitioned_collections: failed; missing evidence marker: %s\n' "${marker}" >>"${log_file}"
          break
        fi
      done
    fi
    if [[ "${status}" == "passed" && "${gate}" == "heavy:low_memory_build" ]]; then
      local marker
      for marker in "${LOW_MEMORY_BUILD_MARKERS[@]}"; do
        if ! grep -qxF "${marker}" "${log_file}"; then
          status="failed"
          exit_code=1
          overall_status=1
          printf '\nlow_memory_build: failed; missing evidence marker: %s\n' "${marker}" >>"${log_file}"
          break
        fi
      done
    fi
    if [[ "${status}" == "passed" && "${gate}" == "heavy:backup_restore" ]]; then
      local marker
      for marker in "${BACKUP_RESTORE_MARKERS[@]}"; do
        if ! grep -qxF "${marker}" "${log_file}"; then
          status="failed"
          exit_code=1
          overall_status=1
          printf '\nbackup_restore: failed; missing evidence marker: %s\n' "${marker}" >>"${log_file}"
          break
        fi
      done
    fi
    if [[ "${status}" == "passed" && "${gate}" == "heavy:physical_backup_wal_replay" ]]; then
      local marker
      if ! marker="$(physical_backup_wal_replay_marker_order_failure "${log_file}")"; then
        status="failed"
        exit_code=1
        overall_status=1
        printf '\nphysical_backup_wal_replay: failed; missing ordered evidence marker: %s\n' "${marker}" >>"${log_file}"
      fi
    fi
  fi
  finished="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if [[ "${status}" == "passed" && "${gate}" == "schema" && -f "${schema_file:-}" ]]; then
    shasum -a 256 "${schema_file}" >>"${log_file}"
  fi
  if [[ "${gate}" == "heavy:upgrade_matrix" && "${status}" == "passed" ]]; then
    printf 'upgrade_from_previous: passed\n' >>"${log_file}"
  fi
  {
    printf 'matrix gate status: %s\n' "${status}"
    printf 'matrix gate exit code: %s\n' "${exit_code}"
  } >>"${log_file}"
  append_result "${major}" "${gate}" "${status}" "${exit_code}" "${started}" "${finished}" "${log_file}" "${command}"
}

for major in "${majors[@]}"; do
  if ! [[ "${major}" =~ ^[0-9]+$ ]] || ! is_supported_major "${major}"; then
    echo "unsupported PostgreSQL major: ${major}" >&2
    exit 2
  fi
done
for ((i = 0; i < ${#majors[@]}; i++)); do
  for ((j = i + 1; j < ${#majors[@]}; j++)); do
    if [[ "${majors[$i]}" == "${majors[$j]}" ]]; then
      echo "duplicate PostgreSQL major: ${majors[$i]}" >&2
      exit 2
    fi
  done
done

full_release_scope=0
if [[ "${mode}" == "all" && "${#majors[@]}" -eq "${#SUPPORTED_MAJORS[@]}" ]]; then
  full_release_scope=1
fi

mkdir -p "${out_dir}"
summary_tsv="${out_dir}/summary.tsv"
report_md="${out_dir}/report.md"
git_sha="unknown"
if git -C "${REPO_ROOT}" rev-parse --verify HEAD >/dev/null 2>&1; then
  git_sha="$(git -C "${REPO_ROOT}" rev-parse --verify HEAD)"
fi
host_os="$(uname -srm)"
rustc_version="$(rustc -V 2>/dev/null || printf 'unavailable')"
cargo_version="$(cargo -V 2>/dev/null || printf 'unavailable')"
cargo_pgrx_version="$(cargo pgrx --version 2>/dev/null || printf 'unavailable')"
started_all="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
worktree_state="unknown"
if git -C "${REPO_ROOT}" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  if [[ -z "$(git -C "${REPO_ROOT}" status --short)" ]]; then
    worktree_state="clean"
  else
    worktree_state="dirty"
  fi
fi

{
  printf 'major\tgate\tstatus\texit_code\tstarted_utc\tfinished_utc\tpg_config_version\tlog\tcommand\tlog_bytes\n'
} >"${summary_tsv}"

{
  printf '# PostgreSQL Matrix Gate Report\n\n'
  printf -- '- Commit: `%s`\n' "${git_sha}"
  printf -- '- Worktree: `%s`\n' "${worktree_state}"
  printf -- '- Host: `%s`\n' "${host_os}"
  printf -- '- Rust: `%s`\n' "${rustc_version}"
  printf -- '- Cargo: `%s`\n' "${cargo_version}"
  printf -- '- Cargo pgrx: `%s`\n' "${cargo_pgrx_version}"
  printf -- '- Started: `%s`\n' "${started_all}"
  printf -- '- Mode: `%s`\n' "${mode}"
  if [[ "${dry_run}" -eq 1 ]]; then
    printf -- '- Execution: `dry-run`\n'
  else
    printf -- '- Execution: `run`\n'
  fi
  printf '\n| Major | Gate | Status | Exit | pg_config | Log |\n'
  printf '|---|---|---|---:|---|---|\n'
} >"${report_md}"

overall_status=0
total_rows=0
passed_rows=0
dry_run_rows=0
skipped_rows=0
failed_rows=0
missing_rows=0
for major in "${majors[@]}"; do
  pg_feature="pg${major}"
  pg_port="288${major}"
  pg_config=""
  pg_config_version="missing"
  if ! pg_config="$(pg_config_for_major "${major}")"; then
    log_file="${out_dir}/pg${major}-missing.log"
    printf 'missing pg_config for PostgreSQL %s\n' "${major}" >"${log_file}"
    started="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    finished="${started}"
    missing_status="missing"
    missing_exit_code="2"
    if [[ "${allow_missing}" -eq 1 || "${dry_run}" -eq 1 ]]; then
      missing_status="skipped"
      missing_exit_code="0"
    fi
    append_result "${major}" "pg_config" "${missing_status}" "${missing_exit_code}" "${started}" "${finished}" "${log_file}" "pg_config lookup"
    if [[ "${allow_missing}" -eq 0 && "${dry_run}" -eq 0 ]]; then
      overall_status=1
    fi
    continue
  fi
  pg_config_version="$("${pg_config}" --version 2>/dev/null || printf 'unavailable')"

  if [[ "${mode}" == "fast" || "${mode}" == "schema" || "${mode}" == "pgrx" || "${mode}" == "heavy" || "${mode}" == "all" ]]; then
    log_file="${out_dir}/pg${major}-pgrx-init.log"
    command="cargo pgrx init --pg${major} ${pg_config}"
    run_gate "${major}" "pgrx-init" "${command}" "${log_file}" \
      cargo pgrx init "--pg${major}" "${pg_config}"
  fi

  if [[ "${mode}" == "fast" || "${mode}" == "all" ]]; then
    log_file="${out_dir}/pg${major}-workspace-fast.log"
    command="cargo test --workspace --exclude context-pg --all-features"
    run_gate "${major}" "workspace-fast" "${command}" "${log_file}" \
      cargo test --workspace --exclude context-pg --all-features

    log_file="${out_dir}/pg${major}-context-pg-check.log"
    command="cargo check -p context-pg --no-default-features --features ${pg_feature}"
    run_gate "${major}" "context-pg-check" "${command}" "${log_file}" \
      env PG_CONFIG="${pg_config}" PG_CONFIG_VERSION="${pg_config_version}" cargo check -p context-pg --no-default-features --features "${pg_feature}"

    log_file="${out_dir}/pg${major}-context-pg-test.log"
    command="cargo test -p context-pg --no-default-features --features ${pg_feature}"
    run_gate "${major}" "context-pg-test" "${command}" "${log_file}" \
      env PG_CONFIG="${pg_config}" PG_CONFIG_VERSION="${pg_config_version}" cargo test -p context-pg --no-default-features --features "${pg_feature}"
  fi

  if [[ "${mode}" == "schema" || "${mode}" == "all" ]]; then
    schema_file="${out_dir}/pg${major}.sql"
    log_file="${out_dir}/pg${major}-schema.log"
    command="cargo pgrx schema -p context-pg pg${major} --out ${schema_file}"
    run_gate "${major}" "schema" "${command}" "${log_file}" \
      cargo pgrx schema -p context-pg "pg${major}" --out "${schema_file}"
  fi

  if [[ "${mode}" == "pgrx" || "${mode}" == "all" ]]; then
    log_file="${out_dir}/pg${major}-pgrx.log"
    command="cargo pgrx test --release -p context-pg pg${major}"
    run_gate "${major}" "pgrx" "${command}" "${log_file}" \
      cargo pgrx test --release -p context-pg "pg${major}"
  fi

  if [[ "${mode}" == "heavy" || "${mode}" == "all" ]]; then
    for heavy_gate in "${HEAVY_GATES[@]}"; do
      heavy_name="$(basename "${heavy_gate}" .sh)"
      log_file="${out_dir}/pg${major}-heavy-${heavy_name}.log"
      command="PG_VERSION=pg${major} PG_FEATURE=${pg_feature} PG_CONFIG=${pg_config} PGPORT=${pg_port} ${heavy_gate}"
      run_gate "${major}" "heavy:${heavy_name}" "${command}" "${log_file}" \
        env PG_VERSION="pg${major}" PG_FEATURE="${pg_feature}" PG_CONFIG="${pg_config}" PGPORT="${pg_port}" "${REPO_ROOT}/${heavy_gate}"
    done
  fi
done

{
  printf '\n## Summary\n\n'
  printf -- '- Rows: `%s`\n' "${total_rows}"
  printf -- '- Passed: `%s`\n' "${passed_rows}"
  printf -- '- Dry-run: `%s`\n' "${dry_run_rows}"
  printf -- '- Skipped: `%s`\n' "${skipped_rows}"
  printf -- '- Failed: `%s`\n' "${failed_rows}"
  printf -- '- Missing: `%s`\n' "${missing_rows}"
  printf -- '- Full release scope: `%s`\n' "${full_release_scope}"
  if [[ "${full_release_scope}" -ne 1 ||
        "${dry_run_rows}" -gt 0 ||
        "${skipped_rows}" -gt 0 ||
        "${failed_rows}" -gt 0 ||
        "${missing_rows}" -gt 0 ||
        "${worktree_state}" != "clean" ]]; then
    printf -- '- Approval: `incomplete`\n'
    printf -- '- Approval note: PostgreSQL matrix release evidence requires `--mode all`, every supported PostgreSQL major exactly once, a clean worktree, no dry-run rows, no skipped rows, no failures, and no missing `pg_config` rows.\n'
  else
    printf -- '- Approval: `complete`\n'
  fi
  printf '\nSummary TSV: `%s`\n' "$(repo_relative_path "${summary_tsv}")"
} >>"${report_md}"
printf 'postgres matrix report: %s\n' "${report_md}"
exit "${overall_status}"
