#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${ROOT}"

bash -n \
  release/build-packages.sh \
  release/checks/open-source-readiness.sh \
  scripts/build-pgxn-dist.sh \
  scripts/build-release-image.sh \
  scripts/promote-release-image.sh \
  scripts/check-repository-contract.sh \
  scripts/run-install-report.sh \
  scripts/render-homebrew-formula.sh \
  scripts/verify-release-image.sh \
  scripts/quickstart.sh \
  tests/shell/build_packages_smoke.sh \
  tests/shell/build_packages_negative_smoke.sh \
  tests/shell/check_public_docs_smoke.sh \
  tests/shell/gitleaks_config_smoke.sh \
  tests/shell/homebrew_formula_smoke.sh \
  tests/shell/pgxn_dist_smoke.sh \
  tests/shell/release_image_contract_smoke.sh \
  tests/shell/release_image_tags_smoke.sh \
  tests/shell/release_workflow_smoke.sh \
  tests/shell/run_install_report_smoke.sh \
  tests/shell/validate_release_smoke.sh

python3 tests/shell/verify_oci_image_test.py
tests/shell/release_workflow_smoke.sh

release/build-packages.sh --help | grep -qF 'complete unsigned V1 source release payload'
scripts/verify-release-payload.py --help | grep -qF -- '--candidate-sha'
scripts/check-public-docs.py --check
scripts/render-release-notes.py --help | grep -qF -- '--candidate-sha'
release/checks/open-source-readiness.sh --help | grep -qF -- '--allow-dirty'
scripts/validate-release.py --help | grep -qF -- '--tag'
scripts/build-pgxn-dist.sh --help | grep -qF 'pgContext-X.Y.Z.zip'
scripts/verify-pgxn-dist.py --help | grep -qF -- '--tag'
scripts/render-homebrew-formula.sh --help | grep -qF 'Evokoa/tap'

if scripts/quickstart.sh invalid-mode 2>/dev/null; then
  echo "quickstart accepted an invalid mode" >&2
  exit 1
fi

jq -e '
  .name == "pgContext" and
  .version == "0.1.0" and
  .license == "apache_2_0" and
  .provides.pgcontext.version == "0.1.0"
' META.json >/dev/null

grep -qF 'cargo pgrx package -p context-pg' release/docker/Dockerfile
grep -qE '^[[:space:]]+context: \.$' release/docker/compose.yml
if grep -qF '::jsonb' playground/demo.sql; then
  echo "playground passes a jsonb filter to the text-filter search overload" >&2
  exit 1
fi
grep -qF 'CREATE EXTENSION IF NOT EXISTS pgcontext;' \
  release/docker/init/01-pgcontext.sql
grep -qF 'pgcontext.vector_hnsw_cosine_ops' playground/demo.sql
grep -qF 'SELECT pgcontext.drop_collection' docs/user_guide/quickstart.md
grep -qF 'DROP EXTENSION pgcontext;' docs/user_guide/quickstart.md
grep -qF 'CARGO_AUDIT_VERSION=' release/tool-versions.env
grep -qF 'CARGO_DENY_VERSION=' release/tool-versions.env
grep -qF 'CARGO_PGRX_VERSION=' release/tool-versions.env
grep -qF 'GITLEAKS_VERSION=' release/tool-versions.env
grep -qF 'release/checks/open-source-readiness.sh' release/README.md
grep -qF 'scripts/generate-sql-object-inventory.sh --check' \
  docs/user_guide/sql_object_inventory.md

for workflow in .github/workflows/ci.yml .github/workflows/release-gates.yml; do
  if grep -Eq 'cargo install (cargo-audit|cargo-deny)( --locked)?$' "${workflow}"; then
    echo "workflow installs an unpinned release tool: ${workflow}" >&2
    exit 1
  fi
done

if command -v docker >/dev/null 2>&1; then
  docker compose -f release/docker/compose.yml config --quiet
fi
