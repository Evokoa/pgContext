#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${ROOT}/dist"
ALLOW_DIRTY=0

usage() {
  cat <<'USAGE'
Usage: release/build-packages.sh [options] TAG

Build and verify the complete unsigned V1 source release payload.

Options:
  --out-dir PATH  Output directory. Defaults to ./dist and must be empty.
  --allow-dirty   Build committed HEAD for diagnostics and mark it dirty.
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
[[ "${TAG}" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || {
  echo "TAG must use vX.Y.Z form: ${TAG}" >&2
  exit 2
}
VERSION="${TAG#v}"
if [[ "${OUT_DIR}" != /* ]]; then
  OUT_DIR="${ROOT}/${OUT_DIR}"
fi
case "${OUT_DIR}" in
  "" | "/") echo "--out-dir must be a non-root path" >&2; exit 2 ;;
esac
if [[ -L "${OUT_DIR}" ]]; then
  echo "refusing to write a release payload through a symlink" >&2
  exit 1
fi
if [[ -e "${OUT_DIR}" && ! -d "${OUT_DIR}" ]]; then
  echo "release payload output path must be a directory" >&2
  exit 1
fi
if [[ -e "${OUT_DIR}" && -n "$(find "${OUT_DIR}" -mindepth 1 -maxdepth 1 -print -quit)" ]]; then
  echo "release payload output directory must be empty" >&2
  exit 1
fi

cd "${ROOT}"
DIRTY=0
if [[ -n "$(git status --porcelain=v1)" ]]; then
  if [[ "${ALLOW_DIRTY}" -ne 1 ]]; then
    echo "refusing to package a dirty worktree; commit changes or pass --allow-dirty" >&2
    exit 1
  fi
  DIRTY=1
fi

"${ROOT}/scripts/validate-release.py" --tag "${TAG}"
COMMIT="$(git rev-parse HEAD)"
SOURCE_DATE_EPOCH="$(git show -s --format=%ct HEAD)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-release-payload.XXXXXX")"
trap 'rm -rf "${WORK_DIR}"' EXIT
if [[ "${ALLOW_DIRTY}" -eq 1 ]]; then
  "${ROOT}/scripts/build-pgxn-dist.sh" --allow-dirty \
    --out-dir "${WORK_DIR}/first" "${TAG}" >/dev/null
  "${ROOT}/scripts/build-pgxn-dist.sh" --allow-dirty \
    --out-dir "${WORK_DIR}/second" "${TAG}" >/dev/null
else
  "${ROOT}/scripts/build-pgxn-dist.sh" \
    --out-dir "${WORK_DIR}/first" "${TAG}" >/dev/null
  "${ROOT}/scripts/build-pgxn-dist.sh" \
    --out-dir "${WORK_DIR}/second" "${TAG}" >/dev/null
fi
ARCHIVE="pgContext-${VERSION}.zip"
cmp "${WORK_DIR}/first/${ARCHIVE}" "${WORK_DIR}/second/${ARCHIVE}"

mkdir -p "${OUT_DIR}"
cp "${WORK_DIR}/first/${ARCHIVE}" "${OUT_DIR}/${ARCHIVE}"
cp LICENSE NOTICE release/ARTIFACT_POLICY.md "${OUT_DIR}/"
"${ROOT}/scripts/generate-release-sbom.py" \
  --output "${OUT_DIR}/SBOM.spdx.json" \
  --version "${VERSION}" --commit "${COMMIT}" \
  --source-date-epoch "${SOURCE_DATE_EPOCH}"
provenance_args=(
  --output "${OUT_DIR}/PROVENANCE.json"
  --archive "${OUT_DIR}/${ARCHIVE}"
  --tag "${TAG}"
  --version "${VERSION}"
  --commit "${COMMIT}"
  --source-date-epoch "${SOURCE_DATE_EPOCH}"
)
if [[ "${DIRTY}" -eq 1 ]]; then
  provenance_args+=(--dirty)
fi
"${ROOT}/scripts/generate-release-provenance.py" "${provenance_args[@]}"

CHECKSUMS="${OUT_DIR}/SHA256SUMS"
: >"${CHECKSUMS}"
for name in ARTIFACT_POLICY.md LICENSE NOTICE PROVENANCE.json SBOM.spdx.json "${ARCHIVE}"; do
  if command -v shasum >/dev/null 2>&1; then
    hash="$(shasum -a 256 "${OUT_DIR}/${name}" | awk '{print $1}')"
  else
    hash="$(sha256sum "${OUT_DIR}/${name}" | awk '{print $1}')"
  fi
  printf '%s  %s\n' "${hash}" "${name}" >>"${CHECKSUMS}"
done

verify_args=(--tag "${TAG}" --candidate-sha "${COMMIT}")
if [[ "${DIRTY}" -eq 1 ]]; then
  verify_args+=(--allow-dirty)
fi
"${ROOT}/scripts/verify-release-payload.py" "${verify_args[@]}" "${OUT_DIR}"
printf 'release payload written to %s\n' "${OUT_DIR}"
