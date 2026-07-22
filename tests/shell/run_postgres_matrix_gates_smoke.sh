#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TMPDIR="${TMPDIR:-${REPO_ROOT}/target/tmp}"
mkdir -p "${TMPDIR}"
work_dir="$(mktemp -d "${TMPDIR}/postgres-matrix-test.XXXXXX")"
fake_bin="${work_dir}/pg17/bin"
default_bin="${work_dir}/default/bin"
client_only_bin="${work_dir}/client-only/bin"
mkdir -p "${fake_bin}" "${default_bin}" "${client_only_bin}"
trap 'rm -rf "${work_dir}"' EXIT

# Derived from the runner's own HEAVY_GATES array rather than duplicated here:
# a gate added there but not staged as a fixture surfaces only as a confusing
# `exit 127` from the missing file, which is exactly how
# artifact_publication_rollback went unnoticed.
heavy_gate_names=()
while IFS= read -r heavy_gate_name; do
  heavy_gate_names+=("${heavy_gate_name}")
done < <(
  sed -n 's|^  tests/heavy/\([a-z0-9_]*\)\.sh$|\1|p' \
    "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh"
)
if [[ "${#heavy_gate_names[@]}" -eq 0 ]]; then
  echo "could not derive HEAVY_GATES from scripts/run-postgres-matrix-gates.sh" >&2
  exit 1
fi

stage_passing_heavy_fixtures() {
  local root="$1"
  local gate
  mkdir -p "${root}/tests/heavy"
  for gate in "${heavy_gate_names[@]}"; do
    cat >"${root}/tests/heavy/${gate}.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'fake heavy gate passed: %s\n' "$(basename "$0")"
SH
    chmod +x "${root}/tests/heavy/${gate}.sh"
  done
}

cat >"${fake_bin}/pg_config" <<'PG_CONFIG'
#!/usr/bin/env bash
case "${1:-}" in
  --bindir) dirname "$0" ;;
  *) printf 'PostgreSQL 17.99-test\n' ;;
esac
PG_CONFIG
chmod +x "${fake_bin}/pg_config"
touch "${fake_bin}/postgres" "${fake_bin}/initdb"
chmod +x "${fake_bin}/postgres" "${fake_bin}/initdb"

cat >"${fake_bin}/cargo" <<'CARGO'
#!/usr/bin/env bash
set -euo pipefail

printf '%s\n' "$*" >>"${FAKE_CARGO_LOG}"

if [[ "$*" == "-V" ]]; then
  printf 'cargo 1.96.0-test\n'
  exit 0
fi

if [[ "$*" == "pgrx --version" ]]; then
  printf 'cargo-pgrx 0.16.0-test\n'
  exit 0
fi

case "${1:-}" in
  pgrx)
    if [[ "${2:-}" == "init" ]]; then
      printf 'fake pgrx init\n'
      exit 0
    fi
    if [[ "${2:-}" == "schema" ]]; then
      out_file=""
      while [[ $# -gt 0 ]]; do
        if [[ "${1:-}" == "--out" ]]; then
          out_file="${2:-}"
          break
        fi
        shift
      done
      if [[ -n "${out_file}" ]]; then
        mkdir -p "$(dirname "${out_file}")"
        printf 'fake schema\n' >"${out_file}"
      fi
      printf 'fake pgrx schema\n'
      exit 0
    fi
    if [[ "${2:-}" == "test" ]]; then
      printf 'fake pgrx test\n'
      exit 0
    fi
    ;;
  test | check)
    printf 'fake cargo %s\n' "${1}"
    exit 0
    ;;
esac

printf 'unexpected cargo invocation: %s\n' "$*" >&2
exit 127
CARGO
chmod +x "${fake_bin}/cargo"

cat >"${default_bin}/pg_config" <<'PG_CONFIG'
#!/usr/bin/env bash
case "${1:-}" in
  --bindir) dirname "$0" ;;
  *) printf 'PostgreSQL 18.99-test\n' ;;
esac
PG_CONFIG
chmod +x "${default_bin}/pg_config"
touch "${default_bin}/postgres" "${default_bin}/initdb"
chmod +x "${default_bin}/postgres" "${default_bin}/initdb"

cat >"${client_only_bin}/pg_config" <<'PG_CONFIG'
#!/usr/bin/env bash
case "${1:-}" in
  --bindir) dirname "$0" ;;
  *) printf 'PostgreSQL 16.99-client-only\n' ;;
esac
PG_CONFIG
chmod +x "${client_only_bin}/pg_config"

PG17_CONFIG="${fake_bin}/pg_config" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --dry-run \
    --major 17 \
    --mode all \
    --out-dir "${work_dir}/report"

summary="${work_dir}/report/summary.tsv"
report="${work_dir}/report/report.md"

if [[ ! -f "${summary}" ]]; then
  echo "missing summary TSV: ${summary}" >&2
  exit 1
fi
if [[ ! -f "${report}" ]]; then
  echo "missing report: ${report}" >&2
  exit 1
fi
head -n 1 "${summary}" | grep -q $'major\tgate\tstatus\texit_code\tstarted_utc\tfinished_utc\tpg_config_version\tlog\tcommand\tlog_bytes'
grep -q -- '- Mode: `all`' "${report}"
grep -q -- '- Execution: `dry-run`' "${report}"
grep -Eq -- '^- Started: `[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z`$' "${report}"

expected_rows=$((3 + 1 + 1 + 1 + 21))
actual_rows="$(tail -n +2 "${summary}" | wc -l | tr -d ' ')"
if [[ "${actual_rows}" != "${expected_rows}" ]]; then
  echo "expected ${expected_rows} dry-run rows, got ${actual_rows}" >&2
  exit 1
fi

assert_row() {
  local gate="$1"
  local command="$2"
  local log_file="${work_dir}/report/pg17-${gate}.log"

  grep -qF $'pg17\t'"${gate}"$'\tdry-run\t0' "${summary}"
  grep -qF "PostgreSQL 17.99-test" "${summary}"
  grep -qF "${command}" "${summary}"
  awk -F '\t' -v gate="${gate}" '
    $1 == "pg17" && $2 == gate && $10 ~ /^[1-9][0-9]*$/ { found = 1 }
    END { exit(found ? 0 : 1) }
  ' "${summary}"
  if [[ ! -f "${log_file}" ]]; then
    echo "missing dry-run log for ${gate}: ${log_file}" >&2
    exit 1
  fi
  grep -qF "major: pg17" "${log_file}"
  grep -qF "gate: ${gate}" "${log_file}"
  grep -qF "pg_config version: PostgreSQL 17.99-test" "${log_file}"
  grep -qF "command: ${command}" "${log_file}"
  grep -qF "dry run: ${command}" "${log_file}"
  grep -qF "pg_config: ${fake_bin}/pg_config" "${log_file}"
  grep -qF "matrix gate status: dry-run" "${log_file}"
  grep -qF "matrix gate exit code: 0" "${log_file}"
}

assert_heavy_row() {
  local heavy_name="$1"
  local command="$2"
  local log_file="${work_dir}/report/pg17-heavy-${heavy_name}.log"

  grep -qF $'pg17\theavy:'"${heavy_name}"$'\tdry-run\t0' "${summary}"
  grep -qF "PostgreSQL 17.99-test" "${summary}"
  grep -qF "${command}" "${summary}"
  awk -F '\t' -v gate="heavy:${heavy_name}" '
    $1 == "pg17" && $2 == gate && $10 ~ /^[1-9][0-9]*$/ { found = 1 }
    END { exit(found ? 0 : 1) }
  ' "${summary}"
  if [[ ! -f "${log_file}" ]]; then
    echo "missing executed log for ${gate}: ${log_file}" >&2
    exit 1
  fi
  grep -qF "major: pg17" "${log_file}"
  grep -qF "gate: heavy:${heavy_name}" "${log_file}"
  grep -qF "pg_config version: PostgreSQL 17.99-test" "${log_file}"
  grep -qF "command: ${command}" "${log_file}"
  grep -qF "dry run: ${command}" "${log_file}"
  grep -qF "pg_config: ${fake_bin}/pg_config" "${log_file}"
  grep -qF "matrix gate status: dry-run" "${log_file}"
  grep -qF "matrix gate exit code: 0" "${log_file}"
}

assert_row "workspace-fast" "cargo test --workspace --exclude context-pg --all-features"
assert_row "context-pg-check" "cargo check -p context-pg --no-default-features --features pg17"
assert_row "context-pg-test" "cargo test -p context-pg --no-default-features --features pg17"
assert_row "pgrx-init" "cargo pgrx init --pg17 ${fake_bin}/pg_config"
assert_row "schema" "cargo pgrx schema -p context-pg pg17 --out ${work_dir}/report/pg17.sql"
assert_row "pgrx" "cargo pgrx test --release -p context-pg pg17"
assert_heavy_row \
  "fresh_install_smoke" \
  "PG_VERSION=pg17 PG_FEATURE=pg17 PG_CONFIG=${fake_bin}/pg_config PGPORT=28817 tests/heavy/fresh_install_smoke.sh"
assert_heavy_row \
  "late_interaction_ann_serving" \
  "PG_VERSION=pg17 PG_FEATURE=pg17 PG_CONFIG=${fake_bin}/pg_config PGPORT=28817 tests/heavy/late_interaction_ann_serving.sh"
assert_heavy_row \
  "build_job_resumability" \
  "PG_VERSION=pg17 PG_FEATURE=pg17 PG_CONFIG=${fake_bin}/pg_config PGPORT=28817 tests/heavy/build_job_resumability.sh"
assert_heavy_row \
  "sqlstate_contract" \
  "PG_VERSION=pg17 PG_FEATURE=pg17 PG_CONFIG=${fake_bin}/pg_config PGPORT=28817 tests/heavy/sqlstate_contract.sh"

write_mmap_hnsw_restart_fixture() {
  local script_path="$1"
  cat >"${script_path}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'mmap_artifact_serving_ready: before_restart\n'
printf 'mmap_artifact_source_recheck: before_restart\n'
printf 'mmap_artifact_budget_rejected: before_restart\n'
printf 'mmap_artifact_serving_ready: after_restart\n'
printf 'mmap_artifact_source_recheck: after_restart\n'
printf 'mmap_artifact_budget_rejected: after_restart\n'
printf 'mmap_artifact_vacuum_recheck: after_restart\n'
printf 'fake mmap restart gate passed\n'
SH
  chmod +x "${script_path}"
}

write_crash_restart_hnsw_fixture() {
  local script_path="$1"
  cat >"${script_path}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'hnsw_restart_nearest_rechecked: before_restart\n'
printf 'hnsw_restart_index_scan: before_restart\n'
printf 'hnsw_mapped_attach: before_restart\n'
printf 'hnsw_restart_nearest_rechecked: after_restart\n'
printf 'hnsw_restart_index_scan: after_restart\n'
printf 'hnsw_mapped_attach: after_restart\n'
printf 'fake HNSW crash restart gate passed\n'
SH
  chmod +x "${script_path}"
}

write_mapped_hnsw_lifecycle_fixture() {
  local script_path="$1"
  cat >"${script_path}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'mapped_hnsw_drop_rollback_preserved\n'
printf 'mapped_hnsw_drop_index_reclaimed\n'
printf 'mapped_hnsw_drop_table_reclaimed\n'
printf 'mapped_hnsw_prepared_commit_reconciled\n'
printf 'mapped_hnsw_prepared_abort_preserved\n'
printf 'mapped_hnsw_drop_marker_restart_preserved\n'
printf 'mapped_hnsw_concurrent_cursor_progressed\n'
printf 'mapped_hnsw_current_temps_reclaimed\n'
printf 'mapped_hnsw_stale_temps_do_not_starve\n'
printf 'mapped_hnsw_reconcile_window_advanced\n'
printf 'mapped_hnsw_temp_drop_reclaimed\n'
printf 'mapped_hnsw_temp_teardown_reclaimed\n'
printf 'mapped_hnsw_drop_database_reclaimed\n'
printf 'fake mapped HNSW lifecycle gate passed\n'
SH
  chmod +x "${script_path}"
}

write_concurrent_read_write_fixture() {
  local script_path="$1"
  cat >"${script_path}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'concurrent_hnsw_writer_completed\n'
printf 'concurrent_hnsw_reader_completed\n'
printf 'concurrent_hnsw_row_count_verified\n'
printf 'concurrent_hnsw_insert_visible\n'
printf 'fake concurrent read/write gate passed\n'
SH
  chmod +x "${script_path}"
}

write_hnsw_vacuum_fixture() {
  local script_path="$1"
  cat >"${script_path}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'hnsw_vacuum_nearest_rechecked\n'
printf 'hnsw_vacuum_deleted_rows_pruned\n'
printf 'hnsw_vacuum_index_ready\n'
printf 'hnsw_vacuum_advice_present\n'
printf 'hnsw_reindex_ready\n'
printf 'fake HNSW vacuum gate passed\n'
SH
  chmod +x "${script_path}"
}

write_filtered_ann_recall_fixture() {
  local script_path="$1"
  cat >"${script_path}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'filtered_ann_exact_oracle_seqscan\n'
printf 'filtered_ann_candidate_indexscan\n'
printf 'filtered_ann_exact_candidates_match\n'
printf 'filtered_ann_recall_passing\n'
printf 'filtered_ann_tenant_recheck_passing\n'
printf 'filtered_ann_no_match_empty\n'
printf 'fake filtered ANN recall gate passed\n'
SH
  chmod +x "${script_path}"
}

write_late_interaction_ann_serving_fixture() {
  local script_path="$1"
  cat >"${script_path}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'late_interaction_ann_candidates_deduped\n'
printf 'late_interaction_ann_exact_rerank_scores\n'
printf 'late_interaction_ann_source_recheck\n'
printf 'late_interaction_ann_deleted_recheck\n'
printf 'late_interaction_ann_budget_rejected\n'
printf 'fake late-interaction ANN serving gate passed\n'
SH
  chmod +x "${script_path}"
}

write_build_job_resumability_fixture() {
  local script_path="$1"
  cat >"${script_path}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'build_job_progress_preserved\n'
printf 'build_job_source_recheck_after_update\n'
printf 'build_job_abandoned_owner_recovered\n'
printf 'build_job_final_serving_ready_view_a\n'
printf 'build_job_final_serving_ready_view_b\n'
printf 'build_job_source_recheck_after_restart\n'
printf 'build_job_vacuum_source_recheck\n'
printf 'fake build-job resumability gate passed\n'
SH
  chmod +x "${script_path}"
}

write_large_exact_search_fixture() {
  local script_path="$1"
  cat >"${script_path}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'NOTICE:  large_exact_rows_loaded\n'
printf 'NOTICE:  large_exact_oracle_match\n'
printf 'NOTICE:  large_exact_missing_filter_empty\n'
printf 'NOTICE:  large_exact_dimension_mismatch_rejected\n'
printf 'NOTICE:  large_exact_unknown_filter_rejected\n'
printf 'fake large exact search gate passed\n'
SH
  chmod +x "${script_path}"
}

write_partitioned_collections_fixture() {
  local script_path="$1"
  cat >"${script_path}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'NOTICE:  partitioned_exact_order_verified\n'
printf 'NOTICE:  partitioned_tenant_filter_verified\n'
printf 'NOTICE:  partitioned_count_verified\n'
printf 'NOTICE:  partitioned_facet_verified\n'
printf 'NOTICE:  partitioned_delete_visibility_verified\n'
printf 'NOTICE:  partitioned_drop_recheck_verified\n'
printf 'NOTICE:  partitioned_unknown_filter_rejected\n'
printf 'fake partitioned collections gate passed\n'
SH
  chmod +x "${script_path}"
}

write_low_memory_build_fixture() {
  local script_path="$1"
  cat >"${script_path}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'NOTICE:  low_memory_rejected_bad_build\n'
printf 'NOTICE:  low_memory_failed_build_cleaned\n'
printf 'NOTICE:  low_memory_index_order_verified\n'
printf 'NOTICE:  low_memory_reltuples_verified\n'
printf 'fake low-memory build gate passed\n'
SH
  chmod +x "${script_path}"
}

write_backup_restore_fixture() {
  local script_path="$1"
  cat >"${script_path}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'backup_restore_dump_created\n'
printf 'backup_restore_restore_completed\n'
printf 'NOTICE:  backup_restore_nearest_verified\n'
printf 'NOTICE:  backup_restore_filter_verified\n'
printf 'NOTICE:  backup_restore_jsonb_facet_verified\n'
printf 'NOTICE:  backup_restore_scroll_verified\n'
printf 'NOTICE:  backup_restore_model_versions_verified\n'
printf 'NOTICE:  backup_restore_migration_verified\n'
printf 'NOTICE:  backup_restore_telemetry_verified\n'
printf 'NOTICE:  backup_restore_query_stats_verified\n'
printf 'NOTICE:  backup_restore_hnsw_ready\n'
printf 'fake backup/restore gate passed\n'
SH
  chmod +x "${script_path}"
}

write_physical_backup_wal_replay_fixture() {
  local script_path="$1"
  cat >"${script_path}" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'physical_backup_basebackup_created\n'
printf 'NOTICE:  physical_backup_exact_nearest_verified: before_replay\n'
printf 'NOTICE:  physical_backup_indexed_nearest_verified: before_replay\n'
printf 'NOTICE:  physical_backup_exact_scroll_verified: before_replay\n'
printf 'NOTICE:  physical_backup_indexed_scroll_verified: before_replay\n'
printf 'NOTICE:  physical_backup_hnsw_ready: before_replay\n'
printf 'physical_backup_restarted_after_writes\n'
printf 'NOTICE:  physical_backup_exact_nearest_verified: after_replay\n'
printf 'NOTICE:  physical_backup_indexed_nearest_verified: after_replay\n'
printf 'NOTICE:  physical_backup_exact_scroll_verified: after_replay\n'
printf 'NOTICE:  physical_backup_indexed_scroll_verified: after_replay\n'
printf 'NOTICE:  physical_backup_hnsw_ready: after_replay\n'
printf 'fake physical backup/WAL replay gate passed\n'
SH
  chmod +x "${script_path}"
}

PATH="${default_bin}:${PATH}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --dry-run \
    --major 18 \
    --mode fast \
    --out-dir "${work_dir}/default-pg-config"
grep -qF $'pg18\tworkspace-fast\tdry-run\t0' "${work_dir}/default-pg-config/summary.tsv"
grep -qF "PostgreSQL 18.99-test" "${work_dir}/default-pg-config/summary.tsv"
grep -qF "pg_config: ${default_bin}/pg_config" \
  "${work_dir}/default-pg-config/pg18-workspace-fast.log"

PATH="${client_only_bin}:${PATH}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
  --dry-run \
  --allow-missing \
  --major 16 \
  --mode fast \
  --out-dir "${work_dir}/client-only"
grep -qF $'pg16\tpg_config\tskipped\t0' "${work_dir}/client-only/summary.tsv"

PATH="${default_bin}:${PATH}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
  --dry-run \
  --allow-missing \
  --major 16 \
  --mode fast \
  --out-dir "${work_dir}/missing"
grep -qF $'pg16\tpg_config\tskipped\t0' "${work_dir}/missing/summary.tsv"
grep -qF -- '- Approval: `incomplete`' "${work_dir}/missing/report.md"
grep -qF -- '- Full release scope: `0`' "${work_dir}/missing/report.md"
grep -qF 'PostgreSQL matrix release evidence requires `--mode all`' "${work_dir}/missing/report.md"

PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/partial-execute-cargo.log" PG17_CONFIG="${fake_bin}/pg_config" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode fast \
    --out-dir "${work_dir}/partial-execute"
grep -qF $'pg17\tpgrx-init\tpassed\t0' "${work_dir}/partial-execute/summary.tsv"
grep -qF $'pg17\tworkspace-fast\tpassed\t0' "${work_dir}/partial-execute/summary.tsv"
grep -qF $'pg17\tcontext-pg-check\tpassed\t0' "${work_dir}/partial-execute/summary.tsv"
grep -qF $'pg17\tcontext-pg-test\tpassed\t0' "${work_dir}/partial-execute/summary.tsv"
grep -qF -- '- Passed: `4`' "${work_dir}/partial-execute/report.md"
grep -qF -- '- Full release scope: `0`' "${work_dir}/partial-execute/report.md"
grep -qF -- '- Approval: `incomplete`' "${work_dir}/partial-execute/report.md"

PATH="${fake_bin}:${PATH}" FAKE_CARGO_LOG="${work_dir}/schema-cargo.log" PG17_CONFIG="${fake_bin}/pg_config" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode schema \
    --out-dir "${work_dir}/schema-execute"
schema_log="${work_dir}/schema-execute/pg17-schema.log"
schema_log_bytes="$(wc -c <"${schema_log}" | tr -d ' ')"
awk -F '\t' -v bytes="${schema_log_bytes}" '
  $1 == "pg17" && $2 == "schema" && $3 == "passed" && $10 == bytes { found = 1 }
  END { exit(found ? 0 : 1) }
' "${work_dir}/schema-execute/summary.tsv"
grep -q 'pg17.sql' "${schema_log}"

heavy_root="${work_dir}/heavy-root"
stage_passing_heavy_fixtures "${heavy_root}"
write_crash_restart_hnsw_fixture "${heavy_root}/tests/heavy/crash_restart_hnsw.sh"
write_mapped_hnsw_lifecycle_fixture "${heavy_root}/tests/heavy/mapped_hnsw_lifecycle_cleanup.sh"
write_mmap_hnsw_restart_fixture "${heavy_root}/tests/heavy/mmap_hnsw_artifact_restart.sh"
write_hnsw_vacuum_fixture "${heavy_root}/tests/heavy/hnsw_vacuum.sh"
write_backup_restore_fixture "${heavy_root}/tests/heavy/backup_restore.sh"
write_physical_backup_wal_replay_fixture "${heavy_root}/tests/heavy/physical_backup_wal_replay.sh"
write_concurrent_read_write_fixture "${heavy_root}/tests/heavy/concurrent_read_write.sh"
write_filtered_ann_recall_fixture "${heavy_root}/tests/heavy/filtered_ann_recall.sh"
write_late_interaction_ann_serving_fixture "${heavy_root}/tests/heavy/late_interaction_ann_serving.sh"
write_build_job_resumability_fixture "${heavy_root}/tests/heavy/build_job_resumability.sh"
write_large_exact_search_fixture "${heavy_root}/tests/heavy/large_exact_search.sh"
write_partitioned_collections_fixture "${heavy_root}/tests/heavy/partitioned_collections.sh"
write_low_memory_build_fixture "${heavy_root}/tests/heavy/low_memory_build.sh"
cat >"${heavy_root}/tests/heavy/upgrade_matrix.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'No previous pgcontext SQL versions are present\n'
printf 'current-version lifecycle coverage still ran\n'
SH
chmod +x "${heavy_root}/tests/heavy/upgrade_matrix.sh"

matrix_pg_configs=()
for major in 15 16 17 18; do
  matrix_bin="${work_dir}/pg${major}-matrix/bin"
  mkdir -p "${matrix_bin}"
  cat >"${matrix_bin}/pg_config" <<SH
#!/usr/bin/env bash
case "\${1:-}" in
  --bindir) dirname "\$0" ;;
  *) printf 'PostgreSQL ${major}.99-test\n' ;;
esac
SH
  chmod +x "${matrix_bin}/pg_config"
  touch "${matrix_bin}/postgres" "${matrix_bin}/initdb"
  chmod +x "${matrix_bin}/postgres" "${matrix_bin}/initdb"
  matrix_pg_configs+=("PG${major}_CONFIG=${matrix_bin}/pg_config")
done

env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-skip-cargo.log" \
  "${matrix_pg_configs[@]}" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --out-dir "${work_dir}/heavy-skip"
grep -qF $'pg15\theavy:fresh_install_smoke\tpassed\t0' "${work_dir}/heavy-skip/summary.tsv"
grep -qF $'pg17\theavy:upgrade_matrix\tskipped\t0' "${work_dir}/heavy-skip/summary.tsv"
grep -qF $'pg18\theavy:sqlstate_contract\tpassed\t0' "${work_dir}/heavy-skip/summary.tsv"
grep -qF -- '- Full release scope: `1`' "${work_dir}/heavy-skip/report.md"
grep -qF -- '- Passed: `104`' "${work_dir}/heavy-skip/report.md"
grep -qF -- '- Skipped: `4`' "${work_dir}/heavy-skip/report.md"
grep -qF -- '- Approval: `incomplete`' "${work_dir}/heavy-skip/report.md"
grep -qF 'major: pg17' "${work_dir}/heavy-skip/pg17-heavy-upgrade_matrix.log"
grep -qF 'gate: heavy:upgrade_matrix' "${work_dir}/heavy-skip/pg17-heavy-upgrade_matrix.log"
grep -qF 'upgrade_from_previous: skipped; no previous SQL versions are present' \
  "${work_dir}/heavy-skip/pg17-heavy-upgrade_matrix.log"
grep -qF 'matrix gate status: skipped' "${work_dir}/heavy-skip/pg17-heavy-upgrade_matrix.log"
grep -qF 'matrix gate exit code: 0' "${work_dir}/heavy-skip/pg17-heavy-upgrade_matrix.log"

cat >"${heavy_root}/tests/heavy/upgrade_matrix.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'upgrade_path_exercised: 0.1.0 -> 1.0.0\n'
printf 'rollback_path_exercised: failed_update_probe -> current_catalog_validated\n'
printf 'representative upgraded behavior passed\n'
SH
chmod +x "${heavy_root}/tests/heavy/upgrade_matrix.sh"
env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-pass-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-pass"
grep -qF $'pg17\theavy:upgrade_matrix\tpassed\t0' "${work_dir}/heavy-pass/summary.tsv"
grep -qF 'upgrade_path_exercised: 0.1.0 -> 1.0.0' \
  "${work_dir}/heavy-pass/pg17-heavy-upgrade_matrix.log"
grep -qF 'rollback_path_exercised: failed_update_probe -> current_catalog_validated' \
  "${work_dir}/heavy-pass/pg17-heavy-upgrade_matrix.log"
grep -qF 'upgrade_from_previous: passed' \
  "${work_dir}/heavy-pass/pg17-heavy-upgrade_matrix.log"
grep -qF 'matrix gate status: passed' \
  "${work_dir}/heavy-pass/pg17-heavy-upgrade_matrix.log"

cat >"${heavy_root}/tests/heavy/mmap_hnsw_artifact_restart.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'mmap_artifact_serving_ready: before_restart\n'
printf 'mmap_artifact_source_recheck: before_restart\n'
printf 'mmap_artifact_budget_rejected: before_restart\n'
printf 'mmap_artifact_serving_ready: after_restart\n'
printf 'mmap_artifact_source_recheck: after_restart\n'
printf 'mmap artifact restart passed without after-restart budget marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/mmap_hnsw_artifact_restart.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-missing-mmap-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-missing-mmap-evidence"; then
  echo "mmap restart pass without explicit marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:mmap_hnsw_artifact_restart\tfailed\t1' \
  "${work_dir}/heavy-missing-mmap-evidence/summary.tsv"
grep -qF 'mmap_hnsw_artifact_restart: failed; missing evidence marker: mmap_artifact_budget_rejected: after_restart' \
  "${work_dir}/heavy-missing-mmap-evidence/pg17-heavy-mmap_hnsw_artifact_restart.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-missing-mmap-evidence/pg17-heavy-mmap_hnsw_artifact_restart.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-missing-mmap-evidence/report.md"
write_mmap_hnsw_restart_fixture "${heavy_root}/tests/heavy/mmap_hnsw_artifact_restart.sh"

cat >"${heavy_root}/tests/heavy/mmap_hnsw_artifact_restart.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'mmap_artifact_serving_ready: before_restart\n'
printf 'mmap_artifact_source_recheck: before_restart\n'
printf 'mmap_artifact_budget_rejected: before_restart\n'
printf 'mmap_artifact_serving_ready: after_restart\n'
printf 'mmap_artifact_source_recheck: after_restart\n'
printf 'mmap_artifact_budget_rejected: x\n'
printf 'mmap_artifact_vacuum_recheck: after_restart\n'
printf 'mmap artifact restart passed with placeholder budget marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/mmap_hnsw_artifact_restart.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-malformed-mmap-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-malformed-mmap-evidence"; then
  echo "mmap restart pass with malformed marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:mmap_hnsw_artifact_restart\tfailed\t1' \
  "${work_dir}/heavy-malformed-mmap-evidence/summary.tsv"
grep -qF 'mmap_hnsw_artifact_restart: failed; missing evidence marker: mmap_artifact_budget_rejected: after_restart' \
  "${work_dir}/heavy-malformed-mmap-evidence/pg17-heavy-mmap_hnsw_artifact_restart.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-malformed-mmap-evidence/pg17-heavy-mmap_hnsw_artifact_restart.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-malformed-mmap-evidence/report.md"
write_mmap_hnsw_restart_fixture "${heavy_root}/tests/heavy/mmap_hnsw_artifact_restart.sh"

cat >"${heavy_root}/tests/heavy/mmap_hnsw_artifact_restart.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'mmap_artifact_serving_ready: before_restart\n'
printf 'mmap_artifact_source_recheck: before_restart\n'
printf 'mmap_artifact_budget_rejected: before_restart\n'
printf 'mmap_artifact_serving_ready: after_restart\n'
printf 'mmap_artifact_source_recheck: after_restart\n'
printf 'mmap_artifact_budget_rejected: after_restart\n'
printf 'mmap artifact restart passed without after-restart vacuum recheck marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/mmap_hnsw_artifact_restart.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-missing-mmap-vacuum-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-missing-mmap-vacuum-evidence"; then
  echo "mmap restart pass without vacuum recheck marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:mmap_hnsw_artifact_restart\tfailed\t1' \
  "${work_dir}/heavy-missing-mmap-vacuum-evidence/summary.tsv"
grep -qF 'mmap_hnsw_artifact_restart: failed; missing evidence marker: mmap_artifact_vacuum_recheck: after_restart' \
  "${work_dir}/heavy-missing-mmap-vacuum-evidence/pg17-heavy-mmap_hnsw_artifact_restart.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-missing-mmap-vacuum-evidence/pg17-heavy-mmap_hnsw_artifact_restart.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-missing-mmap-vacuum-evidence/report.md"
write_mmap_hnsw_restart_fixture "${heavy_root}/tests/heavy/mmap_hnsw_artifact_restart.sh"

cat >"${heavy_root}/tests/heavy/mmap_hnsw_artifact_restart.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'mmap_artifact_serving_ready: before_restart\n'
printf 'mmap_artifact_source_recheck: before_restart\n'
printf 'mmap_artifact_budget_rejected: before_restart\n'
printf 'mmap_artifact_serving_ready: after_restart\n'
printf 'mmap_artifact_source_recheck: after_restart\n'
printf 'mmap_artifact_budget_rejected: after_restart\n'
printf 'mmap_artifact_vacuum_recheck: x\n'
printf 'mmap artifact restart passed with placeholder vacuum recheck marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/mmap_hnsw_artifact_restart.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-malformed-mmap-vacuum-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-malformed-mmap-vacuum-evidence"; then
  echo "mmap restart pass with malformed vacuum recheck marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:mmap_hnsw_artifact_restart\tfailed\t1' \
  "${work_dir}/heavy-malformed-mmap-vacuum-evidence/summary.tsv"
grep -qF 'mmap_hnsw_artifact_restart: failed; missing evidence marker: mmap_artifact_vacuum_recheck: after_restart' \
  "${work_dir}/heavy-malformed-mmap-vacuum-evidence/pg17-heavy-mmap_hnsw_artifact_restart.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-malformed-mmap-vacuum-evidence/pg17-heavy-mmap_hnsw_artifact_restart.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-malformed-mmap-vacuum-evidence/report.md"
write_mmap_hnsw_restart_fixture "${heavy_root}/tests/heavy/mmap_hnsw_artifact_restart.sh"

cat >"${heavy_root}/tests/heavy/hnsw_vacuum.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'hnsw_vacuum_nearest_rechecked\n'
printf 'hnsw_vacuum_deleted_rows_pruned\n'
printf 'hnsw_vacuum_index_ready\n'
printf 'hnsw_vacuum_advice_present\n'
printf 'HNSW vacuum gate passed without reindex marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/hnsw_vacuum.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-missing-hnsw-vacuum-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-missing-hnsw-vacuum-evidence"; then
  echo "HNSW vacuum pass without reindex marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:hnsw_vacuum\tfailed\t1' \
  "${work_dir}/heavy-missing-hnsw-vacuum-evidence/summary.tsv"
grep -qF 'hnsw_vacuum: failed; missing evidence marker: hnsw_reindex_ready' \
  "${work_dir}/heavy-missing-hnsw-vacuum-evidence/pg17-heavy-hnsw_vacuum.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-missing-hnsw-vacuum-evidence/pg17-heavy-hnsw_vacuum.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-missing-hnsw-vacuum-evidence/report.md"
write_hnsw_vacuum_fixture "${heavy_root}/tests/heavy/hnsw_vacuum.sh"

cat >"${heavy_root}/tests/heavy/hnsw_vacuum.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'hnsw_vacuum_nearest_rechecked\n'
printf 'hnsw_vacuum_deleted_rows_pruned\n'
printf 'hnsw_vacuum_index_ready\n'
printf 'hnsw_vacuum_advice_present\n'
printf 'hnsw_reindex_ready: x\n'
printf 'HNSW vacuum gate passed with placeholder reindex marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/hnsw_vacuum.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-malformed-hnsw-vacuum-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-malformed-hnsw-vacuum-evidence"; then
  echo "HNSW vacuum pass with malformed reindex marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:hnsw_vacuum\tfailed\t1' \
  "${work_dir}/heavy-malformed-hnsw-vacuum-evidence/summary.tsv"
grep -qF 'hnsw_vacuum: failed; missing evidence marker: hnsw_reindex_ready' \
  "${work_dir}/heavy-malformed-hnsw-vacuum-evidence/pg17-heavy-hnsw_vacuum.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-malformed-hnsw-vacuum-evidence/pg17-heavy-hnsw_vacuum.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-malformed-hnsw-vacuum-evidence/report.md"
write_hnsw_vacuum_fixture "${heavy_root}/tests/heavy/hnsw_vacuum.sh"

cat >"${heavy_root}/tests/heavy/filtered_ann_recall.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'filtered_ann_exact_oracle_seqscan\n'
printf 'filtered_ann_candidate_indexscan\n'
printf 'filtered_ann_exact_candidates_match\n'
printf 'filtered_ann_recall_passing\n'
printf 'filtered_ann_tenant_recheck_passing\n'
printf 'filtered ANN recall gate passed without no-match marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/filtered_ann_recall.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-missing-filtered-ann-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-missing-filtered-ann-evidence"; then
  echo "filtered ANN recall pass without no-match marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:filtered_ann_recall\tfailed\t1' \
  "${work_dir}/heavy-missing-filtered-ann-evidence/summary.tsv"
grep -qF 'filtered_ann_recall: failed; missing evidence marker: filtered_ann_no_match_empty' \
  "${work_dir}/heavy-missing-filtered-ann-evidence/pg17-heavy-filtered_ann_recall.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-missing-filtered-ann-evidence/pg17-heavy-filtered_ann_recall.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-missing-filtered-ann-evidence/report.md"
write_filtered_ann_recall_fixture "${heavy_root}/tests/heavy/filtered_ann_recall.sh"

cat >"${heavy_root}/tests/heavy/filtered_ann_recall.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'filtered_ann_exact_oracle_seqscan\n'
printf 'filtered_ann_candidate_indexscan\n'
printf 'filtered_ann_exact_candidates_match\n'
printf 'filtered_ann_recall_passing\n'
printf 'filtered_ann_tenant_recheck_passing\n'
printf 'filtered_ann_no_match_empty: x\n'
printf 'filtered ANN recall gate passed with placeholder no-match marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/filtered_ann_recall.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-malformed-filtered-ann-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-malformed-filtered-ann-evidence"; then
  echo "filtered ANN recall pass with malformed no-match marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:filtered_ann_recall\tfailed\t1' \
  "${work_dir}/heavy-malformed-filtered-ann-evidence/summary.tsv"
grep -qF 'filtered_ann_recall: failed; missing evidence marker: filtered_ann_no_match_empty' \
  "${work_dir}/heavy-malformed-filtered-ann-evidence/pg17-heavy-filtered_ann_recall.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-malformed-filtered-ann-evidence/pg17-heavy-filtered_ann_recall.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-malformed-filtered-ann-evidence/report.md"
write_filtered_ann_recall_fixture "${heavy_root}/tests/heavy/filtered_ann_recall.sh"

cat >"${heavy_root}/tests/heavy/late_interaction_ann_serving.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'late_interaction_ann_candidates_deduped\n'
printf 'late_interaction_ann_exact_rerank_scores\n'
printf 'late_interaction_ann_source_recheck\n'
printf 'late_interaction_ann_deleted_recheck\n'
printf 'late-interaction ANN serving gate passed without budget marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/late_interaction_ann_serving.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-missing-late-interaction-ann-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-missing-late-interaction-ann-evidence"; then
  echo "late-interaction ANN serving pass without budget marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:late_interaction_ann_serving\tfailed\t1' \
  "${work_dir}/heavy-missing-late-interaction-ann-evidence/summary.tsv"
grep -qF 'late_interaction_ann_serving: failed; missing evidence marker: late_interaction_ann_budget_rejected' \
  "${work_dir}/heavy-missing-late-interaction-ann-evidence/pg17-heavy-late_interaction_ann_serving.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-missing-late-interaction-ann-evidence/pg17-heavy-late_interaction_ann_serving.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-missing-late-interaction-ann-evidence/report.md"
write_late_interaction_ann_serving_fixture "${heavy_root}/tests/heavy/late_interaction_ann_serving.sh"

cat >"${heavy_root}/tests/heavy/late_interaction_ann_serving.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'late_interaction_ann_candidates_deduped\n'
printf 'late_interaction_ann_exact_rerank_scores\n'
printf 'late_interaction_ann_source_recheck\n'
printf 'late_interaction_ann_deleted_recheck\n'
printf 'late_interaction_ann_budget_rejected: x\n'
printf 'late-interaction ANN serving gate passed with placeholder budget marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/late_interaction_ann_serving.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-malformed-late-interaction-ann-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-malformed-late-interaction-ann-evidence"; then
  echo "late-interaction ANN serving pass with malformed budget marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:late_interaction_ann_serving\tfailed\t1' \
  "${work_dir}/heavy-malformed-late-interaction-ann-evidence/summary.tsv"
grep -qF 'late_interaction_ann_serving: failed; missing evidence marker: late_interaction_ann_budget_rejected' \
  "${work_dir}/heavy-malformed-late-interaction-ann-evidence/pg17-heavy-late_interaction_ann_serving.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-malformed-late-interaction-ann-evidence/pg17-heavy-late_interaction_ann_serving.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-malformed-late-interaction-ann-evidence/report.md"
write_late_interaction_ann_serving_fixture "${heavy_root}/tests/heavy/late_interaction_ann_serving.sh"

cat >"${heavy_root}/tests/heavy/crash_restart_hnsw.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'hnsw_restart_nearest_rechecked: before_restart\n'
printf 'hnsw_restart_index_scan: before_restart\n'
printf 'hnsw_mapped_attach: before_restart\n'
printf 'hnsw_restart_nearest_rechecked: after_restart\n'
printf 'HNSW restart gate passed without after-restart index marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/crash_restart_hnsw.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-missing-hnsw-restart-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-missing-hnsw-restart-evidence"; then
  echo "HNSW restart pass without after-restart index marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:crash_restart_hnsw\tfailed\t1' \
  "${work_dir}/heavy-missing-hnsw-restart-evidence/summary.tsv"
grep -qF 'crash_restart_hnsw: failed; missing evidence marker: hnsw_restart_index_scan: after_restart' \
  "${work_dir}/heavy-missing-hnsw-restart-evidence/pg17-heavy-crash_restart_hnsw.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-missing-hnsw-restart-evidence/pg17-heavy-crash_restart_hnsw.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-missing-hnsw-restart-evidence/report.md"
write_crash_restart_hnsw_fixture "${heavy_root}/tests/heavy/crash_restart_hnsw.sh"

cat >"${heavy_root}/tests/heavy/crash_restart_hnsw.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'hnsw_restart_nearest_rechecked: before_restart\n'
printf 'hnsw_restart_index_scan: before_restart\n'
printf 'hnsw_mapped_attach: before_restart\n'
printf 'hnsw_restart_nearest_rechecked: after_restart\n'
printf 'hnsw_restart_index_scan: x\n'
printf 'HNSW restart gate passed with placeholder after-restart index marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/crash_restart_hnsw.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-malformed-hnsw-restart-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-malformed-hnsw-restart-evidence"; then
  echo "HNSW restart pass with malformed after-restart index marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:crash_restart_hnsw\tfailed\t1' \
  "${work_dir}/heavy-malformed-hnsw-restart-evidence/summary.tsv"
grep -qF 'crash_restart_hnsw: failed; missing evidence marker: hnsw_restart_index_scan: after_restart' \
  "${work_dir}/heavy-malformed-hnsw-restart-evidence/pg17-heavy-crash_restart_hnsw.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-malformed-hnsw-restart-evidence/pg17-heavy-crash_restart_hnsw.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-malformed-hnsw-restart-evidence/report.md"
write_crash_restart_hnsw_fixture "${heavy_root}/tests/heavy/crash_restart_hnsw.sh"

cat >"${heavy_root}/tests/heavy/concurrent_read_write.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'concurrent_hnsw_writer_completed\n'
printf 'concurrent_hnsw_reader_completed\n'
printf 'concurrent_hnsw_row_count_verified\n'
printf 'concurrent read/write gate passed without inserted-row marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/concurrent_read_write.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-missing-concurrent-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-missing-concurrent-evidence"; then
  echo "concurrent read/write pass without inserted-row marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:concurrent_read_write\tfailed\t1' \
  "${work_dir}/heavy-missing-concurrent-evidence/summary.tsv"
grep -qF 'concurrent_read_write: failed; missing evidence marker: concurrent_hnsw_insert_visible' \
  "${work_dir}/heavy-missing-concurrent-evidence/pg17-heavy-concurrent_read_write.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-missing-concurrent-evidence/pg17-heavy-concurrent_read_write.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-missing-concurrent-evidence/report.md"
write_concurrent_read_write_fixture "${heavy_root}/tests/heavy/concurrent_read_write.sh"

cat >"${heavy_root}/tests/heavy/concurrent_read_write.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'concurrent_hnsw_writer_completed\n'
printf 'concurrent_hnsw_reader_completed\n'
printf 'concurrent_hnsw_row_count_verified\n'
printf 'concurrent_hnsw_insert_visible: x\n'
printf 'concurrent read/write gate passed with placeholder inserted-row marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/concurrent_read_write.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-malformed-concurrent-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-malformed-concurrent-evidence"; then
  echo "concurrent read/write pass with malformed inserted-row marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:concurrent_read_write\tfailed\t1' \
  "${work_dir}/heavy-malformed-concurrent-evidence/summary.tsv"
grep -qF 'concurrent_read_write: failed; missing evidence marker: concurrent_hnsw_insert_visible' \
  "${work_dir}/heavy-malformed-concurrent-evidence/pg17-heavy-concurrent_read_write.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-malformed-concurrent-evidence/pg17-heavy-concurrent_read_write.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-malformed-concurrent-evidence/report.md"
write_concurrent_read_write_fixture "${heavy_root}/tests/heavy/concurrent_read_write.sh"

cat >"${heavy_root}/tests/heavy/large_exact_search.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'NOTICE:  large_exact_rows_loaded\n'
printf 'NOTICE:  large_exact_oracle_match\n'
printf 'NOTICE:  large_exact_missing_filter_empty\n'
printf 'NOTICE:  large_exact_dimension_mismatch_rejected\n'
printf 'large exact search gate passed without unknown-filter marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/large_exact_search.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-missing-large-exact-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-missing-large-exact-evidence"; then
  echo "large exact search pass without unknown-filter marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:large_exact_search\tfailed\t1' \
  "${work_dir}/heavy-missing-large-exact-evidence/summary.tsv"
grep -qF 'large_exact_search: failed; missing evidence marker: NOTICE:  large_exact_unknown_filter_rejected' \
  "${work_dir}/heavy-missing-large-exact-evidence/pg17-heavy-large_exact_search.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-missing-large-exact-evidence/pg17-heavy-large_exact_search.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-missing-large-exact-evidence/report.md"
write_large_exact_search_fixture "${heavy_root}/tests/heavy/large_exact_search.sh"

cat >"${heavy_root}/tests/heavy/large_exact_search.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'NOTICE:  large_exact_rows_loaded\n'
printf 'NOTICE:  large_exact_oracle_match\n'
printf 'NOTICE:  large_exact_missing_filter_empty\n'
printf 'NOTICE:  large_exact_dimension_mismatch_rejected\n'
printf 'NOTICE:  large_exact_unknown_filter_rejected: x\n'
printf 'large exact search gate passed with placeholder unknown-filter marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/large_exact_search.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-malformed-large-exact-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-malformed-large-exact-evidence"; then
  echo "large exact search pass with malformed unknown-filter marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:large_exact_search\tfailed\t1' \
  "${work_dir}/heavy-malformed-large-exact-evidence/summary.tsv"
grep -qF 'large_exact_search: failed; missing evidence marker: NOTICE:  large_exact_unknown_filter_rejected' \
  "${work_dir}/heavy-malformed-large-exact-evidence/pg17-heavy-large_exact_search.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-malformed-large-exact-evidence/pg17-heavy-large_exact_search.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-malformed-large-exact-evidence/report.md"
write_large_exact_search_fixture "${heavy_root}/tests/heavy/large_exact_search.sh"

cat >"${heavy_root}/tests/heavy/partitioned_collections.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'NOTICE:  partitioned_exact_order_verified\n'
printf 'NOTICE:  partitioned_tenant_filter_verified\n'
printf 'NOTICE:  partitioned_facet_verified\n'
printf 'NOTICE:  partitioned_delete_visibility_verified\n'
printf 'NOTICE:  partitioned_drop_recheck_verified\n'
printf 'NOTICE:  partitioned_unknown_filter_rejected\n'
printf 'partitioned collections gate passed without count marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/partitioned_collections.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-missing-partitioned-count-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-missing-partitioned-count-evidence"; then
  echo "partitioned collections pass without count marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:partitioned_collections\tfailed\t1' \
  "${work_dir}/heavy-missing-partitioned-count-evidence/summary.tsv"
grep -qF 'partitioned_collections: failed; missing evidence marker: NOTICE:  partitioned_count_verified' \
  "${work_dir}/heavy-missing-partitioned-count-evidence/pg17-heavy-partitioned_collections.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-missing-partitioned-count-evidence/pg17-heavy-partitioned_collections.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-missing-partitioned-count-evidence/report.md"
write_partitioned_collections_fixture "${heavy_root}/tests/heavy/partitioned_collections.sh"

cat >"${heavy_root}/tests/heavy/partitioned_collections.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'NOTICE:  partitioned_exact_order_verified\n'
printf 'NOTICE:  partitioned_tenant_filter_verified\n'
printf 'NOTICE:  partitioned_count_verified: x\n'
printf 'NOTICE:  partitioned_facet_verified\n'
printf 'NOTICE:  partitioned_delete_visibility_verified\n'
printf 'NOTICE:  partitioned_drop_recheck_verified\n'
printf 'NOTICE:  partitioned_unknown_filter_rejected\n'
printf 'partitioned collections gate passed with placeholder count marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/partitioned_collections.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-malformed-partitioned-count-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-malformed-partitioned-count-evidence"; then
  echo "partitioned collections pass with malformed count marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:partitioned_collections\tfailed\t1' \
  "${work_dir}/heavy-malformed-partitioned-count-evidence/summary.tsv"
grep -qF 'partitioned_collections: failed; missing evidence marker: NOTICE:  partitioned_count_verified' \
  "${work_dir}/heavy-malformed-partitioned-count-evidence/pg17-heavy-partitioned_collections.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-malformed-partitioned-count-evidence/pg17-heavy-partitioned_collections.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-malformed-partitioned-count-evidence/report.md"
write_partitioned_collections_fixture "${heavy_root}/tests/heavy/partitioned_collections.sh"

cat >"${heavy_root}/tests/heavy/partitioned_collections.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'NOTICE:  partitioned_exact_order_verified\n'
printf 'NOTICE:  partitioned_tenant_filter_verified\n'
printf 'NOTICE:  partitioned_count_verified\n'
printf 'NOTICE:  partitioned_facet_verified\n'
printf 'NOTICE:  partitioned_delete_visibility_verified\n'
printf 'NOTICE:  partitioned_drop_recheck_verified\n'
printf 'partitioned collections gate passed without unknown-filter marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/partitioned_collections.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-missing-partitioned-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-missing-partitioned-evidence"; then
  echo "partitioned collections pass without unknown-filter marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:partitioned_collections\tfailed\t1' \
  "${work_dir}/heavy-missing-partitioned-evidence/summary.tsv"
grep -qF 'partitioned_collections: failed; missing evidence marker: NOTICE:  partitioned_unknown_filter_rejected' \
  "${work_dir}/heavy-missing-partitioned-evidence/pg17-heavy-partitioned_collections.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-missing-partitioned-evidence/pg17-heavy-partitioned_collections.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-missing-partitioned-evidence/report.md"
write_partitioned_collections_fixture "${heavy_root}/tests/heavy/partitioned_collections.sh"

cat >"${heavy_root}/tests/heavy/partitioned_collections.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'NOTICE:  partitioned_exact_order_verified\n'
printf 'NOTICE:  partitioned_tenant_filter_verified\n'
printf 'NOTICE:  partitioned_count_verified\n'
printf 'NOTICE:  partitioned_facet_verified\n'
printf 'NOTICE:  partitioned_delete_visibility_verified\n'
printf 'NOTICE:  partitioned_drop_recheck_verified\n'
printf 'NOTICE:  partitioned_unknown_filter_rejected: x\n'
printf 'partitioned collections gate passed with placeholder unknown-filter marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/partitioned_collections.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-malformed-partitioned-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-malformed-partitioned-evidence"; then
  echo "partitioned collections pass with malformed unknown-filter marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:partitioned_collections\tfailed\t1' \
  "${work_dir}/heavy-malformed-partitioned-evidence/summary.tsv"
grep -qF 'partitioned_collections: failed; missing evidence marker: NOTICE:  partitioned_unknown_filter_rejected' \
  "${work_dir}/heavy-malformed-partitioned-evidence/pg17-heavy-partitioned_collections.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-malformed-partitioned-evidence/pg17-heavy-partitioned_collections.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-malformed-partitioned-evidence/report.md"
write_partitioned_collections_fixture "${heavy_root}/tests/heavy/partitioned_collections.sh"

cat >"${heavy_root}/tests/heavy/low_memory_build.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'NOTICE:  low_memory_rejected_bad_build\n'
printf 'NOTICE:  low_memory_index_order_verified\n'
printf 'NOTICE:  low_memory_reltuples_verified\n'
printf 'low-memory build gate passed without failed-build cleanup marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/low_memory_build.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-missing-low-memory-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-missing-low-memory-evidence"; then
  echo "low-memory build pass without cleanup marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:low_memory_build\tfailed\t1' \
  "${work_dir}/heavy-missing-low-memory-evidence/summary.tsv"
grep -qF 'low_memory_build: failed; missing evidence marker: NOTICE:  low_memory_failed_build_cleaned' \
  "${work_dir}/heavy-missing-low-memory-evidence/pg17-heavy-low_memory_build.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-missing-low-memory-evidence/pg17-heavy-low_memory_build.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-missing-low-memory-evidence/report.md"
write_low_memory_build_fixture "${heavy_root}/tests/heavy/low_memory_build.sh"

cat >"${heavy_root}/tests/heavy/low_memory_build.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'NOTICE:  low_memory_rejected_bad_build\n'
printf 'NOTICE:  low_memory_failed_build_cleaned\n'
printf 'NOTICE:  low_memory_index_order_verified\n'
printf 'NOTICE:  low_memory_reltuples_verified: x\n'
printf 'low-memory build gate passed with placeholder reltuples marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/low_memory_build.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-malformed-low-memory-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-malformed-low-memory-evidence"; then
  echo "low-memory build pass with malformed reltuples marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:low_memory_build\tfailed\t1' \
  "${work_dir}/heavy-malformed-low-memory-evidence/summary.tsv"
grep -qF 'low_memory_build: failed; missing evidence marker: NOTICE:  low_memory_reltuples_verified' \
  "${work_dir}/heavy-malformed-low-memory-evidence/pg17-heavy-low_memory_build.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-malformed-low-memory-evidence/pg17-heavy-low_memory_build.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-malformed-low-memory-evidence/report.md"
write_low_memory_build_fixture "${heavy_root}/tests/heavy/low_memory_build.sh"

cat >"${heavy_root}/tests/heavy/backup_restore.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'backup_restore_dump_created\n'
printf 'backup_restore_restore_completed\n'
printf 'NOTICE:  backup_restore_nearest_verified\n'
printf 'NOTICE:  backup_restore_filter_verified\n'
printf 'NOTICE:  backup_restore_jsonb_facet_verified\n'
printf 'NOTICE:  backup_restore_scroll_verified\n'
printf 'NOTICE:  backup_restore_migration_verified\n'
printf 'NOTICE:  backup_restore_telemetry_verified\n'
printf 'NOTICE:  backup_restore_query_stats_verified\n'
printf 'NOTICE:  backup_restore_hnsw_ready\n'
printf 'backup/restore gate passed without model-version marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/backup_restore.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-missing-backup-restore-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-missing-backup-restore-evidence"; then
  echo "backup/restore pass without model-version marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:backup_restore\tfailed\t1' \
  "${work_dir}/heavy-missing-backup-restore-evidence/summary.tsv"
grep -qF 'backup_restore: failed; missing evidence marker: NOTICE:  backup_restore_model_versions_verified' \
  "${work_dir}/heavy-missing-backup-restore-evidence/pg17-heavy-backup_restore.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-missing-backup-restore-evidence/pg17-heavy-backup_restore.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-missing-backup-restore-evidence/report.md"
write_backup_restore_fixture "${heavy_root}/tests/heavy/backup_restore.sh"

cat >"${heavy_root}/tests/heavy/backup_restore.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'backup_restore_dump_created\n'
printf 'backup_restore_restore_completed\n'
printf 'NOTICE:  backup_restore_nearest_verified\n'
printf 'NOTICE:  backup_restore_filter_verified\n'
printf 'NOTICE:  backup_restore_jsonb_facet_verified\n'
printf 'NOTICE:  backup_restore_scroll_verified\n'
printf 'NOTICE:  backup_restore_model_versions_verified\n'
printf 'NOTICE:  backup_restore_migration_verified\n'
printf 'NOTICE:  backup_restore_telemetry_verified\n'
printf 'NOTICE:  backup_restore_query_stats_verified\n'
printf 'NOTICE:  backup_restore_hnsw_ready: x\n'
printf 'backup/restore gate passed with placeholder HNSW marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/backup_restore.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-malformed-backup-restore-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-malformed-backup-restore-evidence"; then
  echo "backup/restore pass with malformed HNSW marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:backup_restore\tfailed\t1' \
  "${work_dir}/heavy-malformed-backup-restore-evidence/summary.tsv"
grep -qF 'backup_restore: failed; missing evidence marker: NOTICE:  backup_restore_hnsw_ready' \
  "${work_dir}/heavy-malformed-backup-restore-evidence/pg17-heavy-backup_restore.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-malformed-backup-restore-evidence/pg17-heavy-backup_restore.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-malformed-backup-restore-evidence/report.md"
write_backup_restore_fixture "${heavy_root}/tests/heavy/backup_restore.sh"

cat >"${heavy_root}/tests/heavy/physical_backup_wal_replay.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'physical_backup_basebackup_created\n'
printf 'NOTICE:  physical_backup_exact_nearest_verified: before_replay\n'
printf 'NOTICE:  physical_backup_indexed_nearest_verified: before_replay\n'
printf 'NOTICE:  physical_backup_exact_scroll_verified: before_replay\n'
printf 'NOTICE:  physical_backup_indexed_scroll_verified: before_replay\n'
printf 'NOTICE:  physical_backup_hnsw_ready: before_replay\n'
printf 'NOTICE:  physical_backup_exact_nearest_verified: after_replay\n'
printf 'NOTICE:  physical_backup_indexed_nearest_verified: after_replay\n'
printf 'NOTICE:  physical_backup_exact_scroll_verified: after_replay\n'
printf 'NOTICE:  physical_backup_indexed_scroll_verified: after_replay\n'
printf 'NOTICE:  physical_backup_hnsw_ready: after_replay\n'
printf 'physical backup/WAL replay gate passed without restart marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/physical_backup_wal_replay.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-missing-physical-backup-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-missing-physical-backup-evidence"; then
  echo "physical backup/WAL replay pass without restart marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:physical_backup_wal_replay\tfailed\t1' \
  "${work_dir}/heavy-missing-physical-backup-evidence/summary.tsv"
grep -qF 'physical_backup_wal_replay: failed; missing ordered evidence marker: physical_backup_restarted_after_writes' \
  "${work_dir}/heavy-missing-physical-backup-evidence/pg17-heavy-physical_backup_wal_replay.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-missing-physical-backup-evidence/pg17-heavy-physical_backup_wal_replay.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-missing-physical-backup-evidence/report.md"
write_physical_backup_wal_replay_fixture "${heavy_root}/tests/heavy/physical_backup_wal_replay.sh"

cat >"${heavy_root}/tests/heavy/physical_backup_wal_replay.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'physical_backup_basebackup_created\n'
printf 'NOTICE:  physical_backup_exact_nearest_verified: before_replay\n'
printf 'NOTICE:  physical_backup_indexed_nearest_verified: before_replay\n'
printf 'NOTICE:  physical_backup_exact_scroll_verified: before_replay\n'
printf 'NOTICE:  physical_backup_indexed_scroll_verified: before_replay\n'
printf 'NOTICE:  physical_backup_hnsw_ready: before_replay\n'
printf 'physical_backup_restarted_after_writes\n'
printf 'NOTICE:  physical_backup_exact_nearest_verified: after_replay\n'
printf 'NOTICE:  physical_backup_indexed_nearest_verified: after_replay\n'
printf 'NOTICE:  physical_backup_exact_scroll_verified: after_replay\n'
printf 'NOTICE:  physical_backup_indexed_scroll_verified: after_replay\n'
printf 'NOTICE:  physical_backup_hnsw_ready: after_replay: x\n'
printf 'physical backup/WAL replay gate passed with placeholder HNSW marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/physical_backup_wal_replay.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-malformed-physical-backup-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-malformed-physical-backup-evidence"; then
  echo "physical backup/WAL replay pass with malformed HNSW marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:physical_backup_wal_replay\tfailed\t1' \
  "${work_dir}/heavy-malformed-physical-backup-evidence/summary.tsv"
grep -qF 'physical_backup_wal_replay: failed; missing ordered evidence marker: NOTICE:  physical_backup_hnsw_ready: after_replay' \
  "${work_dir}/heavy-malformed-physical-backup-evidence/pg17-heavy-physical_backup_wal_replay.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-malformed-physical-backup-evidence/pg17-heavy-physical_backup_wal_replay.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-malformed-physical-backup-evidence/report.md"
write_physical_backup_wal_replay_fixture "${heavy_root}/tests/heavy/physical_backup_wal_replay.sh"

cat >"${heavy_root}/tests/heavy/physical_backup_wal_replay.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'physical_backup_basebackup_created\n'
printf 'NOTICE:  physical_backup_exact_nearest_verified: before_replay\n'
printf 'NOTICE:  physical_backup_indexed_nearest_verified: before_replay\n'
printf 'NOTICE:  physical_backup_exact_scroll_verified: before_replay\n'
printf 'NOTICE:  physical_backup_indexed_scroll_verified: before_replay\n'
printf 'NOTICE:  physical_backup_hnsw_ready: before_replay\n'
printf 'NOTICE:  physical_backup_exact_nearest_verified: after_replay\n'
printf 'physical_backup_restarted_after_writes\n'
printf 'NOTICE:  physical_backup_indexed_nearest_verified: after_replay\n'
printf 'NOTICE:  physical_backup_exact_scroll_verified: after_replay\n'
printf 'NOTICE:  physical_backup_indexed_scroll_verified: after_replay\n'
printf 'NOTICE:  physical_backup_hnsw_ready: after_replay\n'
printf 'physical backup/WAL replay gate passed with out-of-order evidence\n'
SH
chmod +x "${heavy_root}/tests/heavy/physical_backup_wal_replay.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-out-of-order-physical-backup-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-out-of-order-physical-backup-evidence"; then
  echo "physical backup/WAL replay pass with out-of-order marker evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:physical_backup_wal_replay\tfailed\t1' \
  "${work_dir}/heavy-out-of-order-physical-backup-evidence/summary.tsv"
grep -qF 'physical_backup_wal_replay: failed; missing ordered evidence marker: NOTICE:  physical_backup_exact_nearest_verified: after_replay' \
  "${work_dir}/heavy-out-of-order-physical-backup-evidence/pg17-heavy-physical_backup_wal_replay.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-out-of-order-physical-backup-evidence/pg17-heavy-physical_backup_wal_replay.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-out-of-order-physical-backup-evidence/report.md"
write_physical_backup_wal_replay_fixture "${heavy_root}/tests/heavy/physical_backup_wal_replay.sh"

cat >"${heavy_root}/tests/heavy/upgrade_matrix.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'representative upgraded behavior passed without evidence marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/upgrade_matrix.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-missing-upgrade-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-missing-upgrade-evidence"; then
  echo "upgrade matrix pass without explicit upgrade evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:upgrade_matrix\tfailed\t1' \
  "${work_dir}/heavy-missing-upgrade-evidence/summary.tsv"
grep -qF 'upgrade_from_previous: failed; no upgrade_path_exercised marker found' \
  "${work_dir}/heavy-missing-upgrade-evidence/pg17-heavy-upgrade_matrix.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-missing-upgrade-evidence/pg17-heavy-upgrade_matrix.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-missing-upgrade-evidence/report.md"

cat >"${heavy_root}/tests/heavy/upgrade_matrix.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'upgrade_path_exercised: 0.1.0 -> 1.0.0\n'
printf 'representative upgraded behavior passed without rollback marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/upgrade_matrix.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-missing-rollback-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-missing-rollback-evidence"; then
  echo "upgrade matrix pass without explicit rollback evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:upgrade_matrix\tfailed\t1' \
  "${work_dir}/heavy-missing-rollback-evidence/summary.tsv"
grep -qF 'upgrade_from_previous: failed; no rollback_path_exercised marker found' \
  "${work_dir}/heavy-missing-rollback-evidence/pg17-heavy-upgrade_matrix.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-missing-rollback-evidence/pg17-heavy-upgrade_matrix.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-missing-rollback-evidence/report.md"

cat >"${heavy_root}/tests/heavy/upgrade_matrix.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'upgrade_path_exercised: 0.1.0 -> 1.0.0\n'
printf 'rollback_path_exercised: x\n'
printf 'representative upgraded behavior passed with placeholder rollback marker\n'
SH
chmod +x "${heavy_root}/tests/heavy/upgrade_matrix.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-malformed-rollback-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-malformed-rollback-evidence"; then
  echo "upgrade matrix pass with malformed rollback evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:upgrade_matrix\tfailed\t1' \
  "${work_dir}/heavy-malformed-rollback-evidence/summary.tsv"
grep -qF 'upgrade_from_previous: failed; no rollback_path_exercised marker found' \
  "${work_dir}/heavy-malformed-rollback-evidence/pg17-heavy-upgrade_matrix.log"
grep -qF 'matrix gate status: failed' \
  "${work_dir}/heavy-malformed-rollback-evidence/pg17-heavy-upgrade_matrix.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-malformed-rollback-evidence/report.md"

cat >"${heavy_root}/tests/heavy/upgrade_matrix.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'upgrade_path_exercised:   ->  \n'
printf 'representative upgraded behavior passed with malformed evidence\n'
SH
chmod +x "${heavy_root}/tests/heavy/upgrade_matrix.sh"
if env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/heavy-malformed-upgrade-evidence-cargo.log" \
  PG17_CONFIG="${work_dir}/pg17-matrix/bin/pg_config" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --major 17 \
    --mode heavy \
    --out-dir "${work_dir}/heavy-malformed-upgrade-evidence"; then
  echo "upgrade matrix pass with malformed upgrade evidence should fail" >&2
  exit 1
fi
grep -qF $'pg17\theavy:upgrade_matrix\tfailed\t1' \
  "${work_dir}/heavy-malformed-upgrade-evidence/summary.tsv"
grep -qF 'upgrade_path_exercised:   ->  ' \
  "${work_dir}/heavy-malformed-upgrade-evidence/pg17-heavy-upgrade_matrix.log"
grep -qF 'upgrade_from_previous: failed; no upgrade_path_exercised marker found' \
  "${work_dir}/heavy-malformed-upgrade-evidence/pg17-heavy-upgrade_matrix.log"
grep -qF -- '- Approval: `incomplete`' \
  "${work_dir}/heavy-malformed-upgrade-evidence/report.md"

clean_matrix_root="${work_dir}/clean-matrix-root"
stage_passing_heavy_fixtures "${clean_matrix_root}"
write_crash_restart_hnsw_fixture "${clean_matrix_root}/tests/heavy/crash_restart_hnsw.sh"
write_mapped_hnsw_lifecycle_fixture "${clean_matrix_root}/tests/heavy/mapped_hnsw_lifecycle_cleanup.sh"
write_mmap_hnsw_restart_fixture "${clean_matrix_root}/tests/heavy/mmap_hnsw_artifact_restart.sh"
write_hnsw_vacuum_fixture "${clean_matrix_root}/tests/heavy/hnsw_vacuum.sh"
write_backup_restore_fixture "${clean_matrix_root}/tests/heavy/backup_restore.sh"
write_physical_backup_wal_replay_fixture "${clean_matrix_root}/tests/heavy/physical_backup_wal_replay.sh"
write_concurrent_read_write_fixture "${clean_matrix_root}/tests/heavy/concurrent_read_write.sh"
write_filtered_ann_recall_fixture "${clean_matrix_root}/tests/heavy/filtered_ann_recall.sh"
write_late_interaction_ann_serving_fixture "${clean_matrix_root}/tests/heavy/late_interaction_ann_serving.sh"
write_build_job_resumability_fixture "${clean_matrix_root}/tests/heavy/build_job_resumability.sh"
write_large_exact_search_fixture "${clean_matrix_root}/tests/heavy/large_exact_search.sh"
write_partitioned_collections_fixture "${clean_matrix_root}/tests/heavy/partitioned_collections.sh"
write_low_memory_build_fixture "${clean_matrix_root}/tests/heavy/low_memory_build.sh"
cat >"${clean_matrix_root}/tests/heavy/upgrade_matrix.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'upgrade_path_exercised: 0.1.0 -> 1.0.0\n'
printf 'rollback_path_exercised: failed_update_probe -> current_catalog_validated\n'
printf 'fake heavy gate passed: %s\n' "$(basename "$0")"
SH
chmod +x "${clean_matrix_root}/tests/heavy/upgrade_matrix.sh"
git -C "${clean_matrix_root}" init -q
git -C "${clean_matrix_root}" add .
git -C "${clean_matrix_root}" \
  -c user.name='Postgres Matrix Test' \
  -c user.email='postgres-matrix-test@example.invalid' \
  commit -q -m 'initial clean matrix fixture'

env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/clean-matrix-cargo.log" \
  "${matrix_pg_configs[@]}" \
  REPO_ROOT="${clean_matrix_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --out-dir "${work_dir}/clean-matrix"
grep -qF -- '- Worktree: `clean`' "${work_dir}/clean-matrix/report.md"
grep -qF -- '- Full release scope: `1`' "${work_dir}/clean-matrix/report.md"
grep -qF -- '- Passed: `108`' "${work_dir}/clean-matrix/report.md"
grep -qF -- '- Skipped: `0`' "${work_dir}/clean-matrix/report.md"
grep -qF -- '- Failed: `0`' "${work_dir}/clean-matrix/report.md"
grep -qF -- '- Missing: `0`' "${work_dir}/clean-matrix/report.md"
grep -qF -- '- Approval: `complete`' "${work_dir}/clean-matrix/report.md"

printf 'dirty\n' >"${clean_matrix_root}/dirty.txt"
env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/dirty-matrix-cargo.log" \
  "${matrix_pg_configs[@]}" \
  REPO_ROOT="${clean_matrix_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --out-dir "${work_dir}/dirty-matrix"
grep -qF -- '- Worktree: `dirty`' "${work_dir}/dirty-matrix/report.md"
grep -qF -- '- Full release scope: `1`' "${work_dir}/dirty-matrix/report.md"
grep -qF -- '- Passed: `108`' "${work_dir}/dirty-matrix/report.md"
grep -qF -- '- Skipped: `0`' "${work_dir}/dirty-matrix/report.md"
grep -qF -- '- Failed: `0`' "${work_dir}/dirty-matrix/report.md"
grep -qF -- '- Missing: `0`' "${work_dir}/dirty-matrix/report.md"
grep -qF -- '- Approval: `incomplete`' "${work_dir}/dirty-matrix/report.md"
grep -qF 'a clean worktree' "${work_dir}/dirty-matrix/report.md"

cat >"${heavy_root}/tests/heavy/upgrade_matrix.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'upgrade_path_exercised: 0.1.0 -> 1.0.0\n'
printf 'rollback_path_exercised: failed_update_probe -> current_catalog_validated\n'
printf 'representative upgraded behavior passed\n'
SH
chmod +x "${heavy_root}/tests/heavy/upgrade_matrix.sh"
env \
  PATH="${fake_bin}:${PATH}" \
  FAKE_CARGO_LOG="${work_dir}/repo-relative-cargo.log" \
  "${matrix_pg_configs[@]}" \
  REPO_ROOT="${heavy_root}" \
  "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" \
    --mode heavy \
    --out-dir "${heavy_root}/target/release-evidence/upgrade"
repo_relative_summary="${heavy_root}/target/release-evidence/upgrade/summary.tsv"
repo_relative_report="${heavy_root}/target/release-evidence/upgrade/report.md"
grep -q 'Summary TSV: `target/release-evidence/upgrade/summary.tsv`' \
  "${repo_relative_report}"
if grep -qF -- "${heavy_root}" "${repo_relative_summary}" "${repo_relative_report}"; then
  echo "repo-local postgres matrix evidence paths should be repo-relative" >&2
  exit 1
fi
awk -F '\t' '
  NR > 1 && ($8 ~ /^\// || $10 !~ /^[1-9][0-9]*$/) { bad = 1 }
  END { exit(bad ? 1 : 0) }
' "${repo_relative_summary}"

assert_fails() {
  local label="$1"
  local expected="$2"
  shift 2
  if "${REPO_ROOT}/scripts/run-postgres-matrix-gates.sh" "$@" 2>"${work_dir}/${label}.err"; then
    echo "${label} should fail" >&2
    exit 1
  fi
  grep -q -- "${expected}" "${work_dir}/${label}.err"
}

assert_fails unsupported-major 'unsupported PostgreSQL major: 14' \
  --dry-run --major 14 --out-dir "${work_dir}/bad-major"
assert_fails duplicate-major 'duplicate PostgreSQL major: 17' \
  --dry-run --major 17 --major 17 --out-dir "${work_dir}/duplicate-major"
assert_fails bad-mode '--mode must be fast, schema, pgrx, heavy, or all' \
  --dry-run --major 17 --mode invalid --out-dir "${work_dir}/bad-mode"
assert_fails root-out-dir '--out-dir must be a non-root path' \
  --dry-run --major 17 --out-dir /
