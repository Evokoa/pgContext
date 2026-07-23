#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
fixture_tmp="${REPO_ROOT}/target/tmp"
mkdir -p "${fixture_tmp}"
work_dir="$(mktemp -d "${fixture_tmp}/release-artifact-report-test.XXXXXX")"
dirty_marker="${REPO_ROOT}/.release-artifact-report-smoke-dirty.$$"
trap 'rm -rf "${work_dir}"; rm -f "${dirty_marker}"' EXIT
if [[ -e "${dirty_marker}" ]]; then
  echo "dirty marker already exists: ${dirty_marker}" >&2
  exit 1
fi
fake_bin="${work_dir}/bin"
mkdir -p "${fake_bin}"
write_matching_fake_cargo() {
  cat >"${fake_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "-V" ]]; then
  printf 'cargo 1.96.0\n'
  exit 0
fi
if [[ "${1:-}" == "pgrx" && "${2:-}" == "--version" ]]; then
  printf 'cargo-pgrx 0.19.1\n'
  exit 0
fi
exit 127
SH
  chmod +x "${fake_bin}/cargo"
}
write_matching_fake_cargo
cat >"${fake_bin}/gpg" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

signature="${3:-}"
artifact="${4:-}"
expected="verified signature for $(shasum -a 256 "${artifact}" | awk '{print $1}')"
if [[ "${1:-}" == "--batch" && "${2:-}" == "--verify" ]] &&
  grep -qxF "${expected}" "${signature}"; then
  exit 0
fi
exit 1
SH
chmod +x "${fake_bin}/gpg"
export PATH="${fake_bin}:${PATH}"

assert_file_exists() {
  local path="$1"
  if [[ ! -f "${path}" ]]; then
    echo "expected file to exist: ${path}" >&2
    exit 1
  fi
}

assert_summary_row_count() {
  local summary_path="$1"
  local expected_rows="$2"
  local actual_rows

  actual_rows="$(tail -n +2 "${summary_path}" | wc -l | tr -d ' ')"
  if [[ "${actual_rows}" != "${expected_rows}" ]]; then
    echo "expected ${expected_rows} summary rows in ${summary_path}, got ${actual_rows}" >&2
    exit 1
  fi
}

write_verified_signature() {
  local artifact="$1"
  printf 'verified signature for %s\n' "$(shasum -a 256 "${artifact}" | awk '{print $1}')" >"${artifact}.asc"
}

write_generation_log() {
  local root="$1"
  local major="$2"
  local artifact="target/release-sql/pg${major}.sql"
  local absolute="${root}/${artifact}"

  {
    printf 'command: cargo pgrx schema -p context-pg pg%s --out %s\n' "${major}" "${artifact}"
    printf 'commit: %s\n' "$(git -C "${root}" rev-parse --verify HEAD)"
    printf 'artifact: %s\n' "${artifact}"
    printf 'sha256: %s\n' "$(shasum -a 256 "${absolute}" | awk '{print $1}')"
  } >"${absolute}.build.log"
}

artifact_a="${work_dir}/pgcontext--0.1.0.sql"
artifact_b="${work_dir}/pgcontext-control.tar.gz"
printf 'CREATE EXTENSION pgcontext;\n' >"${artifact_a}"
printf 'control archive\n' >"${artifact_b}"
write_verified_signature "${artifact_a}"

"${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --dry-run \
  --artifact "${artifact_a}" \
  --artifact "${artifact_b}" \
  --out-dir "${work_dir}/dry-run"

summary="${work_dir}/dry-run/summary.tsv"
report="${work_dir}/dry-run/report.md"
assert_file_exists "${summary}"
assert_file_exists "${report}"
head -n 1 "${summary}" | grep -q $'artifact\tstatus\tsize_bytes\tsha256\tsignature_status\tsignature\tsignature_size_bytes\tsignature_sha256\tlog'
assert_summary_row_count "${summary}" "2"
grep -q $'\tdry-run\t0\tnot-run\tunsigned\t' "${summary}"
grep -q -- '- Execution: `dry-run`' "${report}"
grep -q -- '- Approval: `incomplete`' "${report}"

printf 'dirty\n' >"${dirty_marker}"
if "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --artifact "${artifact_a}" \
  --out-dir "${work_dir}/dirty" 2>"${work_dir}/dirty.err"; then
  rm -f "${dirty_marker}"
  echo "dirty execute run without override should fail" >&2
  exit 1
fi
rm -f "${dirty_marker}"
grep -q 'dirty worktree cannot produce release artifact evidence' "${work_dir}/dirty.err"

"${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --allow-dirty \
  --artifact "${artifact_a}" \
  --artifact "${artifact_b}" \
  --out-dir "${work_dir}/execute"

summary="${work_dir}/execute/summary.tsv"
report="${work_dir}/execute/report.md"
grep -q "$(basename "${artifact_a}")" "${summary}"
grep -q $'\tpassed\t' "${summary}"
grep -q $'\tsigned\t' "${summary}"
grep -q $'\tunsigned\t' "${summary}"
awk -F '\t' '$5 == "signed" && $7 > 0 && $8 ~ /^[0-9a-f]{64}$/ { found = 1 } END { exit(found ? 0 : 1) }' "${summary}"
grep -q -- '- Passed: `2`' "${report}"
grep -q -- '- Unsigned: `1`' "${report}"
grep -q -- '- Dirty override: `1`' "${report}"
grep -q -- '- Approval: `incomplete`' "${report}"
grep -Eq '[0-9a-f]{64}' "${summary}"

if "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --allow-dirty \
  --require-signatures \
  --artifact "${artifact_b}" \
  --out-dir "${work_dir}/unsigned-required"; then
  echo "unsigned but present artifact should fail when signatures are required" >&2
  exit 1
fi
grep -q -- '- Require signatures: `1`' "${work_dir}/unsigned-required/report.md"
grep -q -- '- Unsigned: `1`' "${work_dir}/unsigned-required/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/unsigned-required/report.md"

invalid_signature_artifact="${work_dir}/invalid-signature.sql"
printf 'SQL with invalid signature\n' >"${invalid_signature_artifact}"
printf 'not a verified signature\n' >"${invalid_signature_artifact}.asc"
if "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --allow-dirty \
  --require-signatures \
  --artifact "${invalid_signature_artifact}" \
  --out-dir "${work_dir}/invalid-signature"; then
  echo "arbitrary non-empty signature should fail verification" >&2
  exit 1
fi
grep -q $'invalid-signature.sql\tfailed\t' "${work_dir}/invalid-signature/summary.tsv"
grep -q $'\tinvalid\t' "${work_dir}/invalid-signature/summary.tsv"

empty_signature_artifact="${work_dir}/empty-signature.sql"
printf 'SQL with empty signature\n' >"${empty_signature_artifact}"
: >"${empty_signature_artifact}.asc"
if "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --allow-dirty \
  --require-signatures \
  --artifact "${empty_signature_artifact}" \
  --out-dir "${work_dir}/empty-signature"; then
  echo "empty signature artifact should fail" >&2
  exit 1
fi
grep -q $'empty-signature.sql\tfailed\t' "${work_dir}/empty-signature/summary.tsv"
grep -q $'\tinvalid\t' "${work_dir}/empty-signature/summary.tsv"
grep -q -- '- Failed: `1`' "${work_dir}/empty-signature/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/empty-signature/report.md"
empty_signature_log="$(awk -F'\t' 'NR == 2 { print $9 }' "${work_dir}/empty-signature/summary.tsv")"
grep -q 'signature status: invalid' "${empty_signature_log}"
grep -q 'signature size bytes: 0' "${empty_signature_log}"

symlink_signature_artifact="${work_dir}/symlink-signature.sql"
symlink_signature_target="${work_dir}/symlink-signature.asc.real"
printf 'SQL with symlink signature\n' >"${symlink_signature_artifact}"
write_verified_signature "${symlink_signature_artifact}"
mv "${symlink_signature_artifact}.asc" "${symlink_signature_target}"
ln -s "${symlink_signature_target}" "${symlink_signature_artifact}.asc"
if "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --allow-dirty \
  --require-signatures \
  --artifact "${symlink_signature_artifact}" \
  --out-dir "${work_dir}/symlink-signature"; then
  echo "symlink signature artifact should fail" >&2
  exit 1
fi
grep -q $'symlink-signature.sql\tfailed\t' "${work_dir}/symlink-signature/summary.tsv"
grep -q $'\tinvalid\t' "${work_dir}/symlink-signature/summary.tsv"
grep -q -- '- Failed: `1`' "${work_dir}/symlink-signature/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/symlink-signature/report.md"
symlink_signature_log="$(awk -F'\t' 'NR == 2 { print $9 }' "${work_dir}/symlink-signature/summary.tsv")"
grep -q 'signature status: invalid' "${symlink_signature_log}"
grep -q 'signature failure: signature is a symlink' "${symlink_signature_log}"

external_tmp="${TMPDIR:-/tmp}"
mkdir -p "${external_tmp}"
external_artifact="${external_tmp%/}/pgcontext-external-artifact.$$"
printf 'external artifact\n' >"${external_artifact}"
if "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --allow-dirty \
  --artifact "${external_artifact}" \
  --out-dir "${work_dir}/external-artifact"; then
  rm -f "${external_artifact}"
  echo "external artifact should fail release artifact evidence" >&2
  exit 1
fi
rm -f "${external_artifact}"
grep -q $'pgcontext-external-artifact.' "${work_dir}/external-artifact/summary.tsv"
grep -q $'\tfailed\t0\tnot-run\tunsigned\t' "${work_dir}/external-artifact/summary.tsv"
grep -q -- '- Failed: `1`' "${work_dir}/external-artifact/report.md"
external_artifact_log="$(awk -F'\t' 'NR == 2 { print $9 }' "${work_dir}/external-artifact/summary.tsv")"
grep -q 'artifact is outside repository:' "${external_artifact_log}"

generic_symlink_artifact="${work_dir}/generic-symlink.sql"
generic_symlink_target="${work_dir}/generic-symlink.sql.real"
printf 'generic symlink artifact\n' >"${generic_symlink_target}"
ln -s "${generic_symlink_target}" "${generic_symlink_artifact}"
if "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --allow-dirty \
  --artifact "${generic_symlink_artifact}" \
  --out-dir "${work_dir}/generic-symlink-artifact"; then
  echo "generic symlink artifact should fail release artifact evidence" >&2
  exit 1
fi
grep -q $'generic-symlink.sql\tfailed\t0\tnot-run\tunsigned\t' \
  "${work_dir}/generic-symlink-artifact/summary.tsv"
grep -q -- '- Failed: `1`' "${work_dir}/generic-symlink-artifact/report.md"
generic_symlink_log="$(awk -F'\t' 'NR == 2 { print $9 }' "${work_dir}/generic-symlink-artifact/summary.tsv")"
grep -q 'artifact is a symlink:' "${generic_symlink_log}"

broken_symlink_artifact="${work_dir}/broken-symlink.sql"
ln -s "${work_dir}/does-not-exist.sql" "${broken_symlink_artifact}"
if "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --allow-dirty \
  --artifact "${broken_symlink_artifact}" \
  --out-dir "${work_dir}/broken-symlink-artifact"; then
  echo "broken symlink artifact should fail release artifact evidence" >&2
  exit 1
fi
grep -q $'broken-symlink.sql\tfailed\t0\tnot-run\tunsigned\t' \
  "${work_dir}/broken-symlink-artifact/summary.tsv"
grep -q -- '- Missing: `0`' "${work_dir}/broken-symlink-artifact/report.md"
grep -q -- '- Failed: `1`' "${work_dir}/broken-symlink-artifact/report.md"
broken_symlink_log="$(awk -F'\t' 'NR == 2 { print $9 }' "${work_dir}/broken-symlink-artifact/summary.tsv")"
grep -q 'artifact is a symlink:' "${broken_symlink_log}"

if "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --allow-dirty \
  --artifact "${work_dir}/missing.sql" \
  --out-dir "${work_dir}/missing" 2>"${work_dir}/missing.err"; then
  echo "missing artifact should fail" >&2
  exit 1
fi
grep -q $'missing.sql\tmissing\t0\tnot-run\tunsigned' "${work_dir}/missing/summary.tsv"
missing_log="$(awk -F'\t' 'NR == 2 { print $9 }' "${work_dir}/missing/summary.tsv")"
grep -q 'missing artifact:' "${missing_log}"

if "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --dry-run \
  --artifact "${artifact_a}" \
  --artifact "${artifact_a}" \
  --out-dir "${work_dir}/duplicate" 2>"${work_dir}/duplicate.err"; then
  echo "duplicate artifact should fail" >&2
  exit 1
fi
grep -q 'duplicate artifact:' "${work_dir}/duplicate.err"

if "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --dry-run \
  --out-dir "${work_dir}/no-artifacts" 2>"${work_dir}/no-artifacts.err"; then
  echo "missing --artifact should fail" >&2
  exit 1
fi
grep -q 'at least one --artifact is required' "${work_dir}/no-artifacts.err"

if "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --dry-run \
  --artifact "${artifact_a}" \
  --out-dir / 2>"${work_dir}/bad-out-dir.err"; then
  echo "root out-dir should fail" >&2
  exit 1
fi
grep -q -- '--out-dir must be a non-root path' "${work_dir}/bad-out-dir.err"

clean_root="${work_dir}/clean-root"
mkdir -p \
  "${clean_root}/crates/context-pg" \
  "${clean_root}/target/release-sql"
cat >"${clean_root}/Cargo.toml" <<'DOC'
[workspace.metadata.pgcontext]
rust-toolchain = "1.96.0"
pgrx-version = "0.19.1"
supported-postgres-versions = ["17", "18"]

[workspace.dependencies]
pgrx = "=0.19.1"
DOC
cat >"${clean_root}/.gitignore" <<'DOC'
target/
tmp-sql/
tmp-target/
DOC
cat >"${clean_root}/rust-toolchain.toml" <<'DOC'
[toolchain]
channel = "1.96.0"
DOC
cat >"${clean_root}/crates/context-pg/Cargo.toml" <<'DOC'
[package]
name = "context-pg"
version = "0.1.0"
edition = "2024"
DOC
cat >"${clean_root}/crates/context-pg/pgcontext.control" <<'DOC'
default_version = '0.1.0'
DOC
for major in 17 18; do
  printf 'SQL for PostgreSQL %s\n' "${major}" >"${clean_root}/target/release-sql/pg${major}.sql"
  write_verified_signature "${clean_root}/target/release-sql/pg${major}.sql"
done
git -C "${clean_root}" init -q
git -C "${clean_root}" add .
git -C "${clean_root}" \
  -c user.name='Release Test' \
  -c user.email='release-test@example.invalid' \
  commit -q -m 'initial clean fixture'
for major in 17 18; do
  write_generation_log "${clean_root}" "${major}"
done

REPO_ROOT="${clean_root}" "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --require-signatures \
  --artifact target/release-sql/pg17.sql \
  --out-dir "${work_dir}/clean-partial"
grep -q -- '- Worktree: `clean`' "${work_dir}/clean-partial/report.md"
grep -q -- '- Passed: `1`' "${work_dir}/clean-partial/report.md"
grep -q -- '- Missing supported SQL artifacts: `pg18`' \
  "${work_dir}/clean-partial/report.md"
grep -q -- '- Full release scope: `0`' "${work_dir}/clean-partial/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/clean-partial/report.md"

mkdir -p "${clean_root}/tmp-sql"
for major in 17 18; do
  printf 'wrong path SQL for PostgreSQL %s\n' "${major}" >"${clean_root}/tmp-sql/pg${major}.sql"
  write_verified_signature "${clean_root}/tmp-sql/pg${major}.sql"
done

REPO_ROOT="${clean_root}" "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --require-signatures \
  --artifact tmp-sql/pg17.sql \
  --artifact tmp-sql/pg18.sql \
  --out-dir "${work_dir}/clean-wrong-path"
grep -q -- '- Worktree: `clean`' "${work_dir}/clean-wrong-path/report.md"
grep -q -- '- Passed: `2`' "${work_dir}/clean-wrong-path/report.md"
grep -q -- '- Missing supported SQL artifacts: `pg17,pg18`' \
  "${work_dir}/clean-wrong-path/report.md"
grep -q -- '- Full release scope: `0`' "${work_dir}/clean-wrong-path/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/clean-wrong-path/report.md"

rm -rf "${clean_root}/target/release-sql"
ln -s ../tmp-sql "${clean_root}/target/release-sql"
for major in 17 18; do
  write_generation_log "${clean_root}" "${major}"
done

REPO_ROOT="${clean_root}" "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --require-signatures \
  --artifact tmp-sql/pg17.sql \
  --artifact tmp-sql/pg18.sql \
  --out-dir "${work_dir}/clean-symlink-wrong-label"
grep -q -- '- Worktree: `clean`' "${work_dir}/clean-symlink-wrong-label/report.md"
grep -q -- '- Passed: `2`' "${work_dir}/clean-symlink-wrong-label/report.md"
grep -q -- '- Missing supported SQL artifacts: `pg17,pg18`' \
  "${work_dir}/clean-symlink-wrong-label/report.md"
grep -q -- '- Full release scope: `0`' "${work_dir}/clean-symlink-wrong-label/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/clean-symlink-wrong-label/report.md"

REPO_ROOT="${clean_root}" "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --require-signatures \
  --artifact target/release-sql/pg17.sql \
  --artifact target/release-sql/pg18.sql \
  --out-dir "${work_dir}/clean-symlink-target-label"
grep -q -- '- Worktree: `clean`' "${work_dir}/clean-symlink-target-label/report.md"
grep -q -- '- Passed: `2`' "${work_dir}/clean-symlink-target-label/report.md"
grep -q -- '- Missing supported SQL artifacts: `none`' \
  "${work_dir}/clean-symlink-target-label/report.md"
grep -q -- '- Release SQL directory symlink: `1`' \
  "${work_dir}/clean-symlink-target-label/report.md"
grep -q -- '- Full release scope: `0`' "${work_dir}/clean-symlink-target-label/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/clean-symlink-target-label/report.md"

rm "${clean_root}/target/release-sql"
rm -rf "${clean_root}/tmp-target"
mkdir -p "${clean_root}/tmp-target/release-sql"
for major in 17 18; do
  printf 'parent symlink SQL for PostgreSQL %s\n' "${major}" >"${clean_root}/tmp-target/release-sql/pg${major}.sql"
  write_verified_signature "${clean_root}/tmp-target/release-sql/pg${major}.sql"
done
rm -rf "${clean_root}/target"
ln -s tmp-target "${clean_root}/target"
for major in 17 18; do
  write_generation_log "${clean_root}" "${major}"
done

REPO_ROOT="${clean_root}" "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --allow-dirty \
  --require-signatures \
  --artifact target/release-sql/pg17.sql \
  --artifact target/release-sql/pg18.sql \
  --out-dir "${work_dir}/clean-symlink-target-parent"
grep -q -- '- Worktree: `dirty`' "${work_dir}/clean-symlink-target-parent/report.md"
grep -q -- '- Dirty override: `1`' "${work_dir}/clean-symlink-target-parent/report.md"
grep -q -- '- Passed: `2`' "${work_dir}/clean-symlink-target-parent/report.md"
grep -q -- '- Missing supported SQL artifacts: `none`' \
  "${work_dir}/clean-symlink-target-parent/report.md"
grep -q -- '- Release SQL directory symlink: `1`' \
  "${work_dir}/clean-symlink-target-parent/report.md"
grep -q -- '- Full release scope: `0`' "${work_dir}/clean-symlink-target-parent/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/clean-symlink-target-parent/report.md"

rm "${clean_root}/target"
mkdir -p "${clean_root}/target"
mkdir -p "${clean_root}/target/release-sql"
for major in 17 18; do
  ln -s "../../tmp-sql/pg${major}.sql" "${clean_root}/target/release-sql/pg${major}.sql"
  ln -s "../../tmp-sql/pg${major}.sql.asc" "${clean_root}/target/release-sql/pg${major}.sql.asc"
done
for major in 17 18; do
  write_generation_log "${clean_root}" "${major}"
done

if REPO_ROOT="${clean_root}" "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --require-signatures \
  --artifact target/release-sql/pg17.sql \
  --artifact target/release-sql/pg18.sql \
  --out-dir "${work_dir}/clean-symlink-artifact-files"; then
  echo "symlink artifact files should fail release artifact evidence" >&2
  exit 1
fi
grep -q -- '- Worktree: `clean`' "${work_dir}/clean-symlink-artifact-files/report.md"
grep -q -- '- Passed: `0`' "${work_dir}/clean-symlink-artifact-files/report.md"
grep -q -- '- Failed: `2`' "${work_dir}/clean-symlink-artifact-files/report.md"
grep -q -- '- Missing supported SQL artifacts: `none`' \
  "${work_dir}/clean-symlink-artifact-files/report.md"
grep -q -- '- Supported SQL artifact symlink: `1`' \
  "${work_dir}/clean-symlink-artifact-files/report.md"
grep -q -- '- Release SQL directory symlink: `0`' \
  "${work_dir}/clean-symlink-artifact-files/report.md"
grep -q -- '- Full release scope: `0`' "${work_dir}/clean-symlink-artifact-files/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/clean-symlink-artifact-files/report.md"

rm -rf "${clean_root}/target/release-sql"
mkdir -p "${clean_root}/target/release-sql"
for major in 17 18; do
  printf 'SQL for PostgreSQL %s\n' "${major}" >"${clean_root}/target/release-sql/pg${major}.sql"
  write_verified_signature "${clean_root}/target/release-sql/pg${major}.sql"
done
for major in 17 18; do
  write_generation_log "${clean_root}" "${major}"
done

REPO_ROOT="${clean_root}" "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --require-signatures \
  --artifact target/release-sql/pg17.sql \
  --artifact ./target/release-sql/pg17.sql \
  --artifact target/release-sql/pg18.sql \
  --out-dir "${work_dir}/clean-duplicate-alias"
grep -q -- '- Worktree: `clean`' "${work_dir}/clean-duplicate-alias/report.md"
grep -q -- '- Passed: `3`' "${work_dir}/clean-duplicate-alias/report.md"
grep -q -- '- Missing supported SQL artifacts: `none`' \
  "${work_dir}/clean-duplicate-alias/report.md"
grep -q -- '- Duplicate supported SQL artifacts: `1`' \
  "${work_dir}/clean-duplicate-alias/report.md"
grep -q -- '- Supported SQL artifact symlink: `0`' \
  "${work_dir}/clean-duplicate-alias/report.md"
grep -q -- '- Release SQL directory symlink: `0`' \
  "${work_dir}/clean-duplicate-alias/report.md"
grep -q -- '- Full release scope: `0`' "${work_dir}/clean-duplicate-alias/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/clean-duplicate-alias/report.md"

mv "${clean_root}/target/release-sql/pg17.sql.build.log" \
  "${work_dir}/pg17.sql.build.log.real"
ln -s "${work_dir}/pg17.sql.build.log.real" \
  "${clean_root}/target/release-sql/pg17.sql.build.log"
if REPO_ROOT="${clean_root}" "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --require-signatures \
  --artifact target/release-sql/pg17.sql \
  --artifact target/release-sql/pg18.sql \
  --out-dir "${work_dir}/clean-symlink-generation-log"; then
  echo "symlink generation log should fail release artifact evidence" >&2
  exit 1
fi
grep -q -- '- Worktree: `clean`' "${work_dir}/clean-symlink-generation-log/report.md"
grep -q -- '- Passed: `2`' "${work_dir}/clean-symlink-generation-log/report.md"
grep -q -- '- Invalid generation logs: `1`' \
  "${work_dir}/clean-symlink-generation-log/report.md"
grep -q -- '- Full release scope: `1`' \
  "${work_dir}/clean-symlink-generation-log/report.md"
grep -q -- '- Approval: `incomplete`' \
  "${work_dir}/clean-symlink-generation-log/report.md"
rm "${clean_root}/target/release-sql/pg17.sql.build.log"
mv "${work_dir}/pg17.sql.build.log.real" \
  "${clean_root}/target/release-sql/pg17.sql.build.log"

REPO_ROOT="${clean_root}" "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --require-signatures \
  --artifact target/release-sql/pg17.sql \
  --artifact target/release-sql/pg18.sql \
  --out-dir "${work_dir}/clean-full"
grep -q -- '- Worktree: `clean`' "${work_dir}/clean-full/report.md"
grep -q -- '- Passed: `2`' "${work_dir}/clean-full/report.md"
grep -q -- '- Missing supported SQL artifacts: `none`' \
  "${work_dir}/clean-full/report.md"
grep -q -- '- Supported SQL artifact symlink: `0`' "${work_dir}/clean-full/report.md"
grep -q -- '- Release SQL directory symlink: `0`' "${work_dir}/clean-full/report.md"
grep -q -- '- Full release scope: `1`' "${work_dir}/clean-full/report.md"
grep -q -- '- Approval: `complete`' "${work_dir}/clean-full/report.md"

cat >"${fake_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "-V" ]]; then
  printf 'cargo 1.96.0-test\n'
  exit 0
fi
if [[ "${1:-}" == "pgrx" && "${2:-}" == "--version" ]]; then
  printf 'cargo-pgrx 0.19.10-test\n'
  exit 0
fi
exit 127
SH
chmod +x "${fake_bin}/cargo"
REPO_ROOT="${clean_root}" "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --require-signatures \
  --artifact target/release-sql/pg17.sql \
  --artifact target/release-sql/pg18.sql \
  --out-dir "${work_dir}/wrong-cargo-pgrx"
grep -q -- '- Metadata mismatches: `1`' "${work_dir}/wrong-cargo-pgrx/report.md"
grep -q -- '- Approval: `incomplete`' "${work_dir}/wrong-cargo-pgrx/report.md"
grep -q 'metadata mismatch: cargo pgrx version does not match pinned pgrx version' \
  "${work_dir}/wrong-cargo-pgrx/metadata.log"
write_matching_fake_cargo

REPO_ROOT="${clean_root}" "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --require-signatures \
  --artifact target/release-sql/pg17.sql \
  --artifact target/release-sql/pg18.sql \
  --out-dir "${clean_root}/target/release-evidence/artifacts"
repo_relative_summary="${clean_root}/target/release-evidence/artifacts/summary.tsv"
repo_relative_report="${clean_root}/target/release-evidence/artifacts/report.md"
grep -q 'Summary TSV: `target/release-evidence/artifacts/summary.tsv`' \
  "${repo_relative_report}"
if grep -qF -- "${clean_root}" "${repo_relative_summary}" "${repo_relative_report}"; then
  echo "repo-local release artifact evidence paths should be repo-relative" >&2
  exit 1
fi
awk -F '\t' '
  NR > 1 && ($6 ~ /^\// || $9 ~ /^\// || $7 !~ /^[1-9][0-9]*$/ || $8 !~ /^[0-9a-f]{64}$/) { bad = 1 }
  END { exit(bad ? 1 : 0) }
' "${repo_relative_summary}"

REPO_ROOT="${clean_root}" "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --require-signatures \
  --artifact "${clean_root}/target/release-sql/pg17.sql" \
  --artifact "${clean_root}/target/release-sql/pg18.sql" \
  --out-dir "${clean_root}/target/release-evidence/absolute-input-artifacts"
absolute_input_summary="${clean_root}/target/release-evidence/absolute-input-artifacts/summary.tsv"
absolute_input_report="${clean_root}/target/release-evidence/absolute-input-artifacts/report.md"
grep -q -- '- Approval: `complete`' "${absolute_input_report}"
if grep -qF -- "${clean_root}" "${absolute_input_summary}" "${absolute_input_report}"; then
  echo "repo-local absolute artifact inputs should be normalized to repo-relative evidence paths" >&2
  exit 1
fi
for major in 17 18; do
  grep -q $'target/release-sql/pg'"${major}"$'.sql\tpassed\t' \
    "${absolute_input_summary}"
done

clean_root_link="${work_dir}/clean-root-link"
ln -s "${clean_root}" "${clean_root_link}"
REPO_ROOT="${clean_root_link}" "${REPO_ROOT}/scripts/run-release-artifact-report.sh" \
  --require-signatures \
  --artifact "${clean_root}/target/release-sql/pg17.sql" \
  --artifact "${clean_root}/target/release-sql/pg18.sql" \
  --out-dir "${clean_root_link}/target/release-evidence/symlink-root-physical-input-artifacts"
symlink_root_summary="${clean_root}/target/release-evidence/symlink-root-physical-input-artifacts/summary.tsv"
symlink_root_report="${clean_root}/target/release-evidence/symlink-root-physical-input-artifacts/report.md"
grep -q -- '- Approval: `complete`' "${symlink_root_report}"
if grep -qF -- "${clean_root}" "${symlink_root_summary}" "${symlink_root_report}" ||
  grep -qF -- "${clean_root_link}" "${symlink_root_summary}" "${symlink_root_report}"; then
  echo "symlink-root physical artifact inputs should be normalized to repo-relative evidence paths" >&2
  exit 1
fi
for major in 17 18; do
  grep -q $'target/release-sql/pg'"${major}"$'.sql\tpassed\t' \
    "${symlink_root_summary}"
done
