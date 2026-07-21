#!/usr/bin/env bash
set -euo pipefail

status=0

while IFS= read -r -d '' file; do
  if ! awk '
    /unsafe[[:space:]]*\{/ {
      if (previous_line !~ /SAFETY:/ &&
          previous_previous_line !~ /SAFETY:/ &&
          previous_third_line !~ /SAFETY:/ &&
          previous_fourth_line !~ /SAFETY:/) {
        printf "%s:%d: unsafe block missing nearby SAFETY comment\n", FILENAME, FNR
        failed = 1
      }
    }
    {
      previous_fourth_line = previous_third_line
      previous_third_line = previous_previous_line
      previous_previous_line = previous_line
      previous_line = $0
    }
    END {
      exit failed
    }
  ' "${file}"; then
    status=1
  fi
done < <(find crates/context-core crates/context-pg crates/context-storage -type f -name '*.rs' -print0)

exit "${status}"
