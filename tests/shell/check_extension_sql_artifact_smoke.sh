#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-sql-artifact-smoke.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT

fake_bin="${work_dir}/bin"
repo="${work_dir}/repo"
mkdir -p "${fake_bin}" "${repo}/scripts" "${repo}/sql"
cp "${REPO_ROOT}/scripts/check-extension-sql-artifact.sh" \
  "${repo}/scripts/check-extension-sql-artifact.sh"
chmod +x "${repo}/scripts/check-extension-sql-artifact.sh"

cat >"${repo}/Cargo.toml" <<'TOML'
primary-postgres-version = "17"
TOML

write_fake_cargo() {
  local body="$1"
  cat >"${fake_bin}/cargo" <<SH
#!/usr/bin/env bash
set -euo pipefail
if [[ "\$1" == "pgrx" && "\$2" == "schema" ]]; then
  out=""
  while [[ \$# -gt 0 ]]; do
    if [[ "\$1" == "--out" ]]; then
      out="\$2"
      break
    fi
    shift
  done
  [[ -n "\${out}" ]] || exit 2
  printf '%s' '${body}' >"\${out}"
  exit 0
fi
echo "unexpected cargo command: \$*" >&2
exit 127
SH
  chmod +x "${fake_bin}/cargo"
}

run_check() {
  PATH="${fake_bin}:${PATH}" REPO_ROOT="${repo}" \
    "${repo}/scripts/check-extension-sql-artifact.sh" \
    --artifact "${repo}/sql/pgcontext--0.1.0.sql"
}

simple_sql=$'CREATE EXTENSION pgcontext;\nSTRICT\n'
write_fake_cargo "${simple_sql}"

if run_check >"${work_dir}/missing.out" 2>"${work_dir}/missing.err"; then
  echo "missing artifact unexpectedly passed" >&2
  exit 1
fi
grep -q 'checked-in SQL artifact is missing' "${work_dir}/missing.err"

printf '%s' "${simple_sql}" >"${repo}/sql/pgcontext--0.1.0.sql"
run_check >"${work_dir}/fresh.out" 2>"${work_dir}/fresh.err"

incomplete_sql=$'CREATE ACCESS METHOD pgcontext_hnsw TYPE INDEX HANDLER pgcontext.hnsw_handler;\nCREATE OPERATOR CLASS pgcontext.halfvec_hnsw_ops\n    DEFAULT FOR TYPE pgcontext.halfvec USING pgcontext_hnsw AS\n    OPERATOR 1 pgcontext.<-> (pgcontext.halfvec, pgcontext.halfvec);\n'
printf '%s' "${incomplete_sql}" >"${repo}/sql/pgcontext--0.1.0.sql"
if run_check >"${work_dir}/incomplete.out" 2>"${work_dir}/incomplete.err"; then
  echo "incomplete canonical HNSW opclass unexpectedly passed" >&2
  exit 1
fi
grep -q 'incomplete promoted non-dense HNSW opclass in checked-in SQL artifact' \
  "${work_dir}/incomplete.err"
grep -q 'halfvec_hnsw_ops' "${work_dir}/incomplete.err"

generated_reordered=$'CREATE SCHEMA pgcontext;\n/* <begin connected objects> */\nCREATE FUNCTION pgcontext.a() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$;\n/* </end connected objects> */\n\n/* <begin connected objects> */\nCREATE FUNCTION pgcontext.b() RETURNS int LANGUAGE sql AS $$ SELECT 2 $$;\n/* </end connected objects> */\n'
checked_reordered=$'CREATE SCHEMA pgcontext;\n/* <begin connected objects> */\nCREATE FUNCTION pgcontext.b() RETURNS int LANGUAGE sql AS $$ SELECT 2 $$;\n/* </end connected objects> */\n/* <begin connected objects> */\nCREATE FUNCTION pgcontext.a() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$;\n/* </end connected objects> */\n'
write_fake_cargo "${generated_reordered}"
printf '%s' "${checked_reordered}" >"${repo}/sql/pgcontext--0.1.0.sql"
run_check >"${work_dir}/reordered.out" 2>"${work_dir}/reordered.err"

changed_sql="${checked_reordered/SELECT 2/SELECT 9}"
printf '%s' "${changed_sql}" >"${repo}/sql/pgcontext--0.1.0.sql"
if run_check >"${work_dir}/changed.out" 2>"${work_dir}/changed.err"; then
  echo "changed connected-object block unexpectedly passed" >&2
  exit 1
fi
grep -q 'checked-in SQL artifact is stale for PostgreSQL 17' "${work_dir}/changed.err"

write_fake_cargo "${simple_sql}"
printf 'CREATE EXTENSION stale;\n' >"${repo}/sql/pgcontext--0.1.0.sql"
if run_check >"${work_dir}/stale.out" 2>"${work_dir}/stale.err"; then
  echo "stale artifact unexpectedly passed" >&2
  exit 1
fi
grep -q 'checked-in SQL artifact is stale for PostgreSQL 17' "${work_dir}/stale.err"
