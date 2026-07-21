#!/usr/bin/env bash
set -euo pipefail

source_roots=()
for source_root in crates tests benches fuzz; do
  if [[ -d "${source_root}" ]]; then
    source_roots+=("${source_root}")
  fi
done

if [[ "${#source_roots[@]}" -gt 0 ]]; then
  if find "${source_roots[@]}" -type f \( -name '*.rs' -o -name '*.sql' -o -name '*.md' \) -print0 \
    | xargs -0 grep -nE 'PLAN\.md|TDD.{0,8}by|rust-planning §|rust-implementing §|skill-section' >/tmp/pgcontext-source-hygiene.txt
  then
    cat /tmp/pgcontext-source-hygiene.txt
    exit 1
  fi

  oversized_files="$(find "${source_roots[@]}" -type f -name '*.rs' -print0 \
    | xargs -0 wc -l \
    | awk '$2 != "total" && $1 > 1000 { print $2 "|" $1 }')"
else
  oversized_files=""
fi

large_file_allowlist="scripts/source-hygiene-large-files.data"
while IFS='|' read -r oversized_file line_count; do
  [[ -n "${oversized_file}" ]] || continue
  maximum="$(awk -F'|' -v path="${oversized_file}" '$1 == path { print $2 }' "${large_file_allowlist}")"
  if [[ -z "${maximum}" ]]; then
    echo "Rust source file exceeds 1,000 lines without a reviewed size pin: ${oversized_file} (${line_count})"
    exit 1
  fi
  if (( line_count > maximum )); then
    echo "Pinned large Rust source grew: ${oversized_file} (${line_count} > ${maximum})"
    exit 1
  fi
done <<<"${oversized_files}"

while IFS='|' read -r pinned_file maximum; do
  [[ -n "${pinned_file}" && "${pinned_file}" != \#* ]] || continue
  if [[ ! -f "${pinned_file}" ]]; then
    echo "Pinned large Rust source is missing: ${pinned_file}"
    exit 1
  fi
  line_count="$(wc -l <"${pinned_file}")"
  if (( line_count <= 1000 )); then
    echo "Large-source pin is stale and must be removed: ${pinned_file} (${line_count})"
    exit 1
  fi
  if (( maximum < line_count )); then
    echo "Pinned large Rust source exceeds its reviewed size: ${pinned_file} (${line_count} > ${maximum})"
    exit 1
  fi
done <"${large_file_allowlist}"

scripts/check-unsafe-safety-comments.sh
