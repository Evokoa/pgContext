#!/usr/bin/env bash
set -euo pipefail

PLAN_ONLY=0
if [[ "${1:-}" == "--plan" ]]; then
  PLAN_ONLY=1
  shift
fi
[[ $# -eq 4 ]] || {
  echo "usage: scripts/promote-release-image.sh [--plan] IMAGE TAG SHA EXPECTED_DIGEST" >&2
  exit 2
}
IMAGE="$1"
TAG="$2"
SHA="$3"
EXPECTED_DIGEST="$4"

[[ "${IMAGE}" =~ ^[a-z0-9.-]+(/[a-z0-9._-]+)+$ ]] || {
  echo "IMAGE must be a lowercase registry/repository name" >&2
  exit 2
}
[[ "${TAG}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "TAG must use vX.Y.Z form" >&2
  exit 2
}
[[ "${SHA}" =~ ^[0-9a-f]{40}$ ]] || {
  echo "SHA must be a full lowercase 40-character commit SHA" >&2
  exit 2
}
[[ "${EXPECTED_DIGEST}" =~ ^sha256:[0-9a-f]{64}$ ]] || {
  echo "EXPECTED_DIGEST must be a sha256 OCI manifest digest" >&2
  exit 2
}

VERSION="${TAG#v}"
SHORT_SHA="${SHA:0:12}"
SHA_SOURCE="${IMAGE}:pg17-sha-${SHORT_SHA}"
PREPARED_SOURCE="${IMAGE}:pg17-${TAG}-prepared"
TARGETS=(
  "${IMAGE}:pg17-${TAG}"
  "${IMAGE}:pg17-${VERSION}"
  "${IMAGE}:pg17"
  "${IMAGE}:${TAG}"
  "${IMAGE}:${VERSION}"
  "${IMAGE}:latest"
)
IMMUTABLE_TARGETS=(
  "${IMAGE}:pg17-${TAG}"
  "${IMAGE}:pg17-${VERSION}"
  "${IMAGE}:${TAG}"
  "${IMAGE}:${VERSION}"
)

if [[ "${PLAN_ONLY}" -eq 1 ]]; then
  printf 'source_sha=%s\nsource_prepared=%s\nexpected_digest=%s\n' \
    "${SHA_SOURCE}" "${PREPARED_SOURCE}" "${EXPECTED_DIGEST}"
  printf 'target=%s\n' "${TARGETS[@]}"
  exit 0
fi

resolve_digest() {
  local reference="$1"
  local digest
  digest="$(docker buildx imagetools inspect "${reference}" | awk '/^Digest:/ { print $2; exit }')"
  [[ "${digest}" =~ ^sha256:[0-9a-f]{64}$ ]] || {
    echo "could not resolve an OCI manifest digest for ${reference}" >&2
    exit 1
  }
  printf '%s\n' "${digest}"
}

resolve_optional_digest() {
  local reference="$1"
  local output
  local digest
  if output="$(docker buildx imagetools inspect "${reference}" 2>&1)"; then
    digest="$(awk '/^Digest:/ { print $2; exit }' <<<"${output}")"
    [[ "${digest}" =~ ^sha256:[0-9a-f]{64}$ ]] || {
      echo "could not resolve an OCI manifest digest for ${reference}" >&2
      return 1
    }
    printf '%s\n' "${digest}"
    return 0
  fi
  case "${output}" in
    *"manifest unknown"* | *": not found"* | *"no such manifest"*) return 3 ;;
    *)
      echo "failed to inspect immutable tag ${reference}: ${output}" >&2
      return 1
      ;;
  esac
}

for source in "${SHA_SOURCE}" "${PREPARED_SOURCE}"; do
  actual="$(resolve_digest "${source}")"
  [[ "${actual}" == "${EXPECTED_DIGEST}" ]] || {
    echo "${source} resolves to ${actual}, expected ${EXPECTED_DIGEST}" >&2
    exit 1
  }
done

for target in "${IMMUTABLE_TARGETS[@]}"; do
  if actual="$(resolve_optional_digest "${target}")"; then
    [[ "${actual}" == "${EXPECTED_DIGEST}" ]] || {
      echo "refusing to overwrite immutable tag ${target}: ${actual} != ${EXPECTED_DIGEST}" >&2
      exit 1
    }
  else
    result=$?
    [[ "${result}" -eq 3 ]] || exit "${result}"
  fi
done

tag_args=()
for target in "${TARGETS[@]}"; do
  tag_args+=(--tag "${target}")
done
docker buildx imagetools create "${tag_args[@]}" "${IMAGE}@${EXPECTED_DIGEST}"

for target in "${TARGETS[@]}"; do
  actual="$(resolve_digest "${target}")"
  [[ "${actual}" == "${EXPECTED_DIGEST}" ]] || {
    echo "promoted tag ${target} resolves to ${actual}, expected ${EXPECTED_DIGEST}" >&2
    exit 1
  }
done
