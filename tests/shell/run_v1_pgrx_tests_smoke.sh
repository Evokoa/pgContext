#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TMPDIR="${TMPDIR:-${REPO_ROOT}/target/tmp}"
mkdir -p "${TMPDIR}"
work_dir="$(mktemp -d "${TMPDIR}/run-v1-pgrx-tests.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT

fixture_root="${work_dir}/fixture"
fake_bin="${work_dir}/bin"
fake_share="${work_dir}/share"
log_path="${work_dir}/commands.log"
mkdir -p \
  "${fixture_root}/scripts" \
  "${fixture_root}/crates/context-pg" \
  "${fake_bin}" \
  "${fake_share}/extension"
cp "${REPO_ROOT}/scripts/run-v1-pgrx-tests.sh" \
  "${fixture_root}/scripts/run-v1-pgrx-tests.sh"

cat >"${fixture_root}/crates/context-pg/pgcontext.control" <<'CONTROL'
default_version = '0.1.0'
CONTROL

cat >"${fake_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'cargo:%s\n' "$*" >>"${FAKE_PGRX_LOG}"
if [[ "$*" == "pgrx info pg-config pg17" || "$*" == "pgrx info pg-config pg18" ]]; then
  printf '%s/pg_config\n' "${FAKE_PGRX_BIN}"
fi
SH

cat >"${fake_bin}/pg_config" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == "--sharedir" ]]; then
  printf '%s\n' "${FAKE_PGRX_SHARE}"
  exit 0
fi
printf 'unexpected pg_config invocation: %s\n' "$*" >&2
exit 2
SH

cat >"${fake_bin}/psql" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'psql:%s\n' "$*" >>"${FAKE_PGRX_LOG}"
SH

cat >"${fake_bin}/python3" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'python3:%s\n' "$*" >>"${FAKE_PGRX_LOG}"
SH
cat >"${fake_bin}/initdb" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'initdb:%s\n' "$*" >>"${FAKE_PGRX_LOG}"
SH
cat >"${fake_bin}/pg_ctl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'pg_ctl:%s\n' "$*" >>"${FAKE_PGRX_LOG}"
SH
cat >"${fake_bin}/createdb" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'createdb:%s\n' "$*" >>"${FAKE_PGRX_LOG}"
SH
chmod +x "${fake_bin}/cargo" "${fake_bin}/pg_config" "${fake_bin}/psql" \
  "${fake_bin}/python3" "${fake_bin}/initdb" "${fake_bin}/pg_ctl" \
  "${fake_bin}/createdb"

PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PG_MAJOR=18 \
  PGRX_TEST_PLATFORM=Darwin \
  PGRX_TEST_DBNAME=pgcontext_runner_smoke \
  PGRX_TEST_TMPDIR="${work_dir}/clusters" \
  FAKE_PGRX_LOG="${log_path}" \
  FAKE_PGRX_BIN="${fake_bin}" \
  FAKE_PGRX_SHARE="${fake_share}" \
  "${fixture_root}/scripts/run-v1-pgrx-tests.sh"

grep -q '^cargo:pgrx info pg-config pg18$' "${log_path}"
grep -q \
  '^cargo:pgrx install --test --release -p context-pg .* --no-default-features --features pg18 pg_test$' \
  "${log_path}"
grep -q '^initdb:-D .*pgcontext-pgrx-pg18\..*/data --no-locale --encoding=UTF8$' \
  "${log_path}"
grep -q '^pg_ctl:start -D .*pgcontext-pgrx-pg18\..*/data .*' "${log_path}"
grep -q '^createdb:.*pgcontext_runner_smoke$' "${log_path}"
grep -q '^pg_ctl:stop -D .*pgcontext-pgrx-pg18\..*/data -m fast$' "${log_path}"
grep -q 'python3:scripts/run_pgrx_tests_in_server.py .*--database pgcontext_runner_smoke' \
  "${log_path}"
grep -q \
  "python3:.*--extension-sql ${fake_share}/extension/pgcontext--0.1.0.sql" \
  "${log_path}"
if find "${work_dir}/clusters" -mindepth 1 -print -quit | grep -q .; then
  echo "isolated cluster directory should be removed" >&2
  exit 1
fi

: >"${log_path}"
PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PGRX_TEST_PLATFORM=Linux \
  FAKE_PGRX_LOG="${log_path}" \
  "${fixture_root}/scripts/run-v1-pgrx-tests.sh"
grep -q '^cargo:pgrx test --release -p context-pg pg17$' "${log_path}"

if PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PG_MAJOR=16 \
  PGRX_TEST_PLATFORM=Linux \
  FAKE_PGRX_LOG="${log_path}" \
  "${fixture_root}/scripts/run-v1-pgrx-tests.sh" \
  2>"${work_dir}/invalid-major.err"; then
  echo "unsupported PG_MAJOR should fail" >&2
  exit 1
fi
grep -q 'PG_MAJOR must be 17 or 18' "${work_dir}/invalid-major.err"

if PATH="${fake_bin}:${PATH}" \
  REPO_ROOT="${fixture_root}" \
  PGRX_TEST_PLATFORM=Darwin \
  PGRX_TEST_DBNAME='unsafe-name' \
  FAKE_PGRX_LOG="${log_path}" \
  "${fixture_root}/scripts/run-v1-pgrx-tests.sh" \
  2>"${work_dir}/invalid-name.err"; then
  echo "unsafe PGRX_TEST_DBNAME should fail" >&2
  exit 1
fi
grep -q 'PGRX_TEST_DBNAME must be a simple SQL identifier' \
  "${work_dir}/invalid-name.err"
