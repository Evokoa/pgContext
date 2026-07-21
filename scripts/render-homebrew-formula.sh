#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARCHIVE=""
OUT_DIR="${ROOT}/target/homebrew-formula"

usage() {
  cat <<'USAGE'
Usage: scripts/render-homebrew-formula.sh --archive PATH [--out-dir PATH]

Render the Evokoa/tap pgcontext and pgrx@0.19.1 formula updates.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --archive)
      [[ $# -ge 2 ]] || { echo "--archive requires a path" >&2; exit 2; }
      ARCHIVE="$2"
      shift 2
      ;;
    --out-dir)
      [[ $# -ge 2 ]] || { echo "--out-dir requires a path" >&2; exit 2; }
      OUT_DIR="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

[[ -n "${ARCHIVE}" ]] || { echo "--archive is required" >&2; exit 2; }
if [[ "${ARCHIVE}" != /* ]]; then
  ARCHIVE="${ROOT}/${ARCHIVE}"
fi
if [[ "${OUT_DIR}" != /* ]]; then
  OUT_DIR="${ROOT}/${OUT_DIR}"
fi
case "${OUT_DIR}" in
  "" | "/") echo "--out-dir must be a non-root path" >&2; exit 2 ;;
esac

archive_name="$(basename "${ARCHIVE}")"
if [[ ! "${archive_name}" =~ ^pgContext-([0-9]+\.[0-9]+\.[0-9]+)\.zip$ ]]; then
  echo "archive must be named pgContext-X.Y.Z.zip" >&2
  exit 2
fi
version="${BASH_REMATCH[1]}"
"${ROOT}/scripts/verify-pgxn-dist.py" --tag "v${version}" "${ARCHIVE}"

if command -v shasum >/dev/null 2>&1; then
  checksum="$(shasum -a 256 "${ARCHIVE}" | awk '{print $1}')"
else
  checksum="$(sha256sum "${ARCHIVE}" | awk '{print $1}')"
fi

mkdir -p "${OUT_DIR}"
sed -e "s/@VERSION@/${version}/g" -e "s/@SHA256@/${checksum}/g" \
  "${ROOT}/release/homebrew/pgcontext.rb.in" >"${OUT_DIR}/pgcontext.rb"
cp "${ROOT}/release/homebrew/pgrx@0.19.1.rb" "${OUT_DIR}/pgrx@0.19.1.rb"

printf 'pgcontext_formula=%s\n' "${OUT_DIR}/pgcontext.rb"
printf 'pgrx_formula=%s\n' "${OUT_DIR}/pgrx@0.19.1.rb"
printf 'source_sha256=%s\n' "${checksum}"
