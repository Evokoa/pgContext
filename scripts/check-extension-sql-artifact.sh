#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
pg_major=""
artifact="${REPO_ROOT}/sql/pgcontext--0.2.0.sql"

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
  --artifact PATH Checked-in artifact to compare. Defaults to sql/pgcontext--0.2.0.sql.
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
      'CREATE[[:space:]]+OPERATOR[[:space:]]+CLASS[[:space:]]+([^[:space:]]+\.)?bitvec_hnsw_ops\b|DEFAULT[[:space:]]+FOR[[:space:]]+TYPE[[:space:]]+(pgcontext\.)?bitvec[[:space:]]+USING[[:space:]]+pgcontext_hnsw' \
      "${input}" || true
  )"
  if [[ -n "${matches}" ]]; then
    echo "forbidden default bitvec HNSW opclass found in ${label} SQL artifact: ${input}" >&2
    echo "${matches}" >&2
    echo "bitvec HNSW must require an explicit Hamming or Jaccard opclass." >&2
    exit 1
  fi

  if ! perl -0 - "${input}" <<'PERL'
use strict;
use warnings;

my $path = shift @ARGV;
open my $fh, '<', $path or die "cannot read $path: $!\n";
local $/;
my $sql = <$fh>;
exit 0 unless $sql =~ /CREATE\s+OPERATOR\s+CLASS\s+(?:\S+\.)?(?:halfvec|sparsevec|bitvec)_hnsw/is;

my @specs = (
    ['halfvec_hnsw_ops', 'halfvec', 'default', '<->', 'float_ops', 'halfvec_l2_distance'],
    ['halfvec_hnsw_ip_ops', 'halfvec', 'explicit', '<#>', 'float_ops', 'halfvec_negative_inner_product'],
    ['halfvec_hnsw_cosine_ops', 'halfvec', 'explicit', '<=>', 'float_ops', 'halfvec_cosine_distance'],
    ['halfvec_hnsw_l1_ops', 'halfvec', 'explicit', '<+>', 'float_ops', 'halfvec_l1_distance'],
    ['sparsevec_hnsw_ops', 'sparsevec', 'default', '<->', 'float_ops', 'sparsevec_l2_distance'],
    ['sparsevec_hnsw_ip_ops', 'sparsevec', 'explicit', '<#>', 'float_ops', 'sparsevec_negative_inner_product'],
    ['sparsevec_hnsw_cosine_ops', 'sparsevec', 'explicit', '<=>', 'float_ops', 'sparsevec_cosine_distance'],
    ['sparsevec_hnsw_l1_ops', 'sparsevec', 'explicit', '<+>', 'float_ops', 'sparsevec_l1_distance'],
    ['bitvec_hnsw_hamming_ops', 'bitvec', 'explicit', '<~>', 'integer_ops', 'bitvec_hamming_distance'],
    ['bitvec_hnsw_jaccard_ops', 'bitvec', 'explicit', '<%>', 'float_ops', 'bitvec_jaccard_distance'],
);

for my $spec (@specs) {
    my ($name, $type, $default, $operator, $order, $function) = @$spec;
    my @blocks = $sql =~ /CREATE\s+OPERATOR\s+CLASS\s+(?:\S+\.)?\Q$name\E\b.*?;/gis;
    die "expected exactly one $name opclass, found " . scalar(@blocks) . "\n"
        unless @blocks == 1;
    my $block = $blocks[0];
    my $default_ok = $default eq 'default'
        ? $block =~ /DEFAULT\s+FOR\s+TYPE/is
        : $block !~ /DEFAULT\s+FOR\s+TYPE/is;
    die "invalid $name opclass contract\n" unless
        $default_ok
        && $block =~ /FOR\s+TYPE\s+pgcontext\.\Q$type\E\s+USING\s+pgcontext_hnsw/is
        && $block =~ /OPERATOR\s+1\s+pgcontext\.\Q$operator\E\s*\(pgcontext\.\Q$type\E,\s*pgcontext\.\Q$type\E\)\s+FOR\s+ORDER\s+BY\s+pg_catalog\.\Q$order\E/is
        && $block =~ /FUNCTION\s+1\s+pgcontext\.\Q$function\E\s*\(pgcontext\.\Q$type\E,\s*pgcontext\.\Q$type\E\)/is
        && $block =~ /STORAGE\s+pgcontext\.vector/is;
}
PERL
  then
    echo "incomplete promoted non-dense HNSW opclass in ${label} SQL artifact: ${input}" >&2
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
validate_variant_hnsw_opclasses "${generated}" "generated"

normalize_sql "${artifact_path}" "${expected}"
normalize_sql "${generated}" "${actual}"

if ! diff -u "${expected}" "${actual}" >&2; then
  echo "checked-in SQL artifact is stale for PostgreSQL ${pg_major}: ${artifact_path}" >&2
  echo "refresh with: cargo pgrx schema -p context-pg pg${pg_major} --out sql/pgcontext--0.2.0.sql" >&2
  exit 1
fi
