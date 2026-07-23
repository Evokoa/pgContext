#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"

image='ghcr.io/evokoa/pgcontext'
tag='v0.1.0'
sha='4f43689334777909d1796c1c41b192d55484334b'
digest="sha256:$(printf 'a%.0s' {1..64})"
for pg_major in 17 18; do
  plan="$(scripts/promote-release-image.sh --plan "${image}" "${pg_major}" "${tag}" "${sha}" "${digest}")"
  grep -Fxq "source_sha=${image}:pg${pg_major}-sha-4f4368933477" <<<"${plan}"
  grep -Fxq "source_prepared=${image}:pg${pg_major}-v0.1.0-prepared" <<<"${plan}"
  expected_targets=3
  [[ "${pg_major}" == 17 ]] && expected_targets=6
  [[ "$(grep -c '^target=' <<<"${plan}")" == "${expected_targets}" ]]
  for target in "pg${pg_major}-v0.1.0" "pg${pg_major}-0.1.0" "pg${pg_major}"; do
    grep -Fxq "target=${image}:${target}" <<<"${plan}"
  done
done
for target in v0.1.0 0.1.0 latest; do
  grep -Fxq "target=${image}:${target}" <<<"$(scripts/promote-release-image.sh --plan "${image}" 17 "${tag}" "${sha}" "${digest}")"
done

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT
cp tests/shell/fixtures/release_tags_fake_docker.sh "${tmp}/docker"
chmod +x "${tmp}/docker"
export FAKE_DOCKER_LOG="${tmp}/docker.log"
export FAKE_DOCKER_STATE="${tmp}/registry-mutated"
export FAKE_TRANSIENT_STATE="${tmp}/inspect-retried"
export FAKE_EXPECTED_DIGEST="${digest}"
PATH="${tmp}:${PATH}" scripts/promote-release-image.sh \
  "${image}" 17 "${tag}" "${sha}" "${digest}"
grep -qF "buildx imagetools create --tag ${image}:pg17-v0.1.0" "${FAKE_DOCKER_LOG}"
grep -qF -- "${image}@${digest}" "${FAKE_DOCKER_LOG}"

for mode in FAKE_SHA_MISMATCH FAKE_PREPARED_MISMATCH FAKE_POST_MISMATCH; do
  if env "${mode}=1" PATH="${tmp}:${PATH}" scripts/promote-release-image.sh \
    "${image}" 17 "${tag}" "${sha}" "${digest}" >/dev/null 2>&1; then
    echo "tag promotion accepted ${mode}" >&2
    exit 1
  fi
done

: >"${FAKE_DOCKER_LOG}"
rm -f "${FAKE_DOCKER_STATE}"
if FAKE_IMMUTABLE_CONFLICT=1 PATH="${tmp}:${PATH}" scripts/promote-release-image.sh \
  "${image}" 17 "${tag}" "${sha}" "${digest}" >/dev/null 2>&1; then
  echo "tag promotion overwrote a conflicting immutable version tag" >&2
  exit 1
fi
if grep -qF 'buildx imagetools create' "${FAKE_DOCKER_LOG}"; then
  echo "tag promotion mutated the registry after an immutable-tag conflict" >&2
  exit 1
fi

rm -f "${FAKE_DOCKER_STATE}"
FAKE_IMMUTABLE_MISSING=1 PATH="${tmp}:${PATH}" scripts/promote-release-image.sh \
  "${image}" 17 "${tag}" "${sha}" "${digest}"

rm -f "${FAKE_DOCKER_STATE}" "${FAKE_TRANSIENT_STATE}"
FAKE_TRANSIENT_INSPECT=1 PGCONTEXT_PROMOTE_INSPECT_DELAY_SECONDS=0 \
  PATH="${tmp}:${PATH}" scripts/promote-release-image.sh \
  "${image}" 17 "${tag}" "${sha}" "${digest}"

rm -f "${FAKE_TRANSIENT_STATE}"
resolved="$(FAKE_TRANSIENT_INSPECT=1 PGCONTEXT_OCI_INSPECT_DELAY_SECONDS=0 \
  PATH="${tmp}:${PATH}" scripts/resolve-oci-digest.sh "${image}:${tag}")"
test "${resolved}" = "${digest}"
