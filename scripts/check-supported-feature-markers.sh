#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "usage: scripts/check-supported-feature-markers.sh CAPABILITY_CONTRACT SUPPORTED_FEATURES" >&2
  exit 2
fi

source_contract="$1"
supported_features="$2"

if [[ ! -f "${source_contract}" ]]; then
  echo "missing capability contract: ${source_contract}" >&2
  exit 1
fi
if [[ ! -f "${supported_features}" ]]; then
  echo "missing supported-features inventory: ${supported_features}" >&2
  exit 1
fi

markers="$(
  sed -n 's/.*<!-- capability:\(CAP-[A-Z0-9-]*\) -->.*/\1/p' "${supported_features}"
)"
duplicate_marker="$(printf '%s\n' "${markers}" | sort | uniq -d)"
if [[ -n "${duplicate_marker}" ]]; then
  echo "supported-features inventory contains duplicate capability markers: ${duplicate_marker}" >&2
  exit 1
fi

while IFS= read -r marker; do
  [[ -n "${marker}" ]] || continue
  if ! awk -F'|' -v id="${marker}" \
      'NR > 1 && $1 == id { found = 1 } END { exit !found }' "${source_contract}"; then
    echo "supported-features inventory references unknown capability ID: ${marker}" >&2
    exit 1
  fi
done <<<"${markers}"
