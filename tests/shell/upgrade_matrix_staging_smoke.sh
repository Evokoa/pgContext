#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TMPDIR="${TMPDIR:-${REPO_ROOT}/target/tmp}"
mkdir -p "${TMPDIR}"
work_dir="$(mktemp -d "${TMPDIR}/upgrade-matrix-staging-test.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT

fixture_root="${work_dir}/repo"
fake_bin="${work_dir}/bin"
fake_sharedir="${work_dir}/pg-shared"
fake_extension_dir="${fake_sharedir}/extension"
mkdir -p \
  "${fixture_root}/crates/context-pg" \
  "${fixture_root}/sql" \
  "${fixture_root}/tests/fixtures/pgvector_stub" \
  "${fixture_root}/tests/heavy" \
  "${fake_bin}" \
  "${fake_extension_dir}"

cp "${REPO_ROOT}/tests/heavy/lib.sh" "${fixture_root}/tests/heavy/lib.sh"
cp "${REPO_ROOT}/tests/heavy/upgrade_matrix.sh" "${fixture_root}/tests/heavy/upgrade_matrix.sh"
cp "${REPO_ROOT}/tests/fixtures/pgvector_stub/"* \
  "${fixture_root}/tests/fixtures/pgvector_stub/"
chmod +x "${fixture_root}/tests/heavy/upgrade_matrix.sh"

cat >"${fixture_root}/crates/context-pg/pgcontext.control" <<'CONTROL'
comment = 'pgcontext upgrade staging smoke'
default_version = '0.2.0'
module_pathname = '$libdir/pgcontext'
relocatable = false
superuser = true
CONTROL

cat >"${fixture_root}/sql/pgcontext--0.1.0.control" <<'CONTROL'
comment = 'pgcontext historical upgrade staging smoke'
default_version = '0.1.0'
module_pathname = '$libdir/pgcontext'
relocatable = false
superuser = false
CONTROL
printf 'historical update SQL fixture\n' \
  >"${fixture_root}/sql/pgcontext--0.1.0--0.2.0.sql"

cat >"${fake_bin}/pg_config" <<SH
#!/usr/bin/env bash
set -euo pipefail
case "\${1:-}" in
  --sharedir) printf '%s\n' "${fake_sharedir}" ;;
  *) printf 'PostgreSQL 17.99-upgrade-staging-smoke\n' ;;
esac
SH
chmod +x "${fake_bin}/pg_config"

cat >"${fake_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'cargo %s\n' "$*" >>"${FAKE_UPGRADE_CARGO_LOG}"
case "$*" in
  pgrx\ start\ * | pgrx\ install\ *) exit 0 ;;
  *)
    printf 'unexpected cargo command: %s\n' "$*" >&2
    exit 127
    ;;
esac
SH
chmod +x "${fake_bin}/cargo"

cat >"${fake_bin}/psql" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

input="$(cat)"
request="${input} $*"
{
  printf 'args: %s\n' "$*"
  printf '%s\n' "${input}"
} >>"${FAKE_UPGRADE_PSQL_LOG}"

if [[ "${request}" == *"CREATE EXTENSION pgcontext VERSION '0.1.0'"* ]]; then
  staged="${FAKE_EXTENSION_DIR}/pgcontext--0.1.0.sql"
  if [[ ! -f "${staged}" ]]; then
    printf 'old install SQL was not staged: %s\n' "${staged}" >&2
    exit 77
  fi
  if ! cmp -s "${FAKE_EXPECTED_OLD_SQL}" "${staged}"; then
    printf 'staged old install SQL did not match source fixture\n' >&2
    exit 78
  fi
  printf 'staged-old-sql-present\n' >>"${FAKE_UPGRADE_PSQL_LOG}"
fi

if [[ "${request}" == *"ALTER EXTENSION pgcontext UPDATE TO '0.2.0-rollback-probe'"* ]]; then
  exit 1
fi

exit 0
SH
chmod +x "${fake_bin}/psql"

run_upgrade_matrix() {
  env \
    PATH="${fake_bin}:${PATH}" \
    REPO_ROOT="${fixture_root}" \
    PG_CONFIG="${fake_bin}/pg_config" \
    PG_VERSION=pg17 \
    PG_FEATURE=pg17 \
    PGHOST=localhost \
    PGPORT=28817 \
    DBNAME=pgcontext_upgrade_matrix_smoke \
    UPGRADE_MATRIX_STAGING_ONLY=1 \
    FAKE_EXTENSION_DIR="${fake_extension_dir}" \
    FAKE_EXPECTED_OLD_SQL="${fixture_root}/sql/pgcontext--0.1.0.sql" \
    FAKE_UPGRADE_CARGO_LOG="${work_dir}/cargo.log" \
    FAKE_UPGRADE_PSQL_LOG="${work_dir}/psql.log" \
    "${fixture_root}/tests/heavy/upgrade_matrix.sh"
}

printf 'old install SQL fixture\n' >"${fixture_root}/sql/pgcontext--0.1.0.sql"
if ! run_upgrade_matrix >"${work_dir}/staged.out" 2>"${work_dir}/staged.err"; then
  cat "${work_dir}/staged.err" >&2
  exit 1
fi
grep -qF 'staged-old-sql-present' "${work_dir}/psql.log"
grep -qF 'upgrade_staging_exercised: 0.1.0 -> 0.2.0' "${work_dir}/staged.out"
if [[ -e "${fake_extension_dir}/pgcontext--0.1.0.sql" ]]; then
  echo "staged previous install SQL should be removed after the run" >&2
  exit 1
fi

printf 'conflicting old install SQL\n' >"${fake_extension_dir}/pgcontext--0.1.0.sql"
if run_upgrade_matrix >"${work_dir}/conflict.out" 2>"${work_dir}/conflict.err"; then
  echo "conflicting previous install SQL should fail" >&2
  exit 1
fi
grep -qF 'previous install SQL already exists with different contents' \
  "${work_dir}/conflict.err"
