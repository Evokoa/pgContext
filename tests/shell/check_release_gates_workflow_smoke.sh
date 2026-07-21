#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
workflow="${REPO_ROOT}/.github/workflows/release-gates.yml"

job_block() {
  local job_name="$1"
  awk -v job="${job_name}" '
    $0 ~ "^  " job ":" { in_job = 1; print; next }
    in_job && $0 ~ /^  [A-Za-z0-9_-]+:/ { exit }
    in_job { print }
  ' "${workflow}"
}

assert_workflow_contains() {
  local needle="$1"
  if ! grep -qF -- "${needle}" "${workflow}"; then
    echo "release-gates workflow is missing: ${needle}" >&2
    exit 1
  fi
}

assert_block_contains() {
  local block="$1"
  local needle="$2"
  if ! printf '%s\n' "${block}" | grep -qF -- "${needle}"; then
    echo "release-gates workflow block is missing: ${needle}" >&2
    exit 1
  fi
}

assert_step_contains() {
  local block="$1"
  local step_name="$2"
  local needle="$3"

  if ! printf '%s\n' "${block}" | awk -v step="${step_name}" '
    $0 == "      - name: " step { in_step = 1; print; next }
    in_step && $0 ~ /^      - name: / { exit }
    in_step { print }
  ' | grep -qF -- "${needle}"; then
    echo "release-gates workflow step ${step_name} is missing: ${needle}" >&2
    exit 1
  fi
}

assert_block_present() {
  local block_name="$1"
  local block="$2"
  if [[ -z "${block}" ]]; then
    echo "release-gates workflow is missing job block: ${block_name}" >&2
    exit 1
  fi
}

assert_postgres_matrix_entry() {
  local pg="$1"
  local feature="$2"
  if ! printf '%s\n' "${supported_postgres_block}" | awk -v pg="${pg}" -v feature="${feature}" '
    $0 == "          - pg: \"" pg "\"" {
      getline next_line
      if (next_line == "            feature: " feature) {
        found = 1
      }
    }
    END { exit found ? 0 : 1 }
  '; then
    echo "release-gates workflow is missing PostgreSQL ${pg} matrix feature ${feature}" >&2
    exit 1
  fi
}

assert_postgres_matrix_scope() {
  local actual
  local expected

  actual="$(printf '%s\n' "${supported_postgres_block}" | awk '
    /^        include:$/ { in_include = 1; next }
    in_include && /^    steps:$/ { exit }
    in_include { print }
  ')"
  expected='          - pg: "15"
            feature: pg15
          - pg: "16"
            feature: pg16
          - pg: "17"
            feature: pg17
          - pg: "18"
            feature: pg18'
  if [[ "${actual}" != "${expected}" ]]; then
    echo "release-gates workflow PostgreSQL matrix must contain exactly pg15, pg16, pg17, and pg18 rows" >&2
    exit 1
  fi
}

assert_artifact_report_command_contains() {
  local block="$1"
  local needle="$2"
  if ! printf '%s\n' "${block}" | awk '
    /scripts\/run-release-artifact-report\.sh \\/ { in_command = 1 }
    in_command {
      print
      if ($0 !~ /\\$/) {
        in_command = 0
      }
    }
  ' | grep -qF -- "${needle}"; then
    echo "release-gates artifact report command is missing: ${needle}" >&2
    exit 1
  fi
}

supported_postgres_block="$(job_block "supported-postgres")"
release_artifact_summary_block="$(job_block "release-artifact-summary")"
postgres_matrix_summary_block="$(job_block "postgres-matrix-summary")"
platform_builds_block="$(job_block "platform-builds")"
platform_build_summary_block="$(job_block "platform-build-summary")"
fuzz_release_campaign_block="$(job_block "fuzz-release-campaign")"

assert_block_present "supported-postgres" "${supported_postgres_block}"
assert_block_present "release-artifact-summary" "${release_artifact_summary_block}"
assert_block_present "postgres-matrix-summary" "${postgres_matrix_summary_block}"
assert_block_present "platform-builds" "${platform_builds_block}"
assert_block_present "platform-build-summary" "${platform_build_summary_block}"
assert_block_present "fuzz-release-campaign" "${fuzz_release_campaign_block}"

assert_workflow_contains "name: Release Gates"
assert_workflow_contains "run_fuzz_campaign:"
assert_workflow_contains "fuzz_duration_seconds:"
assert_workflow_contains "fuzz_jobs:"
assert_postgres_matrix_entry "15" "pg15"
assert_postgres_matrix_entry "16" "pg16"
assert_postgres_matrix_entry "17" "pg17"
assert_postgres_matrix_entry "18" "pg18"
assert_postgres_matrix_scope
assert_block_contains "${supported_postgres_block}" "cargo check -p context-pg --no-default-features --features \${{ matrix.feature }}"
assert_block_contains "${supported_postgres_block}" "run: scripts/run-v1-pgrx-tests.sh"
assert_block_contains "${supported_postgres_block}" "scripts/run-postgres-matrix-gates.sh --major \${{ matrix.pg }} --mode heavy --out-dir target/postgres-matrix/pg\${{ matrix.pg }}-heavy"
assert_block_contains "${supported_postgres_block}" "name: postgres-\${{ matrix.pg }}-release-sql"
assert_block_contains "${supported_postgres_block}" "target/release-sql/pg\${{ matrix.pg }}.sql"
assert_block_contains "${supported_postgres_block}" "target/release-sql/pg\${{ matrix.pg }}.sql.build.log"
assert_block_contains "${supported_postgres_block}" "name: Record release artifact report"
assert_block_contains "${supported_postgres_block}" "scripts/run-release-artifact-report.sh \\"
assert_block_contains "${supported_postgres_block}" "--artifact target/release-sql/pg\${{ matrix.pg }}.sql"
assert_block_contains "${supported_postgres_block}" "--out-dir target/release-artifacts/pg\${{ matrix.pg }}"
assert_step_contains "${supported_postgres_block}" "Upload release artifact report" "uses: actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02"
assert_step_contains "${supported_postgres_block}" "Upload release artifact report" "name: postgres-\${{ matrix.pg }}-release-artifact-report"
assert_step_contains "${supported_postgres_block}" "Upload release artifact report" "path: target/release-artifacts/pg\${{ matrix.pg }}"
assert_block_contains "${release_artifact_summary_block}" "release-artifact-summary:"
assert_block_contains "${release_artifact_summary_block}" "needs: supported-postgres"
assert_block_contains "${release_artifact_summary_block}" "if: \${{ always() }}"
assert_block_contains "${release_artifact_summary_block}" "uses: actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093"
assert_block_contains "${release_artifact_summary_block}" "pattern: postgres-*-release-sql"
assert_block_contains "${release_artifact_summary_block}" "merge-multiple: true"
assert_block_contains "${release_artifact_summary_block}" "--out-dir target/release-artifacts/all-postgres"
assert_block_contains "${release_artifact_summary_block}" "name: combined-release-artifact-report"
assert_block_contains "${release_artifact_summary_block}" "path: target/release-artifacts/all-postgres"
assert_block_contains "${postgres_matrix_summary_block}" "postgres-matrix-summary:"
assert_block_contains "${postgres_matrix_summary_block}" "name: Combined PostgreSQL matrix evidence"
assert_block_contains "${postgres_matrix_summary_block}" "components: rustfmt, clippy"
assert_block_contains "${postgres_matrix_summary_block}" "postgresql-15 postgresql-server-dev-15"
assert_block_contains "${postgres_matrix_summary_block}" "postgresql-16 postgresql-server-dev-16"
assert_block_contains "${postgres_matrix_summary_block}" "postgresql-17 postgresql-server-dev-17"
assert_block_contains "${postgres_matrix_summary_block}" "postgresql-18 postgresql-server-dev-18"
assert_block_contains "${postgres_matrix_summary_block}" 'source release/tool-versions.env'
assert_block_contains "${postgres_matrix_summary_block}" 'cargo install cargo-pgrx --version "${CARGO_PGRX_VERSION}" --locked'
assert_block_contains "${postgres_matrix_summary_block}" "scripts/run-postgres-matrix-gates.sh \\"
assert_block_contains "${postgres_matrix_summary_block}" "--mode all"
assert_block_contains "${postgres_matrix_summary_block}" "--out-dir target/postgres-matrix/all-postgres"
assert_block_contains "${postgres_matrix_summary_block}" "name: combined-postgres-matrix-report"
assert_block_contains "${postgres_matrix_summary_block}" "path: target/postgres-matrix/all-postgres"
assert_block_contains "${platform_builds_block}" "run: tests/shell/check_release_gates_workflow_smoke.sh"
assert_block_contains "${platform_builds_block}" "run: tests/shell/upgrade_matrix_staging_smoke.sh"
assert_block_contains "${platform_build_summary_block}" "platform-build-summary:"
assert_block_contains "${platform_build_summary_block}" "name: Combined platform build evidence"
assert_block_contains "${platform_build_summary_block}" "needs: platform-builds"
assert_block_contains "${platform_build_summary_block}" "if: \${{ always() }}"
assert_block_contains "${platform_build_summary_block}" "uses: actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093"
assert_block_contains "${platform_build_summary_block}" "pattern: platform-build-report-*"
assert_block_contains "${platform_build_summary_block}" "path: target/platform-builds"
assert_block_contains "${platform_build_summary_block}" "merge-multiple: true"
assert_block_contains "${platform_build_summary_block}" "scripts/run-platform-build-report.sh \\"
assert_block_contains "${platform_build_summary_block}" "--merge-report target/platform-builds/linux/report.md"
assert_block_contains "${platform_build_summary_block}" "--merge-report target/platform-builds/macos/report.md"
assert_block_contains "${platform_build_summary_block}" "--out-dir target/platform-builds/release-candidate"
assert_block_contains "${platform_build_summary_block}" "name: combined-platform-build-report"
assert_block_contains "${platform_build_summary_block}" "path: target/platform-builds/release-candidate"
assert_block_contains "${fuzz_release_campaign_block}" "fuzz-release-campaign:"
assert_block_contains "${fuzz_release_campaign_block}" "name: Release fuzz campaign"
assert_block_contains "${fuzz_release_campaign_block}" "if: \${{ github.event_name == 'workflow_dispatch' && inputs.run_fuzz_campaign == 'true' }}"
assert_block_contains "${fuzz_release_campaign_block}" "uses: dtolnay/rust-toolchain@fa04a1451ff1842e2626ccb99004d0195b455a88"
assert_block_contains "${fuzz_release_campaign_block}" "toolchain: nightly"
assert_block_contains "${fuzz_release_campaign_block}" "cargo +nightly install cargo-fuzz --locked"
assert_block_contains "${fuzz_release_campaign_block}" "scripts/run-fuzz-campaigns.sh \\"
assert_block_contains "${fuzz_release_campaign_block}" "--duration \"\${{ inputs.fuzz_duration_seconds }}\""
assert_block_contains "${fuzz_release_campaign_block}" "--jobs \"\${{ inputs.fuzz_jobs }}\""
assert_block_contains "${fuzz_release_campaign_block}" "--out-dir target/fuzz-campaigns/release-candidate"
assert_block_contains "${fuzz_release_campaign_block}" "name: release-fuzz-campaign-report"
assert_block_contains "${fuzz_release_campaign_block}" "path: target/fuzz-campaigns/release-candidate"

assert_artifact_report_command_contains "${release_artifact_summary_block}" "--artifact target/release-sql/pg15.sql"
assert_artifact_report_command_contains "${release_artifact_summary_block}" "--artifact target/release-sql/pg16.sql"
assert_artifact_report_command_contains "${release_artifact_summary_block}" "--artifact target/release-sql/pg17.sql"
assert_artifact_report_command_contains "${release_artifact_summary_block}" "--artifact target/release-sql/pg18.sql"

if grep -qE '^  schedule:|uses: [^ ]+@(master|main|v[0-9]+|nightly)([[:space:]]|$)' "${workflow}"; then
  echo "release-gates workflow contains a schedule or mutable action reference" >&2
  exit 1
fi
