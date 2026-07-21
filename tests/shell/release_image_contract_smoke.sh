#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"

bash -n scripts/build-release-image.sh scripts/verify-release-image.sh
scripts/build-release-image.sh --help | grep -qF 'amd64+arm64 OCI image index'
scripts/verify-oci-image.py --help | grep -qF -- '--image'
python3 tests/shell/verify_oci_image_test.py

grep -qF 'postgres:17-bookworm@sha256:' release/docker/Dockerfile
grep -qF 'org.opencontainers.image.version="${VERSION}"' release/docker/Dockerfile
grep -qF 'org.opencontainers.image.revision="${REVISION}"' release/docker/Dockerfile
grep -qF 'org.opencontainers.image.postgresql.major="${PG_MAJOR}"' release/docker/Dockerfile
grep -qF -- '--platform linux/amd64,linux/arm64' scripts/build-release-image.sh
grep -qF -- '--provenance mode=max' scripts/build-release-image.sh
grep -qF 'ARTIFACT_REVISION="${REVISION}-dirty"' scripts/build-release-image.sh
grep -qF 'DIRTY_SUFFIX="-dirty"' scripts/build-release-image.sh

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT
cp tests/shell/fixtures/release_image_fake_docker.sh "${tmp}/docker"
chmod +x "${tmp}/docker"
touch "${tmp}/candidate.oci.tar"
export FAKE_DOCKER_LOG="${tmp}/docker.log"
export FAKE_IMAGE='ghcr.io/evokoa/pgcontext:pg17-v0.1.0-prepared'

for platform in linux/amd64 linux/arm64; do
  PATH="${tmp}:${PATH}" scripts/verify-release-image.sh \
    "${tmp}/candidate.oci.tar" "${FAKE_IMAGE}" "${platform}" >/dev/null
done
registry_digest="sha256:$(printf 'b%.0s' {1..64})"
PATH="${tmp}:${PATH}" scripts/verify-release-image.sh --registry \
  "ghcr.io/evokoa/pgcontext@${registry_digest}" linux/amd64 >/dev/null
grep -qF "pull --platform linux/amd64 ghcr.io/evokoa/pgcontext@${registry_digest}" \
  "${FAKE_DOCKER_LOG}"
grep -qF 'run --detach --pull=never --platform linux/amd64' "${FAKE_DOCKER_LOG}"
grep -qF 'run --detach --pull=never --platform linux/arm64' "${FAKE_DOCKER_LOG}"
grep -qF 'image rm -f ghcr.io/evokoa/pgcontext:pg17-v0.1.0-prepared' "${FAKE_DOCKER_LOG}"
grep -qF 'rm -f pgcontext-release-' "${FAKE_DOCKER_LOG}"

if FAKE_LOAD_WRONG=1 PATH="${tmp}:${PATH}" scripts/verify-release-image.sh \
  "${tmp}/candidate.oci.tar" "${FAKE_IMAGE}" linux/amd64 >/dev/null 2>&1; then
  echo "runtime verification accepted a mismatched loaded image" >&2
  exit 1
fi

for mode in FAKE_FILTER_WRONG FAKE_ORDER_WRONG FAKE_PLAN_WRONG; do
  if env "${mode}=1" PATH="${tmp}:${PATH}" scripts/verify-release-image.sh \
    "${tmp}/candidate.oci.tar" "${FAKE_IMAGE}" linux/amd64 >/dev/null 2>&1; then
    echo "runtime verification accepted ${mode}" >&2
    exit 1
  fi
done

if FAKE_NOT_FINAL=1 PGCONTEXT_VERIFY_WAIT_ATTEMPTS=1 \
  PATH="${tmp}:${PATH}" scripts/verify-release-image.sh \
  "${tmp}/candidate.oci.tar" "${FAKE_IMAGE}" linux/amd64 >/dev/null 2>&1; then
  echo "runtime verification accepted the temporary initialization server" >&2
  exit 1
fi
grep -qF '/proc/1/comm' "${FAKE_DOCKER_LOG}"
