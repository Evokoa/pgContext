#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-sql-artifact-smoke.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT

fake_bin="${work_dir}/bin"
repo="${work_dir}/repo"
mkdir -p "${fake_bin}" "${repo}/scripts" "${repo}/sql"

cp "${REPO_ROOT}/scripts/check-extension-sql-artifact.sh" "${repo}/scripts/check-extension-sql-artifact.sh"
chmod +x "${repo}/scripts/check-extension-sql-artifact.sh"

cat >"${repo}/Cargo.toml" <<'TOML'
primary-postgres-version = "17"
TOML

cat >"${fake_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "$1" == "pgrx" && "$2" == "schema" ]]; then
  out=""
  while [[ $# -gt 0 ]]; do
    if [[ "$1" == "--out" ]]; then
      out="$2"
      break
    fi
    shift
  done
  [[ -n "${out}" ]] || exit 2
  {
    printf 'CREATE EXTENSION pgcontext;\n'
    printf 'STRICT \n'
    printf '\n\n'
  } >"${out}"
  exit 0
fi

echo "unexpected cargo command: $*" >&2
exit 127
SH
chmod +x "${fake_bin}/cargo"

PATH="${fake_bin}:${PATH}" REPO_ROOT="${repo}" \
  "${repo}/scripts/check-extension-sql-artifact.sh" \
  --artifact "${repo}/sql/pgcontext--0.1.0.sql" \
  >"${work_dir}/missing.out" 2>"${work_dir}/missing.err" && {
    echo "missing artifact unexpectedly passed" >&2
    exit 1
  }
grep -q 'checked-in SQL artifact is missing' "${work_dir}/missing.err"

printf 'CREATE EXTENSION pgcontext;\nSTRICT\n' >"${repo}/sql/pgcontext--0.1.0.sql"
PATH="${fake_bin}:${PATH}" REPO_ROOT="${repo}" \
  "${repo}/scripts/check-extension-sql-artifact.sh" \
  --artifact "${repo}/sql/pgcontext--0.1.0.sql" \
  >"${work_dir}/fresh.out" 2>"${work_dir}/fresh.err"

cat >"${repo}/sql/pgcontext--0.1.0.sql" <<'SQL'
CREATE EXTENSION pgcontext;
CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_ops
    DEFAULT FOR TYPE public.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (public.halfvec, public.halfvec);
SQL
PATH="${fake_bin}:${PATH}" REPO_ROOT="${repo}" \
  "${repo}/scripts/check-extension-sql-artifact.sh" \
  --artifact "${repo}/sql/pgcontext--0.1.0.sql" \
  >"${work_dir}/forbidden-artifact.out" 2>"${work_dir}/forbidden-artifact.err" && {
    echo "incomplete checked-in halfvec HNSW opclass unexpectedly passed" >&2
    exit 1
  }
grep -q 'incomplete dense-storage variant HNSW opclass found in checked-in SQL artifact' \
  "${work_dir}/forbidden-artifact.err"
grep -q 'halfvec_hnsw_ops' "${work_dir}/forbidden-artifact.err"

cat >"${repo}/sql/pgcontext--0.1.0.sql" <<'SQL'
CREATE EXTENSION pgcontext;
CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_ops
    DEFAULT FOR TYPE public.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (public.halfvec, public.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_l2_distance(public.halfvec, public.halfvec),
    STORAGE public.vector;
CREATE OPERATOR CLASS pgcontext.bitvec_hnsw_hamming_ops
    FOR TYPE public.bitvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<~> (public.bitvec, public.bitvec) FOR ORDER BY pg_catalog.integer_ops,
    FUNCTION 1 pgcontext.bitvec_hamming_distance(public.bitvec, public.bitvec),
    STORAGE public.vector;
SQL
cat >"${fake_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "$1" == "pgrx" && "$2" == "schema" ]]; then
  out=""
  while [[ $# -gt 0 ]]; do
    if [[ "$1" == "--out" ]]; then
      out="$2"
      break
    fi
    shift
  done
  [[ -n "${out}" ]] || exit 2
  cat >"${out}" <<'SQL'
CREATE EXTENSION pgcontext;
CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_ops
    DEFAULT FOR TYPE public.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (public.halfvec, public.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_l2_distance(public.halfvec, public.halfvec),
    STORAGE public.vector;
CREATE OPERATOR CLASS pgcontext.bitvec_hnsw_hamming_ops
    FOR TYPE public.bitvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<~> (public.bitvec, public.bitvec) FOR ORDER BY pg_catalog.integer_ops,
    FUNCTION 1 pgcontext.bitvec_hamming_distance(public.bitvec, public.bitvec),
    STORAGE public.vector;
SQL
  exit 0
fi

echo "unexpected cargo command: $*" >&2
exit 127
SH
chmod +x "${fake_bin}/cargo"
PATH="${fake_bin}:${PATH}" REPO_ROOT="${repo}" \
  "${repo}/scripts/check-extension-sql-artifact.sh" \
  --artifact "${repo}/sql/pgcontext--0.1.0.sql" \
  >"${work_dir}/partial-halfvec.out" 2>"${work_dir}/partial-halfvec.err" && {
    echo "partial halfvec/bitvec HNSW opclass set unexpectedly passed" >&2
    exit 1
  }
grep -q 'expected exactly one promoted dense-storage variant HNSW opclass for each variant' \
  "${work_dir}/partial-halfvec.err"
grep -q 'sparsevec_hnsw_ops=0' "${work_dir}/partial-halfvec.err"

cat >"${repo}/sql/pgcontext--0.1.0.sql" <<'SQL'
CREATE EXTENSION pgcontext;
CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_ops
    DEFAULT FOR TYPE public.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (public.halfvec, public.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_l2_distance(public.halfvec, public.halfvec),
    STORAGE public.vector;
CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_ops
    DEFAULT FOR TYPE public.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (public.sparsevec, public.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_l2_distance(public.sparsevec, public.sparsevec),
    STORAGE public.vector;
CREATE OPERATOR CLASS pgcontext.bitvec_hnsw_hamming_ops
    FOR TYPE public.bitvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<~> (public.bitvec, public.bitvec) FOR ORDER BY pg_catalog.integer_ops,
    FUNCTION 1 pgcontext.bitvec_hamming_distance(public.bitvec, public.bitvec),
    STORAGE public.vector;
SQL
cat >"${fake_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "$1" == "pgrx" && "$2" == "schema" ]]; then
  out=""
  while [[ $# -gt 0 ]]; do
    if [[ "$1" == "--out" ]]; then
      out="$2"
      break
    fi
    shift
  done
  [[ -n "${out}" ]] || exit 2
  cat >"${out}" <<'SQL'
CREATE EXTENSION pgcontext;
CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_ops
    DEFAULT FOR TYPE public.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (public.sparsevec, public.sparsevec);
SQL
  exit 0
fi

echo "unexpected cargo command: $*" >&2
exit 127
SH
chmod +x "${fake_bin}/cargo"
PATH="${fake_bin}:${PATH}" REPO_ROOT="${repo}" \
  "${repo}/scripts/check-extension-sql-artifact.sh" \
  --artifact "${repo}/sql/pgcontext--0.1.0.sql" \
  >"${work_dir}/forbidden-generated.out" 2>"${work_dir}/forbidden-generated.err" && {
    echo "incomplete generated sparsevec HNSW opclass unexpectedly passed" >&2
    exit 1
  }
grep -q 'incomplete dense-storage variant HNSW opclass found in generated SQL artifact' \
  "${work_dir}/forbidden-generated.err"
grep -q 'sparsevec_hnsw_ops' "${work_dir}/forbidden-generated.err"

cat >"${repo}/sql/pgcontext--0.1.0.sql" <<'SQL'
CREATE EXTENSION pgcontext;
CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_ops
    DEFAULT FOR TYPE public.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (public.sparsevec, public.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_l2_distance(public.sparsevec, public.sparsevec),
    STORAGE public.vector;
CREATE OPERATOR CLASS pgcontext.bitvec_hnsw_hamming_ops
    FOR TYPE public.bitvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<~> (public.bitvec, public.bitvec) FOR ORDER BY pg_catalog.integer_ops,
    FUNCTION 1 pgcontext.bitvec_hamming_distance(public.bitvec, public.bitvec),
    STORAGE public.vector;
SQL
cat >"${fake_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "$1" == "pgrx" && "$2" == "schema" ]]; then
  out=""
  while [[ $# -gt 0 ]]; do
    if [[ "$1" == "--out" ]]; then
      out="$2"
      break
    fi
    shift
  done
  [[ -n "${out}" ]] || exit 2
  cat >"${out}" <<'SQL'
CREATE EXTENSION pgcontext;
CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_ops
    DEFAULT FOR TYPE public.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (public.sparsevec, public.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_l2_distance(public.sparsevec, public.sparsevec),
    STORAGE public.vector;
CREATE OPERATOR CLASS pgcontext.bitvec_hnsw_hamming_ops
    FOR TYPE public.bitvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<~> (public.bitvec, public.bitvec) FOR ORDER BY pg_catalog.integer_ops,
    FUNCTION 1 pgcontext.bitvec_hamming_distance(public.bitvec, public.bitvec),
    STORAGE public.vector;
SQL
  exit 0
fi

echo "unexpected cargo command: $*" >&2
exit 127
SH
chmod +x "${fake_bin}/cargo"
PATH="${fake_bin}:${PATH}" REPO_ROOT="${repo}" \
  "${repo}/scripts/check-extension-sql-artifact.sh" \
  --artifact "${repo}/sql/pgcontext--0.1.0.sql" \
  >"${work_dir}/partial-sparsevec.out" 2>"${work_dir}/partial-sparsevec.err" && {
    echo "partial sparsevec/bitvec HNSW opclass set unexpectedly passed" >&2
    exit 1
  }
grep -q 'expected exactly one promoted dense-storage variant HNSW opclass for each variant' \
  "${work_dir}/partial-sparsevec.err"
grep -q 'halfvec_hnsw_ops=0' "${work_dir}/partial-sparsevec.err"

cat >"${repo}/sql/pgcontext--0.1.0.sql" <<'SQL'
CREATE EXTENSION pgcontext;
CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_ops
    DEFAULT FOR TYPE public.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (public.halfvec, public.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_l2_distance(public.halfvec, public.halfvec),
    STORAGE public.vector;
CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_ops
    DEFAULT FOR TYPE public.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (public.sparsevec, public.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_l2_distance(public.sparsevec, public.sparsevec),
    STORAGE public.vector;
CREATE OPERATOR CLASS pgcontext.bitvec_hnsw_hamming_ops
    FOR TYPE public.bitvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<~> (public.bitvec, public.bitvec) FOR ORDER BY pg_catalog.integer_ops,
    FUNCTION 1 pgcontext.bitvec_hamming_distance(public.bitvec, public.bitvec),
    STORAGE public.vector;
SQL
cat >"${fake_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "$1" == "pgrx" && "$2" == "schema" ]]; then
  out=""
  while [[ $# -gt 0 ]]; do
    if [[ "$1" == "--out" ]]; then
      out="$2"
      break
    fi
    shift
  done
  [[ -n "${out}" ]] || exit 2
  cat >"${out}" <<'SQL'
CREATE EXTENSION pgcontext;
CREATE OPERATOR CLASS pgcontext.halfvec_hnsw_ops
    DEFAULT FOR TYPE public.halfvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (public.halfvec, public.halfvec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.halfvec_l2_distance(public.halfvec, public.halfvec),
    STORAGE public.vector;
CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_ops
    DEFAULT FOR TYPE public.sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (public.sparsevec, public.sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_l2_distance(public.sparsevec, public.sparsevec),
    STORAGE public.vector;
CREATE OPERATOR CLASS pgcontext.bitvec_hnsw_hamming_ops
    FOR TYPE public.bitvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<~> (public.bitvec, public.bitvec) FOR ORDER BY pg_catalog.integer_ops,
    FUNCTION 1 pgcontext.bitvec_hamming_distance(public.bitvec, public.bitvec),
    STORAGE public.vector;
SQL
  exit 0
fi

echo "unexpected cargo command: $*" >&2
exit 127
SH
chmod +x "${fake_bin}/cargo"
PATH="${fake_bin}:${PATH}" REPO_ROOT="${repo}" \
  "${repo}/scripts/check-extension-sql-artifact.sh" \
  --artifact "${repo}/sql/pgcontext--0.1.0.sql" \
  >"${work_dir}/allowed-variant-set.out" 2>"${work_dir}/allowed-variant-set.err"

cat >"${repo}/sql/pgcontext--0.1.0.sql" <<'SQL'
CREATE EXTENSION pgcontext;
CREATE OPERATOR CLASS pgcontext.sparsevec_hnsw_ops
    DEFAULT FOR TYPE sparsevec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<-> (sparsevec, sparsevec) FOR ORDER BY pg_catalog.float_ops,
    FUNCTION 1 pgcontext.sparsevec_l2_distance(sparsevec, sparsevec),
    STORAGE vector;
SQL
PATH="${fake_bin}:${PATH}" REPO_ROOT="${repo}" \
  "${repo}/scripts/check-extension-sql-artifact.sh" \
  --artifact "${repo}/sql/pgcontext--0.1.0.sql" \
  >"${work_dir}/unqualified-sparsevec.out" 2>"${work_dir}/unqualified-sparsevec.err" && {
    echo "unqualified sparsevec HNSW opclass unexpectedly passed" >&2
    exit 1
  }
grep -q 'incomplete dense-storage variant HNSW opclass found in checked-in SQL artifact' \
  "${work_dir}/unqualified-sparsevec.err"
grep -q 'sparsevec_hnsw_ops' "${work_dir}/unqualified-sparsevec.err"

printf 'CREATE EXTENSION pgcontext;\nSTRICT\n' >"${repo}/sql/pgcontext--0.1.0.sql"
cat >"${fake_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "$1" == "pgrx" && "$2" == "schema" ]]; then
  out=""
  while [[ $# -gt 0 ]]; do
    if [[ "$1" == "--out" ]]; then
      out="$2"
      break
    fi
    shift
  done
  [[ -n "${out}" ]] || exit 2
  cat >"${out}" <<'SQL'
CREATE EXTENSION pgcontext;
CREATE OPERATOR CLASS pgcontext.bitvec_hnsw_ops
    DEFAULT FOR TYPE bitvec USING pgcontext_hnsw AS
    OPERATOR 1 pgcontext.<~> (bitvec, bitvec);
SQL
  exit 0
fi

echo "unexpected cargo command: $*" >&2
exit 127
SH
chmod +x "${fake_bin}/cargo"
PATH="${fake_bin}:${PATH}" REPO_ROOT="${repo}" \
  "${repo}/scripts/check-extension-sql-artifact.sh" \
  --artifact "${repo}/sql/pgcontext--0.1.0.sql" \
  >"${work_dir}/forbidden-bitvec.out" 2>"${work_dir}/forbidden-bitvec.err" && {
    echo "forbidden generated bitvec HNSW opclass unexpectedly passed" >&2
    exit 1
  }
grep -q 'forbidden incomplete variant HNSW opclass found in generated SQL artifact' \
  "${work_dir}/forbidden-bitvec.err"
grep -q 'bitvec_hnsw_ops' "${work_dir}/forbidden-bitvec.err"

cat >"${fake_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "$1" == "pgrx" && "$2" == "schema" ]]; then
  out=""
  while [[ $# -gt 0 ]]; do
    if [[ "$1" == "--out" ]]; then
      out="$2"
      break
    fi
    shift
  done
  [[ -n "${out}" ]] || exit 2
  cat >"${out}" <<'SQL'
CREATE SCHEMA pgcontext;
/* <begin connected objects> */
CREATE FUNCTION pgcontext.a() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$;
/* </end connected objects> */

/* <begin connected objects> */
CREATE FUNCTION pgcontext.b() RETURNS int LANGUAGE sql AS $$ SELECT 2 $$;
/* </end connected objects> */
SQL
  exit 0
fi

echo "unexpected cargo command: $*" >&2
exit 127
SH
chmod +x "${fake_bin}/cargo"

cat >"${repo}/sql/pgcontext--0.1.0.sql" <<'SQL'
CREATE SCHEMA pgcontext;
/* <begin connected objects> */
CREATE FUNCTION pgcontext.b() RETURNS int LANGUAGE sql AS $$ SELECT 2 $$;
/* </end connected objects> */
/* <begin connected objects> */
CREATE FUNCTION pgcontext.a() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$;
/* </end connected objects> */
SQL
PATH="${fake_bin}:${PATH}" REPO_ROOT="${repo}" \
  "${repo}/scripts/check-extension-sql-artifact.sh" \
  --artifact "${repo}/sql/pgcontext--0.1.0.sql" \
  >"${work_dir}/reordered.out" 2>"${work_dir}/reordered.err"

cat >"${repo}/sql/pgcontext--0.1.0.sql" <<'SQL'
CREATE SCHEMA pgcontext;
/* <begin connected objects> */
CREATE FUNCTION pgcontext.b() RETURNS int LANGUAGE sql AS $$ SELECT 9 $$;
/* </end connected objects> */
/* <begin connected objects> */
CREATE FUNCTION pgcontext.a() RETURNS int LANGUAGE sql AS $$ SELECT 1 $$;
/* </end connected objects> */
SQL
PATH="${fake_bin}:${PATH}" REPO_ROOT="${repo}" \
  "${repo}/scripts/check-extension-sql-artifact.sh" \
  --artifact "${repo}/sql/pgcontext--0.1.0.sql" \
  >"${work_dir}/changed-block.out" 2>"${work_dir}/changed-block.err" && {
    echo "changed connected-object block unexpectedly passed" >&2
    exit 1
  }
grep -q 'checked-in SQL artifact is stale for PostgreSQL 17' "${work_dir}/changed-block.err"

cat >"${fake_bin}/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "$1" == "pgrx" && "$2" == "schema" ]]; then
  out=""
  while [[ $# -gt 0 ]]; do
    if [[ "$1" == "--out" ]]; then
      out="$2"
      break
    fi
    shift
  done
  [[ -n "${out}" ]] || exit 2
  {
    printf 'CREATE EXTENSION pgcontext;\n'
    printf 'STRICT \n'
    printf '\n\n'
  } >"${out}"
  exit 0
fi

echo "unexpected cargo command: $*" >&2
exit 127
SH
chmod +x "${fake_bin}/cargo"

printf 'CREATE EXTENSION stale;\n' >"${repo}/sql/pgcontext--0.1.0.sql"
PATH="${fake_bin}:${PATH}" REPO_ROOT="${repo}" \
  "${repo}/scripts/check-extension-sql-artifact.sh" \
  --artifact "${repo}/sql/pgcontext--0.1.0.sql" \
  >"${work_dir}/stale.out" 2>"${work_dir}/stale.err" && {
    echo "stale artifact unexpectedly passed" >&2
    exit 1
  }
grep -q 'checked-in SQL artifact is stale for PostgreSQL 17' "${work_dir}/stale.err"
