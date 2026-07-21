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

grep -qF 'workflow_dispatch:' "${workflow}"
grep -qF 'group: pgcontext-release' "${workflow}"
grep -qF 'candidate_sha:' "${workflow}"
grep -qF 'prepared_digest:' "${workflow}"
grep -qF 'prepare_run_id:' "${workflow}"
grep -qF 'container: pgxn/pgxn-tools@sha256:' "${workflow}"
grep -qF 'repository: Evokoa/homebrew-tap' "${workflow}"
grep -qF 'scripts/promote-release-image.sh' "${workflow}"
grep -qF 'scripts/build-release-image.sh' "${workflow}"
grep -qF 'release/build-packages.sh --out-dir dist/payload' "${workflow}"

if grep -Eq 'pg(14|15|16|18)|postgresql-(14|15|16|18)' "${workflow}"; then
  echo "release workflow advertises an unsupported PostgreSQL major" >&2
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

prepare_image="$(job_block prepare-image)"
preflight="$(job_block publish-preflight)"
publish_image="$(job_block publish-image)"
pgxn_status="$(job_block pgxn-status)"
publish_pgxn="$(job_block publish-pgxn)"
publish_github="$(job_block publish-github)"
publish_homebrew="$(job_block publish-homebrew)"

grep -qF "needs.validate.outputs.mode == 'prepare'" <<<"${prepare_image}"
grep -qF 'linux/amd64' <<<"${prepare_image}"
grep -qF 'linux/arm64' <<<"${prepare_image}"
grep -qF 'scripts/verify-release-image.sh' <<<"${prepare_image}"
if grep -Eq 'packages: write|docker/login-action|skopeo copy|push=true' <<<"${prepare_image}"; then
  echo "prepare mode can mutate GHCR" >&2
  exit 1
fi

grep -qF 'environment: release' <<<"${preflight}"
grep -qF 'actions: read' <<<"${preflight}"
grep -qF 'gh run download' <<<"${preflight}"
grep -qF 'needs: [validate, publish-preflight]' <<<"${publish_image}"
grep -qF 'packages: write' <<<"${publish_image}"
grep -qF 'refusing to overwrite immutable tag' <<<"${publish_image}"
grep -qF 'quay.io/skopeo/stable@sha256:' <<<"${publish_image}"
grep -qF '"${skopeo_image}" copy --all --preserve-digests' <<<"${publish_image}"
grep -qF 'scripts/verify-release-image.sh --registry "${digest_ref}" linux/amd64' <<<"${publish_image}"
grep -qF 'scripts/verify-release-image.sh --registry "${digest_ref}" linux/arm64' <<<"${publish_image}"

grep -qF 'needs: [validate, publish-image]' <<<"${pgxn_status}"
grep -qF 'remote_sha1=' <<<"${pgxn_status}"
grep -qF 'existing PGXN archive conflicts' <<<"${pgxn_status}"
grep -qF 'needs: [validate, publish-image, pgxn-status]' <<<"${publish_pgxn}"
if grep -qF '[[' <<<"${publish_pgxn}"; then
  echo "PGXN container job uses Bash-only conditionals under its default shell" >&2
  exit 1
fi
grep -qF 'needs: [validate, publish-pgxn]' <<<"${publish_github}"
grep -qF 'needs: [validate, publish-github]' <<<"${publish_homebrew}"
grep -qF 'gh release create' <<<"${publish_github}"
grep -qF 'for asset in accepted/source/payload/*' <<<"${publish_github}"
grep -qF 'cmp "${asset}"' <<<"${publish_github}"
for block in "${publish_image}" "${publish_pgxn}" "${publish_github}" "${publish_homebrew}"; do
  grep -qF 'environment: release' <<<"${block}"
done
grep -qF 'scripts/render-homebrew-formula.sh' <<<"${preflight}"
grep -qF 'scripts/verify-release-payload.py' <<<"${preflight}"
grep -qF 'scripts/render-release-notes.py' <<<"${publish_github}"
grep -qF -- '--notes-file target/release-notes.md' <<<"${publish_github}"
grep -qF 'cmp target/release-notes.md target/existing-release-notes.md' <<<"${publish_github}"
grep -qF 'cmp target/preflight-homebrew/pgcontext.rb' <<<"${preflight}"
if grep -qF 'source "${record}"' <<<"${preflight}"; then
  echo "protected preflight executes a downloaded record as shell code" >&2
  exit 1
fi
if grep -qF -- '--clobber' <<<"${publish_github}"; then
  echo "GitHub release publication can overwrite an immutable asset" >&2
  exit 1
fi
