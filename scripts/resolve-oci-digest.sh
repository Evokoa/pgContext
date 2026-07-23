#!/usr/bin/env bash
set -euo pipefail

[[ $# -eq 1 ]] || {
  echo "usage: scripts/resolve-oci-digest.sh IMAGE_REFERENCE" >&2
  exit 2
}
REFERENCE="$1"
ATTEMPTS="${PGCONTEXT_OCI_INSPECT_ATTEMPTS:-12}"
DELAY_SECONDS="${PGCONTEXT_OCI_INSPECT_DELAY_SECONDS:-5}"
[[ "${ATTEMPTS}" =~ ^[1-9][0-9]*$ ]] || {
  echo "PGCONTEXT_OCI_INSPECT_ATTEMPTS must be a positive integer" >&2
  exit 2
}
[[ "${DELAY_SECONDS}" =~ ^[0-9]+$ ]] || {
  echo "PGCONTEXT_OCI_INSPECT_DELAY_SECONDS must be a non-negative integer" >&2
  exit 2
}

output=""
for ((attempt = 1; attempt <= ATTEMPTS; attempt++)); do
  if output="$(docker buildx imagetools inspect "${REFERENCE}" 2>&1)"; then
    digest="$(awk '/^Digest:/ {print $2; exit}' <<<"${output}")"
    if [[ "${digest}" =~ ^sha256:[0-9a-f]{64}$ ]]; then
      printf '%s\n' "${digest}"
      exit 0
    fi
  fi
  if ((attempt < ATTEMPTS)); then
    sleep "${DELAY_SECONDS}"
  fi
done
echo "could not resolve OCI digest for ${REFERENCE}: ${output}" >&2
exit 1
