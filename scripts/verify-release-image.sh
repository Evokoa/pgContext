#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE_MODE=archive
if [[ "${1:-}" == "--registry" ]]; then
  [[ $# -eq 3 ]] || {
    echo "usage: scripts/verify-release-image.sh --registry IMAGE@DIGEST PLATFORM" >&2
    exit 2
  }
  SOURCE_MODE=registry
  OCI_ARCHIVE=""
  IMAGE="$2"
  PLATFORM="$3"
  [[ "${IMAGE}" =~ @sha256:[0-9a-f]{64}$ ]] || {
    echo "registry verification requires an immutable IMAGE@sha256:DIGEST reference" >&2
    exit 2
  }
else
  [[ $# -eq 3 ]] || {
    echo "usage: scripts/verify-release-image.sh OCI_ARCHIVE IMAGE PLATFORM" >&2
    exit 2
  }
  OCI_ARCHIVE="$1"
  IMAGE="$2"
  PLATFORM="$3"
fi
case "${PLATFORM}" in
  linux/amd64 | linux/arm64) ;;
  *) echo "PLATFORM must be linux/amd64 or linux/arm64" >&2; exit 2 ;;
esac
WAIT_ATTEMPTS="${PGCONTEXT_VERIFY_WAIT_ATTEMPTS:-90}"
[[ "${WAIT_ATTEMPTS}" =~ ^[1-9][0-9]*$ ]] || {
  echo "PGCONTEXT_VERIFY_WAIT_ATTEMPTS must be a positive integer" >&2
  exit 2
}

die() {
  echo "release image verification failed: $*" >&2
  exit 1
}

name="pgcontext-release-${PLATFORM##*/}-$$"
cleanup() {
  docker rm -f "${name}" >/dev/null 2>&1 || true
  docker image rm -f "${IMAGE}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker image rm -f "${IMAGE}" >/dev/null 2>&1 || true
if [[ "${SOURCE_MODE}" == archive ]]; then
  load_output="$(docker load --input "${OCI_ARCHIVE}")"
  printf '%s\n' "${load_output}"
  grep -Fxq "Loaded image: ${IMAGE}" <<<"${load_output}" || \
    die "loaded archive did not provide ${IMAGE}"
else
  docker pull --platform "${PLATFORM}" "${IMAGE}"
fi
docker run --detach --pull=never --platform "${PLATFORM}" --name "${name}" \
  -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=pgcontext "${IMAGE}"
for _ in $(seq 1 "${WAIT_ATTEMPTS}"); do
  # The official image starts a temporary PostgreSQL server while it runs
  # init scripts. Its healthcheck can pass just before that server shuts down.
  # PID 1 becomes postgres only after the entrypoint reaches the final server.
  if docker exec "${name}" sh -c '[ "$(cat /proc/1/comm)" = postgres ]' \
      >/dev/null 2>&1 \
    && [[ "$(docker inspect --format '{{if .State.Health}}{{.State.Health.Status}}{{end}}' "${name}")" == "healthy" ]]; then
    break
  fi
  sleep 1
done
docker exec "${name}" sh -c '[ "$(cat /proc/1/comm)" = postgres ]' \
  >/dev/null 2>&1 || die "container did not reach its final PostgreSQL process"
health="$(docker inspect --format '{{.State.Health.Status}}' "${name}")"
[[ "${health}" == "healthy" ]] || die "container health is ${health}"
server_version="$(docker exec "${name}" psql -U postgres -d pgcontext -Atc 'SHOW server_version_num')"
[[ "${server_version}" == 17* ]] || die "server is not PostgreSQL 17: ${server_version}"
if ! docker exec -i "${name}" psql -U postgres -d pgcontext -v ON_ERROR_STOP=1 \
  <"${ROOT}/playground/demo.sql"; then
  die "packaged demo failed"
fi

filtered="$({ docker exec "${name}" psql -U postgres -d pgcontext -Atv ON_ERROR_STOP=1 -c \
  "SELECT string_agg(source_key, ',' ORDER BY score) FROM pgcontext.search('playground_docs', '[1,0,0]'::pgcontext.vector, '{\"must\":[{\"key\":\"category\",\"match\":\"database\"}]}', 4);"; } 2>/dev/null)"
[[ "${filtered}" == "postgres,vectors" ]] || \
  die "metadata-filter result mismatch: ${filtered}"

ordered="$({ docker exec "${name}" psql -U postgres -d pgcontext -Atv ON_ERROR_STOP=1 -c \
  "SET enable_seqscan=off; SELECT string_agg(id, ',' ORDER BY distance) FROM (SELECT id, embedding OPERATOR(pgcontext.<=>) '[1,0,0]'::pgcontext.vector AS distance FROM public.pgcontext_playground_docs ORDER BY embedding OPERATOR(pgcontext.<=>) '[1,0,0]'::pgcontext.vector LIMIT 3) ranked;"; } 2>/dev/null)"
[[ "${ordered##*$'\n'}" == "postgres,rust,vectors" ]] || \
  die "HNSW ordering mismatch: ${ordered##*$'\n'}"

plan="$({ docker exec "${name}" psql -U postgres -d pgcontext -Atv ON_ERROR_STOP=1 -c \
  "SET enable_seqscan=off; EXPLAIN (COSTS OFF) SELECT id FROM public.pgcontext_playground_docs ORDER BY embedding OPERATOR(pgcontext.<=>) '[1,0,0]'::pgcontext.vector LIMIT 3;"; } 2>/dev/null)"
grep -qF 'Index Scan using pgcontext_playground_docs_hnsw' <<<"${plan}" || \
  die "query plan did not use the packaged HNSW index"
