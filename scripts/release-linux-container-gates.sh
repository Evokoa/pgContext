#!/usr/bin/env bash
set -euo pipefail

PG_MAJOR="${PG_MAJOR:-17}"
RUST_VERSION="${RUST_VERSION:-1.96.0}"
IMAGE_TAG="pgcontext-release-gates:pg${PG_MAJOR}-rust${RUST_VERSION}"

docker build \
  --file docker/release-gates.Dockerfile \
  --build-arg "PG_MAJOR=${PG_MAJOR}" \
  --build-arg "RUST_VERSION=${RUST_VERSION}" \
  --tag "${IMAGE_TAG}" \
  .

docker run --rm \
  --volume "${PWD}:/workspace" \
  --workdir /workspace \
  --env "PG_MAJOR=${PG_MAJOR}" \
  "${IMAGE_TAG}"
