#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${ROOT}/dist"
ALLOW_DIRTY=0

usage() {
  cat <<'USAGE'
Usage: scripts/build-pgxn-dist.sh [options] TAG

Build pgContext-X.Y.Z.zip from the current committed release candidate.

Options:
  --out-dir PATH  Output directory. Defaults to ./dist.
  --allow-dirty   Build committed HEAD for diagnostics from a dirty checkout.
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
    *)
      break
      ;;
  esac
done

[[ $# -eq 1 ]] || { usage >&2; exit 2; }
TAG="$1"
if [[ ! "${TAG}" =~ ^v([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
  echo "TAG must use vX.Y.Z form: ${TAG}" >&2
  exit 2
fi
VERSION="${TAG#v}"

if [[ "${OUT_DIR}" != /* ]]; then
  OUT_DIR="${ROOT}/${OUT_DIR}"
fi
case "${OUT_DIR}" in
  "" | "/") echo "--out-dir must be a non-root path" >&2; exit 2 ;;
esac

if [[ -n "$(git -C "${ROOT}" status --short)" && "${ALLOW_DIRTY}" -ne 1 ]]; then
  echo "refusing to package a dirty worktree; commit changes or pass --allow-dirty" >&2
  exit 1
fi

"${ROOT}/scripts/validate-release.py" --tag "${TAG}"
mkdir -p "${OUT_DIR}"
ARCHIVE="${OUT_DIR}/pgContext-${VERSION}.zip"
rm -f "${ARCHIVE}"
git -C "${ROOT}" archive --format=zip --prefix="pgContext-${VERSION}/" \
  --output="${ARCHIVE}" HEAD
"${ROOT}/scripts/verify-pgxn-dist.py" --tag "${TAG}" "${ARCHIVE}"
printf '%s\n' "${ARCHIVE}"
