#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
PYTHON_BIN="${PYTHON_BIN:-python3}"
GIT_BIN="${GIT_BIN:-git}"
WAL_REPLAY_SCRIPT="${WAL_REPLAY_SCRIPT:-${REPO_ROOT}/tests/heavy/hnsw_wal_crash_replay.sh}"
STANDBY_SCRIPT="${STANDBY_SCRIPT:-${REPO_ROOT}/tests/heavy/hnsw_replica_promotion.sh}"
RELATIONS_SCRIPT="${RELATIONS_SCRIPT:-${REPO_ROOT}/tests/heavy/hnsw_relation_kinds.sh}"

FAILPOINTS='before_page_initialization,after_page_initialization,before_append,after_append,before_rewiring,after_rewiring,before_generic_xlog_finish,after_generic_xlog_finish,before_metapage_publication,after_metapage_publication'
GATES=(hnsw-wal-crash-replay hnsw-standby-promotion hnsw-relation-kinds)
KINDS=(crash-replay standby-promotion relation-kinds)
OWNERS=(context-pg context-pg context-pg)
TOOLS=("cargo-pgrx,pg_ctl,psql" "pg_basebackup,pg_ctl,psql" "cargo-pgrx,psql")
CALLBACKS=(HnswPhysicalFailpoint none none)
FAILPOINT_ROWS=("${FAILPOINTS}" none none)
SCRIPTS=(tests/heavy/hnsw_wal_crash_replay.sh tests/heavy/hnsw_replica_promotion.sh tests/heavy/hnsw_relation_kinds.sh)
COMMANDS=(
  "HNSW_FAILPOINTS=${FAILPOINTS} PG_VERSION=pg17 PG_FEATURE='pg17 pg_test' bash tests/heavy/hnsw_wal_crash_replay.sh"
  "PGRX_DATA_DIR=<pgrx-data-dir> PG_VERSION=pg17 bash tests/heavy/hnsw_replica_promotion.sh"
  "PG_VERSION=pg17 bash tests/heavy/hnsw_relation_kinds.sh"
)

pg_major=""
out_dir=""
pgrx_data_dir=""
mode=""

usage() {
  cat <<'USAGE'
Usage: scripts/run-pg17-recovery-report.sh --pg-major 17 (--plan | --dry-run | --approve) [options]

Render or execute the PG17 recovery manifest. Approval is explicit because the
rows stop/restart PostgreSQL and create a temporary streaming replica.

Options:
  --pg-major N       PostgreSQL major. The product-build manifest supports 17.
  --out-dir PATH     Report directory. Defaults to target/pg17-recovery/<SHA>.
  --pgrx-data-dir P  Required by --approve for the standby/promotion row.
  --plan             Print the canonical TSV manifest without writing or running.
  --dry-run          Write non-executing report rows and logs.
  --approve          Execute every non-skippable manifest row.
  -h, --help         Show this help text.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --pg-major)
      [[ $# -ge 2 ]] || { echo "--pg-major requires a value" >&2; exit 2; }
      pg_major="$2"
      shift 2
      ;;
    --out-dir)
      [[ $# -ge 2 ]] || { echo "--out-dir requires a value" >&2; exit 2; }
      out_dir="$2"
      shift 2
      ;;
    --pgrx-data-dir)
      [[ $# -ge 2 ]] || { echo "--pgrx-data-dir requires a value" >&2; exit 2; }
      pgrx_data_dir="$2"
      shift 2
      ;;
    --plan|--dry-run|--approve)
      [[ -z "${mode}" ]] || { echo "--plan, --dry-run, and --approve are mutually exclusive" >&2; exit 2; }
      mode="${1#--}"
      shift
      ;;
    -h|--help)
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

[[ "${pg_major}" == "17" ]] || { echo "--pg-major must be 17 for this product-build manifest" >&2; exit 2; }
[[ -n "${mode}" ]] || { echo "choose one of --plan, --dry-run, or --approve" >&2; exit 2; }

print_plan() {
  printf 'gate\tkind\towner\ttools\tcallback\tfailpoints\tscript\tcommand\n'
  local index
  for ((index = 0; index < ${#GATES[@]}; index++)); do
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
      "${GATES[index]}" "${KINDS[index]}" "${OWNERS[index]}" \
      "${TOOLS[index]}" "${CALLBACKS[index]}" "${FAILPOINT_ROWS[index]}" \
      "${SCRIPTS[index]}" "${COMMANDS[index]}"
  done
}

if [[ "${mode}" == "plan" ]]; then
  print_plan
  exit 0
fi

if [[ -z "${out_dir}" ]]; then
  out_dir="${REPO_ROOT}/target/pg17-recovery/$(git -C "${REPO_ROOT}" rev-parse HEAD)"
elif [[ "${out_dir}" != /* ]]; then
  out_dir="${REPO_ROOT}/${out_dir}"
fi
out_dir="$("${PYTHON_BIN}" -c 'import os, sys; print(os.path.realpath(sys.argv[1]))' "${out_dir}")"
[[ -n "${out_dir}" && "${out_dir}" != "/" ]] || { echo "--out-dir must be a non-root path" >&2; exit 2; }

if [[ "${mode}" == "approve" && -z "${pgrx_data_dir}" ]]; then
  echo "--pgrx-data-dir is required with --approve" >&2
  exit 2
fi

logs_dir="${out_dir}/logs"
summary="${out_dir}/summary.tsv"
report="${out_dir}/report.md"
mkdir -p "${logs_dir}"
printf 'gate\tkind\towner\tstatus\texit_code\tlog\tcommand\n' >"${summary}"

run_gate() {
  case "$1" in
    hnsw-wal-crash-replay)
      HNSW_FAILPOINTS="${FAILPOINTS}" PG_VERSION=pg17 PG_FEATURE='pg17 pg_test' \
        bash "${WAL_REPLAY_SCRIPT}"
      ;;
    hnsw-standby-promotion)
      PGRX_DATA_DIR="${pgrx_data_dir}" PG_VERSION=pg17 bash "${STANDBY_SCRIPT}"
      ;;
    hnsw-relation-kinds)
      PG_VERSION=pg17 bash "${RELATIONS_SCRIPT}"
      ;;
    *)
      echo "unknown recovery gate: $1" >&2
      return 2
      ;;
  esac
}

overall=0
index=0
for gate in "${GATES[@]}"; do
  kind="${KINDS[index]}"
  owner="${OWNERS[index]}"
  command="${COMMANDS[index]}"
  log_relative="logs/${gate}.log"
  log_file="${out_dir}/${log_relative}"
  if [[ "${mode}" == "dry-run" ]]; then
    status="dry-run"
    exit_code=0
    {
      printf 'gate: %s\nkind: %s\nowner: %s\npg major: %s\ncommand: %s\nstatus: dry-run\n' \
        "${gate}" "${kind}" "${owner}" "${pg_major}" "${command}"
    } >"${log_file}"
  elif (cd "${REPO_ROOT}" && run_gate "${gate}") >"${log_file}" 2>&1; then
    status="pass"
    exit_code=0
  else
    exit_code=$?
    status="fail"
    overall=1
  fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${gate}" "${kind}" "${owner}" "${status}" "${exit_code}" \
    "${log_relative}" "${command}" >>"${summary}"
  index=$((index + 1))
done

git_state="not-checked"
approval="incomplete"
if [[ "${mode}" == "approve" ]]; then
  if [[ -d "${REPO_ROOT}/.git" && -z "$(cd "${REPO_ROOT}" && "${GIT_BIN}" status --porcelain)" ]]; then
    git_state="clean"
    [[ "${overall}" == "0" ]] && approval="complete"
  else
    git_state="dirty-or-unavailable"
  fi
fi

{
  printf '# PG17 Recovery Report\n\n'
  printf -- '- PostgreSQL major: `%s`\n' "${pg_major}"
  printf -- '- Execution: `%s`\n' "${mode}"
  printf -- '- Manifest rows: `%s`\n' "${#GATES[@]}"
  printf -- '- Worktree: `%s`\n' "${git_state}"
  printf -- '- Approval: `%s`\n\n' "${approval}"
  printf '| Gate | Kind | Owner | Status | Log |\n|---|---|---|---|---|\n'
  while IFS=$'\t' read -r gate kind owner status _exit_code log_relative _command; do
    [[ "${gate}" == "gate" ]] && continue
    printf '| `%s` | `%s` | `%s` | `%s` | [%s](%s) |\n' \
      "${gate}" "${kind}" "${owner}" "${status}" "${log_relative}" "${log_relative}"
  done <"${summary}"
  if [[ "${approval}" != "complete" ]]; then
    printf '\nApproval requires explicit --approve, passing every row, and a clean worktree.\n'
  fi
} >"${report}"

if [[ "${overall}" != "0" ]]; then
  echo "PG17 recovery report contains failing rows: ${report}" >&2
  exit 1
fi

echo "PG17 recovery report written: ${report}"
