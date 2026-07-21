#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"
workflow=.github/workflows/release.yml

job_block() {
  awk -v job="$1" '
    $0 == "  " job ":" { found = 1 }
    found && $0 ~ /^  [a-z0-9-]+:$/ && $0 != "  " job ":" { exit }
    found { print }
  ' "${workflow}"
}

grep -qF 'name: Prepare Or Publish Packages' "${workflow}"
grep -qF 'workflow_dispatch:' "${workflow}"
grep -qF 'group: pgcontext-release' "${workflow}"
grep -qF 'options:' "${workflow}"
grep -qF -- '- prepare' "${workflow}"
grep -qF -- '- publish' "${workflow}"
grep -qF 'candidate_sha:' "${workflow}"
grep -qF 'prepare_run_id:' "${workflow}"
grep -qF 'source_archive_sha256:' "${workflow}"
grep -qF 'scripts/validate-release.py --tag "${{ steps.release.outputs.tag }}" --check-master' "${workflow}"
grep -qF 'container: pgxn/pgxn-tools@sha256:' "${workflow}"
grep -qF 'Release tag signature is not verified by GitHub' "${workflow}"
grep -qF 'Verify GitHub Release is an empty draft' "${workflow}"

if grep -Eq 'pg(14|15|16|18)|postgresql-(14|15|16|18)' "${workflow}"; then
  echo "release workflow advertises an unsupported PostgreSQL major" >&2
  exit 1
fi
if grep -qF 'repository: Evokoa/homebrew-tap' "${workflow}"; then
  echo "PGXN and Docker workflow unexpectedly publishes Homebrew" >&2
  exit 1
fi
if grep -Eq 'uses: [^ ]+@(master|main|v[0-9]+|nightly)([[:space:]]|$)' "${workflow}"; then
  echo "release workflow contains a mutable action reference" >&2
  exit 1
fi
if grep -E '^[[:space:]]+uses:' "${workflow}" | grep -Ev '@[0-9a-f]{40}([[:space:]]|$)'; then
  echo "release workflow contains an action not pinned to a full commit SHA" >&2
  exit 1
fi

pgxn_artifact="$(job_block pgxn-artifact)"
source_attestation="$(job_block verify-source-attestation)"
approval="$(job_block approve-publishing)"
publish_pgxn="$(job_block publish-pgxn)"
pgxn_verify="$(job_block pgxn-verify)"
attach_pgxn="$(job_block attach-pgxn-artifact)"
docker_build="$(job_block docker)"
docker_merge="$(job_block docker-merge)"
docker_verify="$(job_block docker-verify)"
publish_docker="$(job_block publish-docker)"
published_verify="$(job_block publish-docker-verify)"
default_verify="$(job_block docker-verify-default)"
prepare_summary="$(job_block prepare-summary)"
publish_summary="$(job_block publish-summary)"

grep -qF "needs.validate.outputs.mode == 'prepare'" <<<"${pgxn_artifact}"
grep -qF 'release/build-packages.sh' <<<"${pgxn_artifact}"
grep -qF 'source-payload-${{ needs.validate.outputs.version }}' <<<"${pgxn_artifact}"
grep -qF 'actions/attest@' <<<"${pgxn_artifact}"
grep -qF 'subject-path: dist/pgContext-' <<<"${pgxn_artifact}"

grep -qF "needs.validate.outputs.mode == 'publish'" <<<"${source_attestation}"
grep -qF 'gh attestation verify' <<<"${source_attestation}"
grep -qF 'source_archive_sha256' <<<"${source_attestation}"
grep -qF 'prepare_run_id' <<<"${source_attestation}"

grep -qF "needs.validate.outputs.mode == 'publish'" <<<"${approval}"
grep -qF 'environment: release' <<<"${approval}"

grep -qF 'approve-publishing' <<<"${publish_pgxn}"
grep -qF 'PGXN_USERNAME' <<<"${publish_pgxn}"
grep -qF 'PGXN_PASSWORD' <<<"${publish_pgxn}"
grep -qF 'pgxn-release "dist/pgContext-' <<<"${publish_pgxn}"
grep -qF 'https://api.pgxn.org/dist/pgcontext/' <<<"${pgxn_verify}"

grep -qF 'pgxn-verify' <<<"${attach_pgxn}"
grep -qF 'contents: write' <<<"${attach_pgxn}"
grep -qF 'gh release upload' <<<"${attach_pgxn}"
grep -qF 'cmp "${asset}"' <<<"${attach_pgxn}"
if grep -qF -- '--clobber' <<<"${attach_pgxn}"; then
  echo "GitHub release publication can overwrite an immutable asset" >&2
  exit 1
fi

grep -qF "needs.validate.outputs.mode == 'prepare'" <<<"${docker_build}"
grep -qF 'linux/amd64' <<<"${docker_build}"
grep -qF 'linux/arm64' <<<"${docker_build}"
grep -qF 'ubuntu-24.04-arm' <<<"${docker_build}"
grep -qF 'file: release/docker/Dockerfile' <<<"${docker_build}"
grep -qF 'PG_MAJOR=17' <<<"${docker_build}"
grep -qF 'push-by-digest=true' <<<"${docker_build}"
grep -qF 'provenance: mode=max' <<<"${docker_build}"
grep -qF 'packages: write' <<<"${docker_build}"

grep -qF -- '- docker' <<<"${docker_merge}"
grep -qF 'pg17-sha-${{ needs.validate.outputs.short_sha }}' <<<"${docker_merge}"
grep -qF 'pg17-${{ needs.validate.outputs.tag }}-prepared' <<<"${docker_merge}"
grep -qF 'actions/attest@' <<<"${docker_merge}"
grep -qF 'push-to-registry: true' <<<"${docker_merge}"

grep -qF 'docker-merge' <<<"${docker_verify}"
grep -qF 'scripts/verify-release-image.sh --registry' <<<"${docker_verify}"
grep -qF 'gh attestation verify "oci://' <<<"${docker_verify}"
grep -qF 'linux/amd64' <<<"${docker_verify}"
grep -qF 'linux/arm64' <<<"${docker_verify}"

grep -qF 'approve-publishing' <<<"${publish_docker}"
grep -qF 'scripts/promote-release-image.sh' <<<"${publish_docker}"
grep -qF 'pg17-sha-${SHORT_SHA}' <<<"${publish_docker}"
grep -qF 'pg17-${TAG}-prepared' <<<"${publish_docker}"

for block in "${published_verify}" "${default_verify}"; do
  grep -qF 'publish-docker' <<<"${block}"
  grep -qF 'scripts/verify-release-image.sh --registry' <<<"${block}"
  grep -qF 'linux/amd64' <<<"${block}"
  grep -qF 'linux/arm64' <<<"${block}"
done

grep -qF 'pgxn-artifact' <<<"${prepare_summary}"
grep -qF 'docker-verify' <<<"${prepare_summary}"
grep -qF 'pgxn-verify' <<<"${publish_summary}"
grep -qF 'attach-pgxn-artifact' <<<"${publish_summary}"
grep -qF 'publish-docker-verify' <<<"${publish_summary}"
grep -qF 'docker-verify-default' <<<"${publish_summary}"
