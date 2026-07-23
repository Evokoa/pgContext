#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
CARGO_BIN="${CARGO_BIN:-cargo}"
RUSTC_BIN="${RUSTC_BIN:-rustc}"
PYTHON_BIN="${PYTHON_BIN:-python3}"
HNSW_CALLBACK_CHECKER="${HNSW_CALLBACK_CHECKER:-${REPO_ROOT}/scripts/check-hnsw-callback-guards.sh}"
UNSAFE_COMMENT_CHECKER="${UNSAFE_COMMENT_CHECKER:-${REPO_ROOT}/scripts/check-unsafe-safety-comments.sh}"

GATES=(
  callback-source-inventory
  unsafe-safety-comments
  storage-segment-miri
  storage-mapped-view-miri
  storage-mapped-real-asan
  storage-mapped-real-tsan
  hnsw-pg17-asan
  hnsw-pg17-tsan
)
KINDS=(static static miri miri asan tsan asan tsan)
OWNERS=(context-pg workspace context-storage context-storage context-storage context-storage context-pg context-pg)
COMMANDS=(
  'scripts/check-hnsw-callback-guards.sh'
  'scripts/check-unsafe-safety-comments.sh'
  'MIRIFLAGS=-Zmiri-disable-isolation cargo +nightly miri test -p context-storage --test segment_format'
  'MIRIFLAGS=-Zmiri-disable-isolation cargo +nightly miri test -p context-storage --test mapped_hnsw_view'
  'RUSTFLAGS=-Zsanitizer=address cargo +nightly test -Zbuild-std --target <host-target> -p context-storage --test mapped_generation_subprocess'
  'RUSTFLAGS=-Zsanitizer=thread cargo +nightly test -Zbuild-std --target <host-target> -p context-storage --test mapped_generation_subprocess'
  'PG_CONFIG=<pg17-config> RUSTFLAGS=-Zsanitizer=address cargo +nightly pgrx test -p context-pg pg17 hnsw'
  'PG_CONFIG=<pg17-config> RUSTFLAGS=-Zsanitizer=thread cargo +nightly pgrx test -p context-pg pg17 hnsw'
)

pg_major=""
out_dir=""
mode="run"

usage() {
  cat <<'USAGE'
Usage: scripts/run-unsafe-hardening-report.sh --pg-major 17 [options]

Run the frozen unsafe-hardening manifest or render its deterministic plan.

Options:
  --pg-major N    PostgreSQL major. The product-build manifest supports 17.
  --out-dir PATH  Report directory. Defaults to target/unsafe-hardening/<SHA>.
  --plan          Print the canonical TSV manifest without writing or running.
  --dry-run       Write dry-run report rows and logs without invoking tools.
  -h, --help      Show this help text.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --pg-major)
      [[ $# -ge 2 ]] || {
        echo "--pg-major requires a value" >&2
        exit 2
      }
      pg_major="$2"
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
    --plan)
      [[ "${mode}" == "run" ]] || {
        echo "--plan and --dry-run are mutually exclusive" >&2
        exit 2
      }
      mode="plan"
      shift
      ;;
    --dry-run)
      [[ "${mode}" == "run" ]] || {
        echo "--plan and --dry-run are mutually exclusive" >&2
        exit 2
      }
      mode="dry-run"
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

if [[ "${pg_major}" != "17" ]]; then
  echo "--pg-major must be 17 for this product-build manifest" >&2
  exit 2
fi

print_plan() {
  printf 'gate\tkind\towner\tpg_major\tcommand\n'
  local index
  for ((index = 0; index < ${#GATES[@]}; index++)); do
    printf '%s\t%s\t%s\t%s\t%s\n' \
      "${GATES[index]}" \
      "${KINDS[index]}" \
      "${OWNERS[index]}" \
      "${pg_major}" \
      "${COMMANDS[index]}"
  done
}

if [[ "${mode}" == "plan" ]]; then
  print_plan
  exit 0
fi

if [[ -z "${out_dir}" ]]; then
  out_dir="${REPO_ROOT}/target/unsafe-hardening/$(git -C "${REPO_ROOT}" rev-parse HEAD)"
elif [[ "${out_dir}" != /* ]]; then
  out_dir="${REPO_ROOT}/${out_dir}"
fi
out_dir="$("${PYTHON_BIN}" -c \
  'import os, sys; print(os.path.realpath(sys.argv[1]))' "${out_dir}")"
if [[ -z "${out_dir}" || "${out_dir}" == "/" ]]; then
  echo "--out-dir must be a non-root path" >&2
  exit 2
fi

logs_dir="${out_dir}/logs"
summary="${out_dir}/summary.tsv"
report="${out_dir}/report.md"
mkdir -p "${logs_dir}"
printf 'gate\tkind\towner\tstatus\texit_code\tlog\tcommand\n' >"${summary}"

pg_config=""
pg_config_version="not-required"
host_target="${RUST_TARGET:-}"
if [[ "${mode}" == "run" ]]; then
  if [[ -z "${host_target}" ]]; then
    host_target="$("${RUSTC_BIN}" -vV | sed -n 's/^host: //p')"
  fi
  if [[ -z "${host_target}" ]]; then
    echo "could not determine Rust host target for sanitizer rows" >&2
    exit 1
  fi
  pg_config="${PG_CONFIG:-${PG17_CONFIG:-}}"
  if [[ -z "${pg_config}" ]]; then
    for candidate in \
      /opt/homebrew/opt/postgresql@17/bin/pg_config \
      /usr/local/opt/postgresql@17/bin/pg_config \
      /usr/lib/postgresql/17/bin/pg_config
    do
      if [[ -x "${candidate}" ]]; then
        pg_config="${candidate}"
        break
      fi
    done
  fi
  if [[ -z "${pg_config}" || ! -x "${pg_config}" ]]; then
    echo "PG17 pg_config is required for sanitizer rows" >&2
    exit 1
  fi
  pg_config_version="$("${pg_config}" --version)"
  case "${pg_config_version}" in
    *' 17.'*) ;;
    *)
      echo "pg_config must report PostgreSQL 17: ${pg_config_version}" >&2
      exit 1
      ;;
  esac
fi

run_gate() {
  local gate="$1"
  case "${gate}" in
    callback-source-inventory)
      "${HNSW_CALLBACK_CHECKER}"
      ;;
    unsafe-safety-comments)
      "${UNSAFE_COMMENT_CHECKER}"
      ;;
    storage-segment-miri)
      PG_CONFIG= RUSTFLAGS= MIRIFLAGS=-Zmiri-disable-isolation \
        "${CARGO_BIN}" +nightly miri test -p context-storage --test segment_format
      ;;
    storage-mapped-view-miri)
      PG_CONFIG= RUSTFLAGS= MIRIFLAGS=-Zmiri-disable-isolation \
        "${CARGO_BIN}" +nightly miri test -p context-storage --test mapped_hnsw_view
      ;;
    storage-mapped-real-asan)
      PG_CONFIG= MIRIFLAGS= RUSTFLAGS=-Zsanitizer=address \
        "${CARGO_BIN}" +nightly test -Zbuild-std --target "${host_target}" \
          -p context-storage --test mapped_generation_subprocess
      ;;
    storage-mapped-real-tsan)
      PG_CONFIG= MIRIFLAGS= RUSTFLAGS=-Zsanitizer=thread \
        "${CARGO_BIN}" +nightly test -Zbuild-std --target "${host_target}" \
          -p context-storage --test mapped_generation_subprocess
      ;;
    hnsw-pg17-asan)
      PG_CONFIG="${pg_config}" MIRIFLAGS= RUSTFLAGS=-Zsanitizer=address \
        "${CARGO_BIN}" +nightly pgrx test -p context-pg pg17 hnsw
      ;;
    hnsw-pg17-tsan)
      PG_CONFIG="${pg_config}" MIRIFLAGS= RUSTFLAGS=-Zsanitizer=thread \
        "${CARGO_BIN}" +nightly pgrx test -p context-pg pg17 hnsw
      ;;
    *)
      echo "unknown unsafe-hardening gate: ${gate}" >&2
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
      printf 'gate: %s\n' "${gate}"
      printf 'kind: %s\n' "${kind}"
      printf 'owner: %s\n' "${owner}"
      printf 'pg major: %s\n' "${pg_major}"
      printf 'command: %s\n' "${command}"
      printf 'status: dry-run\n'
    } >"${log_file}"
  else
    if (
      cd "${REPO_ROOT}" || exit $?
      run_gate "${gate}"
    ) >"${log_file}" 2>&1; then
      status="pass"
      exit_code=0
    else
      exit_code=$?
      status="fail"
      overall=1
    fi
  fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "${gate}" "${kind}" "${owner}" "${status}" "${exit_code}" \
    "${log_relative}" "${command}" >>"${summary}"
  index=$((index + 1))
done

{
  printf '# Unsafe Hardening Report\n\n'
  printf -- '- PostgreSQL major: `%s`\n' "${pg_major}"
  printf -- '- Execution: `%s`\n' "${mode}"
  printf -- '- Manifest rows: `%s`\n' "${#GATES[@]}"
  printf -- '- pg_config: `%s`\n\n' "${pg_config_version}"
  printf '| Gate | Kind | Owner | Status | Log |\n'
  printf '|---|---|---|---|---|\n'
  while IFS=$'\t' read -r gate kind owner status _exit_code log_relative _command; do
    [[ "${gate}" == "gate" ]] && continue
    printf '| `%s` | `%s` | `%s` | `%s` | [%s](%s) |\n' \
      "${gate}" "${kind}" "${owner}" "${status}" "${log_relative}" "${log_relative}"
  done <"${summary}"
} >"${report}"

if [[ "${overall}" != "0" ]]; then
  echo "unsafe hardening report contains failing rows: ${report}" >&2
  exit 1
fi

echo "unsafe hardening report written: ${report}"
