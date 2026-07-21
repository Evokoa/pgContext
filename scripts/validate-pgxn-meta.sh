#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
META_PATH="${1:-${ROOT}/META.json}"
VALIDATOR_IMAGE="pgxn/pgxn-tools@sha256:a54f1e66c563c7e47b34ed08597d0ee5da1168303b61ef47e66a4a9904ab849d"

[[ -f "${META_PATH}" ]] || {
  echo "PGXN metadata file does not exist: ${META_PATH}" >&2
  exit 2
}
META_DIR="$(cd "$(dirname "${META_PATH}")" && pwd)"
META_NAME="$(basename "${META_PATH}")"

if command -v validate_pgxn_meta >/dev/null 2>&1; then
  validate_pgxn_meta "${META_PATH}"
elif command -v docker >/dev/null 2>&1; then
  docker run --rm --platform linux/amd64 \
    --volume "${META_DIR}:/pgxn-meta:ro" \
    --workdir /pgxn-meta \
    "${VALIDATOR_IMAGE}" validate_pgxn_meta "${META_NAME}"
else
  echo "validate_pgxn_meta or Docker is required for canonical PGXN validation" >&2
  exit 1
fi
