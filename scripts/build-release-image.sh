#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${ROOT}/target/release-images"
ALLOW_DIRTY=0
PG_MAJOR=17

usage() {
  cat <<'USAGE'
Usage: scripts/build-release-image.sh [options] TAG

Build a local amd64+arm64 OCI image index with provenance attestations.

Options:
  --out-dir PATH  Output directory. Defaults under target/release-images.
  --pg-major N    PostgreSQL major to package: 17 or 18.
  --allow-dirty   Permit a diagnostic build from a dirty checkout.
  -h, --help      Show this help.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out-dir)
      [[ $# -ge 2 ]] || { echo "--out-dir requires a path" >&2; exit 2; }
      OUT_DIR="$2"
      shift 2
      ;;
    --allow-dirty)
      ALLOW_DIRTY=1
      shift
      ;;
    --pg-major)
      [[ $# -ge 2 ]] || { echo "--pg-major requires a value" >&2; exit 2; }
      PG_MAJOR="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    --*)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
    *) break ;;
  esac
done

case "${PG_MAJOR}" in
  17) POSTGRES_IMAGE="postgres:17-bookworm@sha256:4f736ae292687621d4dbe0d499ffd024a36bd2ee7d8ca6f2ccd4c800f047b394" ;;
  18) POSTGRES_IMAGE="postgres:18-bookworm@sha256:1961f96e6029a02c3812d7cb329a3b03a3ac2bb067058dec17b0f5596aca9296" ;;
  *) echo "--pg-major must be 17 or 18" >&2; exit 2 ;;
esac

[[ $# -eq 1 ]] || { usage >&2; exit 2; }
TAG="$1"
if [[ ! "${TAG}" =~ ^v([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
  echo "TAG must use vX.Y.Z form: ${TAG}" >&2
  exit 2
fi
VERSION="${TAG#v}"
REVISION="$(git -C "${ROOT}" rev-parse HEAD)"
ARTIFACT_REVISION="${REVISION}"
DIRTY_SUFFIX=""

if [[ -n "$(git -C "${ROOT}" status --short)" ]]; then
  if [[ "${ALLOW_DIRTY}" -ne 1 ]]; then
    echo "refusing to build release images from a dirty worktree" >&2
    exit 1
  fi
  ARTIFACT_REVISION="${REVISION}-dirty"
  DIRTY_SUFFIX="-dirty"
fi
if [[ "${OUT_DIR}" != /* ]]; then
  OUT_DIR="${ROOT}/${OUT_DIR}"
fi
case "${OUT_DIR}" in
  "" | "/") echo "--out-dir must be a non-root path" >&2; exit 2 ;;
esac

"${ROOT}/scripts/validate-release.py" --tag "${TAG}"
mkdir -p "${OUT_DIR}"
OCI_ARCHIVE="${OUT_DIR}/pgcontext-pg${PG_MAJOR}-${VERSION}-${REVISION:0:12}${DIRTY_SUFFIX}.oci.tar"
METADATA="${OUT_DIR}/pgcontext-pg${PG_MAJOR}-${VERSION}-${REVISION:0:12}${DIRTY_SUFFIX}.metadata.json"
IMAGE="ghcr.io/evokoa/pgcontext:pg${PG_MAJOR}-${TAG}-prepared"

docker buildx build "${ROOT}" \
  --file "${ROOT}/release/docker/Dockerfile" \
  --platform linux/amd64,linux/arm64 \
  --build-arg "PG_MAJOR=${PG_MAJOR}" \
  --build-arg "POSTGRES_IMAGE=${POSTGRES_IMAGE}" \
  --build-arg "VERSION=${VERSION}" \
  --build-arg "REVISION=${ARTIFACT_REVISION}" \
  --provenance mode=max \
  --tag "${IMAGE}" \
  --metadata-file "${METADATA}" \
  --output "type=oci,dest=${OCI_ARCHIVE}"

"${ROOT}/scripts/verify-oci-image.py" \
  --image "${IMAGE}" --pg-major "${PG_MAJOR}" --version "${VERSION}" \
  --revision "${ARTIFACT_REVISION}" "${OCI_ARCHIVE}"
printf 'image=%s\noci_archive=%s\nmetadata=%s\n' "${IMAGE}" "${OCI_ARCHIVE}" "${METADATA}"
