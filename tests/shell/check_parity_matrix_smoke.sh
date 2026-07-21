#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TMPDIR="${TMPDIR:-${REPO_ROOT}/target/tmp}"
mkdir -p "${TMPDIR}"
work_dir="$(mktemp -d "${TMPDIR}/parity-matrix-test.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT

make_fixture() {
  local root="$1"
  mkdir -p "${root}/scripts" "${root}/docs/user_guide"
  cp "${REPO_ROOT}/scripts/generate-parity-matrix.sh" "${root}/scripts/"
  cp "${REPO_ROOT}/scripts/check-parity-matrix.sh" "${root}/scripts/"
  chmod +x "${root}/scripts/generate-parity-matrix.sh" "${root}/scripts/check-parity-matrix.sh"
  cat >"${root}/docs/user_guide/api_reference.md" <<'DOC'
# API
DOC
  cat >"${root}/docs/user_guide/vector_search.md" <<'DOC'
# Vector Search
DOC
  cat >"${root}/docs/user_guide/parity_matrix.data" <<'DOC'
Capability|Reference|Status|pgContext release contract|Owning reference
Exact vector search over arrays and registered tables|pgvector,Qdrant|stable|Stable exact search.|docs/user_guide/vector_search.md
Dense vector SQL type, casts, operators, aggregates|pgvector|stable|Stable dense SQL.|docs/user_guide/api_reference.md
Filter JSON over ordinary columns and JSONB paths|Qdrant|stable|Stable filters.|docs/user_guide/api_reference.md
HNSW access method|pgvector,Qdrant|experimental|Implemented experimental HNSW.|docs/user_guide/vector_search.md
Filtered ANN serving|Qdrant|experimental|Implemented experimental filtered ANN.|docs/user_guide/vector_search.md
SQL halfvec|pgvector|experimental|SQL-visible but outside stable promise.|docs/user_guide/api_reference.md
IVFFlat|pgvector|intentionally different|Not supported in first production surface.|docs/user_guide/vector_search.md
DOC
  (cd "${root}" && scripts/generate-parity-matrix.sh)
}

good_root="${work_dir}/good"
make_fixture "${good_root}"
(cd "${good_root}" && scripts/check-parity-matrix.sh)
(cd "${good_root}" && scripts/check-parity-matrix.sh --require-v1-launch-complete)

missing_v1_root="${work_dir}/missing-v1"
make_fixture "${missing_v1_root}"
perl -0pi -e 's/^Filtered ANN serving[^\n]*\n//m' \
  "${missing_v1_root}/docs/user_guide/parity_matrix.data"
(cd "${missing_v1_root}" && scripts/generate-parity-matrix.sh)
if (cd "${missing_v1_root}" && scripts/check-parity-matrix.sh --require-v1-launch-complete) \
  >"${work_dir}/missing-v1.err" 2>&1; then
  echo "launch-complete parity should require filtered ANN" >&2
  exit 1
fi
grep -q 'V1 launch requires Filtered ANN serving' "${work_dir}/missing-v1.err"

stale_root="${work_dir}/stale"
make_fixture "${stale_root}"
printf '\nmanual stale edit\n' >>"${stale_root}/docs/user_guide/parity_matrix.md"
if (cd "${stale_root}" && scripts/check-parity-matrix.sh) >"${work_dir}/stale.err" 2>&1; then
  echo "stale generated parity matrix should fail" >&2
  exit 1
fi
grep -q 'manual stale edit' "${work_dir}/stale.err"

bad_status_root="${work_dir}/bad-status"
make_fixture "${bad_status_root}"
perl -0pi -e 's/experimental/preview/' "${bad_status_root}/docs/user_guide/parity_matrix.data"
(cd "${bad_status_root}" && scripts/generate-parity-matrix.sh)
if (cd "${bad_status_root}" && scripts/check-parity-matrix.sh) >"${work_dir}/bad-status.err" 2>&1; then
  echo "invalid parity status should fail" >&2
  exit 1
fi
grep -q 'invalid parity status for HNSW access method: preview' "${work_dir}/bad-status.err"

duplicate_root="${work_dir}/duplicate"
make_fixture "${duplicate_root}"
cat >>"${duplicate_root}/docs/user_guide/parity_matrix.data" <<'DOC'
SQL halfvec|pgvector|experimental|Duplicate row.|docs/user_guide/api_reference.md
DOC
(cd "${duplicate_root}" && scripts/generate-parity-matrix.sh)
if (cd "${duplicate_root}" && scripts/check-parity-matrix.sh) >"${work_dir}/duplicate.err" 2>&1; then
  echo "duplicate parity capability should fail" >&2
  exit 1
fi
grep -q 'duplicate parity capability: SQL halfvec' "${work_dir}/duplicate.err"

stable_contradiction_root="${work_dir}/stable-contradiction"
make_fixture "${stable_contradiction_root}"
perl -0pi -e 's/SQL halfvec\|pgvector\|experimental\|SQL-visible but outside stable promise\./SQL halfvec|pgvector|stable|SQL-visible but outside stable promise./' \
  "${stable_contradiction_root}/docs/user_guide/parity_matrix.data"
(cd "${stable_contradiction_root}" && scripts/generate-parity-matrix.sh)
if (cd "${stable_contradiction_root}" && scripts/check-parity-matrix.sh) \
  >"${work_dir}/stable-contradiction.err" 2>&1; then
  echo "stable parity row with non-stable contract wording should fail" >&2
  exit 1
fi
grep -q 'stable parity row has non-stable contract wording for SQL halfvec' \
  "${work_dir}/stable-contradiction.err"

planned_root="${work_dir}/planned-row"
make_fixture "${planned_root}"
cat >>"${planned_root}/docs/user_guide/parity_matrix.data" <<'DOC'
Future capability|Qdrant|planned|Not part of this release.|docs/user_guide/vector_search.md
DOC
(cd "${planned_root}" && scripts/generate-parity-matrix.sh)
if (cd "${planned_root}" && scripts/check-parity-matrix.sh --require-v1-launch-complete) >"${work_dir}/planned.err" 2>&1; then
  echo "planned parity row outside the roadmap should fail launch validation" >&2
  exit 1
fi
grep -q 'planned parity rows must point to docs/user_guide/roadmap.md' \
  "${work_dir}/planned.err"
