#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
REGISTRY="${REPO_ROOT}/fuzz/smoke-targets.txt"
RUNS=64
SEED=1337
SELECTED_TARGET=""
DRY_RUN=false
LIST_ONLY=false

usage() {
  cat <<'USAGE'
Usage: scripts/run-fuzz-smoke.sh [options]

Run every registered fuzz target with deterministic, bounded libFuzzer work.

Options:
  --target NAME  Run one registered target.
  --runs N       Total deterministic runs per target (1..=10000; default 64).
  --seed N       Fixed unsigned 32-bit seed (default 1337).
  --dry-run      Validate inputs and print commands without executing cargo.
  --list         Print registered target names and exit.
  -h, --help     Show this help text.
USAGE
}

decimal_at_most() {
  local value="$1"
  local maximum="$2"
  [[ "${value}" =~ ^(0|[1-9][0-9]*)$ ]] || return 1
  if [[ "${#value}" -lt "${#maximum}" ]]; then
    return 0
  fi
  [[ "${#value}" -eq "${#maximum}" ]] || return 1
  [[ "${value}" == "${maximum}" || "${value}" < "${maximum}" ]]
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --target)
      [[ $# -ge 2 ]] || { echo 'missing value for --target' >&2; exit 2; }
      SELECTED_TARGET="$2"
      shift 2
      ;;
    --runs)
      [[ $# -ge 2 ]] || { echo 'missing value for --runs' >&2; exit 2; }
      RUNS="$2"
      shift 2
      ;;
    --seed)
      [[ $# -ge 2 ]] || { echo 'missing value for --seed' >&2; exit 2; }
      SEED="$2"
      shift 2
      ;;
    --dry-run)
      DRY_RUN=true
      shift
      ;;
    --list)
      LIST_ONLY=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown fuzz smoke option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if ! decimal_at_most "${RUNS}" 10000 || [[ "${RUNS}" == 0 ]]; then
  echo 'fuzz smoke runs must be an integer in 1..=10000' >&2
  exit 2
fi
if ! decimal_at_most "${SEED}" 4294967295; then
  echo 'fuzz smoke seed must be an unsigned 32-bit integer' >&2
  exit 2
fi
[[ -f "${REGISTRY}" ]] || { echo 'missing fuzz smoke registry: fuzz/smoke-targets.txt' >&2; exit 1; }
[[ ! -L "${REGISTRY}" ]] || { echo 'fuzz smoke registry must not be a symlink' >&2; exit 1; }

targets=()
max_lengths=()
while IFS= read -r row || [[ -n "${row}" ]]; do
  [[ -n "${row}" ]] || continue
  [[ "${row}" == \#* ]] && continue
  if [[ ! "${row}" =~ ^([a-z0-9_]+)\|([0-9]+)$ ]]; then
    echo "invalid fuzz smoke registry row: ${row}" >&2
    exit 1
  fi
  target="${BASH_REMATCH[1]}"
  max_len="${BASH_REMATCH[2]}"
  if ! decimal_at_most "${max_len}" 1048576 || [[ "${max_len}" == 0 ]]; then
    echo "invalid fuzz smoke max_len for ${target}: ${max_len}" >&2
    exit 1
  fi
  for registered in "${targets[@]:-}"; do
    [[ "${registered}" != "${target}" ]] || { echo "duplicate fuzz smoke target: ${target}" >&2; exit 1; }
  done
  targets+=("${target}")
  max_lengths+=("${max_len}")
done <"${REGISTRY}"

[[ "${#targets[@]}" -gt 0 ]] || { echo 'fuzz smoke registry is empty' >&2; exit 1; }

if "${LIST_ONLY}"; then
  printf '%s\n' "${targets[@]}"
  exit 0
fi

selected_indexes=()
if [[ -n "${SELECTED_TARGET}" ]]; then
  for index in "${!targets[@]}"; do
    if [[ "${targets[${index}]}" == "${SELECTED_TARGET}" ]]; then
      selected_indexes+=("${index}")
    fi
  done
  [[ "${#selected_indexes[@]}" -eq 1 ]] || {
    echo "fuzz smoke target is not registered: ${SELECTED_TARGET}" >&2
    exit 1
  }
else
  selected_indexes=("${!targets[@]}")
fi

for index in "${selected_indexes[@]}"; do
  target="${targets[${index}]}"
  max_len="${max_lengths[${index}]}"
  target_source="fuzz/fuzz_targets/${target}.rs"
  corpus="fuzz/corpus/${target}"
  work_corpus="target/fuzz-smoke/corpus/${target}"
  artifact_dir="target/fuzz-smoke/artifacts/${target}"

  [[ -f "${REPO_ROOT}/${target_source}" ]] || {
    echo "fuzz smoke target source is missing: ${target_source}" >&2
    exit 1
  }
  [[ ! -L "${REPO_ROOT}/${target_source}" ]] || {
    echo "fuzz smoke target source must not be a symlink: ${target_source}" >&2
    exit 1
  }
  [[ -d "${REPO_ROOT}/${corpus}" ]] || {
    echo "fuzz smoke corpus is missing: ${corpus}" >&2
    exit 1
  }
  [[ ! -L "${REPO_ROOT}/${corpus}" ]] || {
    echo "fuzz smoke corpus must not be a symlink: ${corpus}" >&2
    exit 1
  }
  if find "${REPO_ROOT}/${corpus}" -type l -print -quit | grep -q .; then
    echo "fuzz smoke corpus contains a symlink: ${corpus}" >&2
    exit 1
  fi
  if ! find "${REPO_ROOT}/${corpus}" -type f -print -quit | grep -q .; then
    echo "fuzz smoke corpus is empty: ${corpus}" >&2
    exit 1
  fi

  command=(
    cargo +nightly fuzz run "${target}" "${work_corpus}" --
    "-runs=${RUNS}"
    "-seed=${SEED}"
    "-max_len=${max_len}"
    -timeout=5
    "-artifact_prefix=${artifact_dir}/"
  )
  if "${DRY_RUN}"; then
    printf '%s ' "${command[@]}"
    printf '\n'
  else
    rm -rf "${REPO_ROOT}/${work_corpus}"
    mkdir -p "${REPO_ROOT}/${work_corpus}" "${REPO_ROOT}/${artifact_dir}"
    cp -R "${REPO_ROOT}/${corpus}/." "${REPO_ROOT}/${work_corpus}/"
    (cd "${REPO_ROOT}" && "${command[@]}")
  fi
done
