#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${ROOT}/target/release-images"
ALLOW_DIRTY=0

usage() {
  cat <<'USAGE'
Usage: scripts/build-release-image.sh [options] TAG

Build a local amd64+arm64 OCI image index with provenance attestations.

Options:
  --out-dir PATH  Output directory. Defaults under target/release-images.
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
OCI_ARCHIVE="${OUT_DIR}/pgcontext-pg17-${VERSION}-${REVISION:0:12}${DIRTY_SUFFIX}.oci.tar"
METADATA="${OUT_DIR}/pgcontext-pg17-${VERSION}-${REVISION:0:12}${DIRTY_SUFFIX}.metadata.json"
IMAGE="ghcr.io/evokoa/pgcontext:pg17-${TAG}-prepared"

docker buildx build "${ROOT}" \
  --file "${ROOT}/release/docker/Dockerfile" \
  --platform linux/amd64,linux/arm64 \
  --build-arg "VERSION=${VERSION}" \
  --build-arg "REVISION=${ARTIFACT_REVISION}" \
  --provenance mode=max \
  --tag "${IMAGE}" \
  --metadata-file "${METADATA}" \
  --output "type=oci,dest=${OCI_ARCHIVE}"

"${ROOT}/scripts/verify-oci-image.py" \
  --image "${IMAGE}" --version "${VERSION}" \
  --revision "${ARTIFACT_REVISION}" "${OCI_ARCHIVE}"
printf 'image=%s\noci_archive=%s\nmetadata=%s\n' "${IMAGE}" "${OCI_ARCHIVE}" "${METADATA}"
