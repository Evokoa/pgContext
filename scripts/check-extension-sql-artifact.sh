#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
pg_major=""
artifact="${REPO_ROOT}/sql/pgcontext--0.1.0.sql"

usage() {
  cat <<'USAGE'
Usage: scripts/check-extension-sql-artifact.sh [options]

Generate pgContext extension SQL with cargo-pgrx and compare it with the
checked-in SQL artifact. The comparison normalizes trailing whitespace and sorts
pgrx connected-object blocks because cargo-pgrx may emit the same objects in a
different order across runs; it still fails when the artifact is missing SQL
objects, catalog constraints, operators, or changed block content.

Options:
  --pg-major N    PostgreSQL major to generate. Defaults to workspace metadata.
  --artifact PATH Checked-in artifact to compare. Defaults to sql/pgcontext--0.1.0.sql.
  -h, --help      Show this help text.
USAGE
}

metadata_value() {
  local key="$1"
  sed -nE "s/^${key}[[:space:]]*=[[:space:]]*\"([^\"]+)\".*/\\1/p" "${REPO_ROOT}/Cargo.toml" | head -n 1
}

normalize_sql() {
  local input="$1"
  local output="$2"
  # pgrx can reorder connected-object blocks even when the SQL surface is the
  # same. Sort whole blocks after whitespace cleanup so the gate catches stale
  # objects without flaking on pgrx emission order.
  perl -0ne '
    s/[ \t]+$//mg;
    s/\n+\z/\n/;
    s/\n+\/\* <begin connected objects> \*\//\n\/\* <begin connected objects> \*\//g;
    s/\/\* <\/end connected objects> \*\/\n+/\/\* <\/end connected objects> \*\/\n/g;
    my @parts = split(/(?=\/\* <begin connected objects> \*\/\n)/);
    my $prefix = shift @parts;
    print $prefix;
    print sort @parts;
  ' "${input}" >"${output}"
}

validate_variant_hnsw_opclasses() {
  local input="$1"
  local label="$2"
  local matches

  matches="$(
    grep -En \
      'CREATE[[:space:]]+OPERATOR[[:space:]]+CLASS[[:space:]]+([^[:space:]]+\.)?bitvec_hnsw_ops\b|DEFAULT[[:space:]]+FOR[[:space:]]+TYPE[[:space:]]+(public\.)?bitvec[[:space:]]+USING[[:space:]]+pgcontext_hnsw|CREATE[[:space:]]+OPERATOR[[:space:]]+CLASS[[:space:]]+([^[:space:]]+\.)?bitvec_hnsw_jaccard_ops\b' \
      "${input}" || true
  )"

  if [[ -n "${matches}" ]]; then
    echo "forbidden incomplete variant HNSW opclass found in ${label} SQL artifact: ${input}" >&2
    echo "${matches}" >&2
    echo "bitvec HNSW must use explicit metric opclasses; only bitvec_hnsw_hamming_ops is currently promoted." >&2
    exit 1
  fi

  matches="$(
    perl -0ne '
      while (/CREATE\s+OPERATOR\s+CLASS\s+(?:\S+\.)?halfvec_hnsw_ops\b.*?;/gis) {
        my $block = $&;
        print $block unless $block =~ /DEFAULT\s+FOR\s+TYPE\s+public\.halfvec\s+USING\s+pgcontext_hnsw/is
            && $block =~ /OPERATOR\s+1\s+pgcontext\.<->\s*\(public\.halfvec,\s*public\.halfvec\)\s+FOR\s+ORDER\s+BY\s+pg_catalog\.float_ops/is
            && $block =~ /FUNCTION\s+1\s+pgcontext\.halfvec_l2_distance\s*\(public\.halfvec,\s*public\.halfvec\)/is
            && $block =~ /STORAGE\s+public\.vector/is;
      }
      while (/CREATE\s+OPERATOR\s+CLASS\s+(?:\S+\.)?sparsevec_hnsw_ops\b.*?;/gis) {
        my $block = $&;
        print $block unless $block =~ /DEFAULT\s+FOR\s+TYPE\s+public\.sparsevec\s+USING\s+pgcontext_hnsw/is
            && $block =~ /OPERATOR\s+1\s+pgcontext\.<->\s*\(public\.sparsevec,\s*public\.sparsevec\)\s+FOR\s+ORDER\s+BY\s+pg_catalog\.float_ops/is
            && $block =~ /FUNCTION\s+1\s+pgcontext\.sparsevec_l2_distance\s*\(public\.sparsevec,\s*public\.sparsevec\)/is
            && $block =~ /STORAGE\s+public\.vector/is;
      }
      while (/CREATE\s+OPERATOR\s+CLASS\s+(?:\S+\.)?bitvec_hnsw_hamming_ops\b.*?;/gis) {
        my $block = $&;
        print $block unless $block =~ /FOR\s+TYPE\s+public\.bitvec\s+USING\s+pgcontext_hnsw/is
            && $block !~ /DEFAULT\s+FOR\s+TYPE/is
            && $block =~ /OPERATOR\s+1\s+pgcontext\.<~>\s*\(public\.bitvec,\s*public\.bitvec\)\s+FOR\s+ORDER\s+BY\s+pg_catalog\.integer_ops/is
            && $block =~ /FUNCTION\s+1\s+pgcontext\.bitvec_hamming_distance\s*\(public\.bitvec,\s*public\.bitvec\)/is
            && $block =~ /STORAGE\s+public\.vector/is;
      }
    ' "${input}"
  )"

  if [[ -n "${matches}" ]]; then
    echo "incomplete dense-storage variant HNSW opclass found in ${label} SQL artifact: ${input}" >&2
    echo "${matches}" >&2
    echo "variant HNSW opclasses must use the promoted metric operator and dense vector storage." >&2
    exit 1
  fi

  local halfvec_count
  local sparsevec_count
  local bitvec_hamming_count
  local hnsw_surface_count

  halfvec_count="$(
    perl -0ne '
      my @blocks = /CREATE\s+OPERATOR\s+CLASS\s+(?:\S+\.)?halfvec_hnsw_ops\b.*?;/gis;
      print scalar(@blocks);
    ' "${input}"
  )"
  sparsevec_count="$(
    perl -0ne '
      my @blocks = /CREATE\s+OPERATOR\s+CLASS\s+(?:\S+\.)?sparsevec_hnsw_ops\b.*?;/gis;
      print scalar(@blocks);
    ' "${input}"
  )"
  bitvec_hamming_count="$(
    perl -0ne '
      my @blocks = /CREATE\s+OPERATOR\s+CLASS\s+(?:\S+\.)?bitvec_hnsw_hamming_ops\b.*?;/gis;
      print scalar(@blocks);
    ' "${input}"
  )"
  hnsw_surface_count="$(
    grep -Ec \
      'CREATE[[:space:]]+ACCESS[[:space:]]+METHOD[[:space:]]+pgcontext_hnsw\b|CREATE[[:space:]]+OPERATOR[[:space:]]+CLASS[[:space:]]+([^[:space:]]+\.)?(halfvec_hnsw_ops|sparsevec_hnsw_ops|bitvec_hnsw_hamming_ops)\b' \
      "${input}" || true
  )"

  if [[ "${hnsw_surface_count}" != "0" && ( "${halfvec_count}" != "1" || "${sparsevec_count}" != "1" || "${bitvec_hamming_count}" != "1" ) ]]; then
    echo "expected exactly one promoted dense-storage variant HNSW opclass for each variant in ${label} SQL artifact: ${input}" >&2
    echo "found halfvec_hnsw_ops=${halfvec_count}, sparsevec_hnsw_ops=${sparsevec_count}, bitvec_hnsw_hamming_ops=${bitvec_hamming_count}" >&2
    exit 1
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --pg-major)
      [[ $# -ge 2 ]] || {
        echo "--pg-major requires a value" >&2
        exit 2
      }
      pg_major="$2"
      shift 2
      ;;
    --artifact)
      [[ $# -ge 2 ]] || {
        echo "--artifact requires a value" >&2
        exit 2
      }
      artifact="$2"
      shift 2
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "${pg_major}" ]]; then
  pg_major="$(metadata_value "primary-postgres-version")"
fi
pg_major="${pg_major#pg}"
if [[ ! "${pg_major}" =~ ^[0-9]+$ ]]; then
  echo "--pg-major must be a numeric PostgreSQL major" >&2
  exit 2
fi

case "${artifact}" in
  /*) artifact_path="${artifact}" ;;
  *) artifact_path="${REPO_ROOT}/${artifact}" ;;
esac

if [[ ! -f "${artifact_path}" ]]; then
  echo "checked-in SQL artifact is missing: ${artifact_path}" >&2
  exit 1
fi
if [[ -L "${artifact_path}" ]]; then
  echo "checked-in SQL artifact must not be a symlink: ${artifact_path}" >&2
  exit 1
fi
validate_variant_hnsw_opclasses "${artifact_path}" "checked-in"

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/pgcontext-sql-artifact.XXXXXX")"
trap 'rm -rf "${tmp_dir}"' EXIT
generated="${tmp_dir}/generated.sql"
expected="${tmp_dir}/expected.normalized.sql"
actual="${tmp_dir}/actual.normalized.sql"

(
  cd "${REPO_ROOT}"
  cargo pgrx schema -p context-pg "pg${pg_major}" --out "${generated}"
)
# The committed artifact carries the pgvector-coexist guards; apply the same
# transform to the fresh pgrx output before diffing so the two paths can
# never diverge. (Regeneration flow: cargo pgrx schema ... --out sql/... &&
# python3 scripts/transform-sql-artifact-coexist.py sql/...)
python3 "${REPO_ROOT}/scripts/transform-sql-artifact-coexist.py" "${generated}"
validate_variant_hnsw_opclasses "${generated}" "generated"

normalize_sql "${artifact_path}" "${expected}"
normalize_sql "${generated}" "${actual}"

if ! diff -u "${expected}" "${actual}" >&2; then
  echo "checked-in SQL artifact is stale for PostgreSQL ${pg_major}: ${artifact_path}" >&2
  echo "refresh with: cargo pgrx schema -p context-pg pg${pg_major} --out sql/pgcontext--0.1.0.sql" >&2
  exit 1
fi
