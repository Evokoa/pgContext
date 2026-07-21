#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
SOURCE="${REPO_ROOT}/docs/user_guide/capability_contract.data"
PARITY="${REPO_ROOT}/docs/user_guide/parity_matrix.data"
FLOOR="${REPO_ROOT}/docs/user_guide/capability_product_floor.data"
PGVECTOR_FLOOR="${REPO_ROOT}/docs/user_guide/pgvector_v1_floor.data"

if [[ ! -f "${SOURCE}" ]]; then
  echo "missing capability contract: ${SOURCE}" >&2
  exit 1
fi
if [[ ! -f "${FLOOR}" ]]; then
  echo "missing product-floor contract: ${FLOOR}" >&2
  exit 1
fi
if [[ ! -f "${PGVECTOR_FLOOR}" ]]; then
  echo "missing pgvector v1 floor: ${PGVECTOR_FLOOR}" >&2
  exit 1
fi

"${REPO_ROOT}/scripts/check-parity-matrix.sh"

expected="$(mktemp "${TMPDIR:-/tmp}/pgcontext-capability-expected.XXXXXX")"
actual="$(mktemp "${TMPDIR:-/tmp}/pgcontext-capability-actual.XXXXXX")"
trap 'rm -f "${expected}" "${actual}"' EXIT

awk -F'|' '
  NR == 1 { next }
  { printf "%s|%s|%s\n", $1, $3, $5 }
' "${PARITY}" | sort >"${expected}"

awk -F'|' -v root="${REPO_ROOT}" '
  function fail(message) {
    print message > "/dev/stderr"
    exit 1
  }
  function verify_ref(value, label, row, parts, path, fragment, command) {
    if (value == "") {
      fail("missing " label " reference for capability row " row)
    }
    split(value, parts, "#")
    path = root "/" parts[1]
    command = "test -f \"" path "\""
    if (system(command) != 0) {
      fail("missing " label " path for capability row " row ": " parts[1])
    }
    fragment = parts[2]
    if (fragment != "") {
      command = "grep -Fq -- \"" fragment "\" \"" path "\""
      if (system(command) != 0) {
        fail("missing " label " fragment for capability row " row ": " value)
      }
    }
  }
  NR == 1 {
    if ($0 != "Capability ID|Capability|Maturity|Source owner|Consumer|Focused test|Lifecycle test|User doc|Source-to-sink trace") {
      fail("invalid capability contract header")
    }
    next
  }
  NF != 9 { fail("invalid capability row " NR ": expected 9 columns, got " NF) }
  $1 !~ /^CAP-[A-Z0-9-]+$/ { fail("invalid capability ID on row " NR ": " $1) }
  $3 != "stable" && $3 != "experimental" && $3 != "planned" && $3 != "intentionally different" {
    fail("invalid capability maturity on row " NR ": " $3)
  }
  seen_id[$1]++ { fail("duplicate capability ID: " $1) }
  seen_capability[$2]++ { fail("duplicate capability name: " $2) }
  $9 == "" { fail("missing source-to-sink trace for capability row " NR) }
  {
    if ($6 !~ /#/) { fail("focused test must include an exact marker on row " NR ": " $6) }
    if ($7 !~ /#/) { fail("lifecycle test must include an exact marker on row " NR ": " $7) }
    verify_ref($4, "source owner", NR)
    verify_ref($5, "consumer", NR)
    verify_ref($6, "focused test", NR)
    verify_ref($7, "lifecycle test", NR)
    verify_ref($8, "user doc", NR)
    printf "%s|%s|%s\n", $2, $3, $8
  }
' "${SOURCE}" | sort >"${actual}"

diff -u "${expected}" "${actual}"

require_contract() {
  local id="$1"
  local maturity="$2"
  if ! awk -F'|' -v id="${id}" -v maturity="${maturity}" \
      'NR > 1 && $1 == id && $3 == maturity { found = 1 } END { exit !found }' \
      "${SOURCE}"; then
    echo "capability floor requires ${id} with maturity ${maturity}" >&2
    exit 1
  fi
}

# Pinned pgvector-compatible v1 floor.
require_contract CAP-PGVECTOR-DENSE-SQL stable
require_contract CAP-EXACT-SEARCH stable
require_contract CAP-HNSW-AM experimental
require_contract CAP-PGVECTOR-HALFVEC experimental
require_contract CAP-PGVECTOR-SPARSEVEC experimental
require_contract CAP-PGVECTOR-BIT experimental

# Required Qdrant-style product build floor. Experimental means implemented SQL
# exists but later build/hardening evidence still owns promotion.
for id in \
  CAP-FILTERS CAP-NAMED-DENSE CAP-NAMED-SPARSE CAP-HYBRID \
  CAP-QUANTIZATION CAP-LATE-INTERACTION CAP-REBUILDABLE-ARTIFACTS \
  CAP-OBSERVABILITY; do
  if ! awk -F'|' -v id="${id}" 'NR > 1 && $1 == id { found = 1 } END { exit !found }' \
      "${SOURCE}"; then
    echo "missing required Qdrant-style build capability: ${id}" >&2
    exit 1
  fi
done

awk -F'|' -v root="${REPO_ROOT}" -v capabilities="${SOURCE}" '
  function fail(message) {
    print message > "/dev/stderr"
    exit 1
  }
  function verify_ref(value, label, row, parts, path, fragment, command) {
    split(value, parts, "#")
    path = root "/" parts[1]
    command = "test -f \"" path "\""
    if (value == "" || system(command) != 0) {
      fail("invalid " label " reference on product-floor row " row ": " value)
    }
    fragment = parts[2]
    if (fragment != "") {
      command = "grep -Fq -- \"" fragment "\" \"" path "\""
      if (system(command) != 0) {
        fail("missing " label " fragment on product-floor row " row ": " value)
      }
    }
  }
  NR == 1 {
    if ($0 != "Requirement ID|Capability ID|Current source-to-sink state|Owning public contract|Required consumer|Post-freeze-only evidence") {
      fail("invalid product-floor contract header")
    }
    next
  }
  NF != 6 { fail("invalid product-floor row " NR ": expected 6 columns, got " NF) }
  $1 !~ /^FLOOR-[A-Z0-9-]+$/ { fail("invalid product-floor ID on row " NR ": " $1) }
  $3 != "serving" && $3 != "partial" && $3 != "exact-only" && $3 != "metadata-only" && $3 != "manual-only" && $3 != "missing" {
    fail("invalid source-to-sink state on product-floor row " NR ": " $3)
  }
  seen[$1]++ { fail("duplicate product-floor ID: " $1) }
  {
    command = "awk -F\047|\047 -v id=\047" $2 "\047 \047NR > 1 && $1 == id { found = 1 } END { exit !found }\047 \"" capabilities "\""
    if (system(command) != 0) {
      fail("product-floor row references unknown capability ID: " $2)
    }
    verify_ref($4, "build owner", NR)
    verify_ref($5, "consumer", NR)
    if ($6 == "") { fail("missing post-freeze evidence boundary on row " NR) }
  }
' "${FLOOR}"

for id in \
  FLOOR-FILTERED-ANN FLOOR-NAMED-DENSE FLOOR-VECTOR-CONFIG FLOOR-NAMED-SPARSE \
  FLOOR-HYBRID-EXECUTION FLOOR-QUANTIZED-HNSW FLOOR-LATE-INTERACTION \
  FLOOR-GENERATION-MAINTENANCE FLOOR-MMAP-SERVING \
  FLOOR-AUTOMATIC-OBSERVABILITY; do
  if ! awk -F'|' -v id="${id}" 'NR > 1 && $1 == id { found = 1 } END { exit !found }' \
      "${FLOOR}"; then
    echo "missing required product-floor item: ${id}" >&2
    exit 1
  fi
done

require_contract CAP-IVFFLAT intentionally\ different
if grep -Eiq 'CREATE[[:space:]]+(ACCESS METHOD|OPERATOR CLASS).*ivfflat' \
    "${REPO_ROOT}/sql/pgcontext--0.1.0.sql"; then
  echo "IVFFlat SQL appeared despite the intentional-difference contract" >&2
  exit 1
fi

awk -F'|' -v root="${REPO_ROOT}" '
  function fail(message) {
    print message > "/dev/stderr"
    exit 1
  }
  function verify_ref(value, label, row, parts, path, fragment, command) {
    split(value, parts, "#")
    path = root "/" parts[1]
    command = "test -f \"" path "\""
    if (value == "" || system(command) != 0) {
      fail("invalid " label " reference on pgvector floor row " row ": " value)
    }
    fragment = parts[2]
    if (fragment != "") {
      command = "grep -Fq -- \"" fragment "\" \"" path "\""
      if (system(command) != 0) {
        fail("missing " label " fragment on pgvector floor row " row ": " value)
      }
    }
  }
  NR == 1 {
    if ($0 != "Floor ID|Category|Representation|Semantics|Public maturity|V1 decision|Installed evidence|Owning public contract") {
      fail("invalid pgvector v1 floor header")
    }
    next
  }
  NF != 8 { fail("invalid pgvector floor row " NR ": expected 8 columns, got " NF) }
  $1 !~ /^PGV-[A-Z0-9-]+$/ { fail("invalid pgvector floor ID on row " NR ": " $1) }
  $5 != "stable" && $5 != "experimental" && $5 != "planned" {
    fail("invalid pgvector floor maturity on row " NR ": " $5)
  }
  $6 != "installed" && $6 != "roadmap" { fail("invalid v1 decision on row " NR ": " $6) }
  seen[$1]++ { fail("duplicate pgvector floor ID: " $1) }
  $6 == "installed" && $7 == "none" { fail("installed pgvector row lacks evidence: " $1) }
  $6 == "roadmap" && $7 != "none" { fail("roadmap pgvector row unexpectedly claims installed evidence: " $1) }
  $5 == "planned" && $6 != "roadmap" { fail("planned pgvector row is not assigned to the roadmap: " $1) }
  $5 != "planned" && $6 == "roadmap" { fail("implemented pgvector row is incorrectly assigned to the roadmap: " $1) }
  $5 == "planned" && $8 !~ /^docs\/user_guide\/roadmap.md#/ {
    fail("planned pgvector row must be owned by the public roadmap: " $1)
  }
  {
    if ($7 != "none") { verify_ref($7, "installed evidence", NR) }
    verify_ref($8, "build owner", NR)
  }
' "${PGVECTOR_FLOOR}"

for id in \
  PGV-ANN-VECTOR-L2 PGV-ANN-VECTOR-IP PGV-ANN-VECTOR-COSINE PGV-ANN-VECTOR-L1 \
  PGV-ANN-HALFVEC-L2 PGV-ANN-HALFVEC-IP PGV-ANN-HALFVEC-COSINE PGV-ANN-HALFVEC-L1 \
  PGV-ANN-SPARSEVEC-L2 PGV-ANN-SPARSEVEC-IP PGV-ANN-SPARSEVEC-COSINE PGV-ANN-SPARSEVEC-L1 \
  PGV-ANN-BITVEC-HAMMING PGV-ANN-BITVEC-JACCARD; do
  if ! awk -F'|' -v id="${id}" 'NR > 1 && $1 == id { found = 1 } END { exit !found }' \
      "${PGVECTOR_FLOOR}"; then
    echo "missing pgvector ANN floor pair: ${id}" >&2
    exit 1
  fi
done

# V1 source-to-sink trace. These structural guards deliberately pin executable
# wiring, not documentation prose: installed dense opclasses must reach metric
# dispatch and persisted page traversal, while filtered ANN must derive masks
# from the registered source predicate and pass the attached index OID into the
# page-backed candidate function.
require_fixed() {
  local path="$1"
  local fragment="$2"
  local label="$3"
  if ! grep -Fq -- "${fragment}" "${REPO_ROOT}/${path}"; then
    echo "missing ${label}: ${path}#${fragment}" >&2
    exit 1
  fi
}

for opclass in \
  vector_hnsw_ops vector_hnsw_ip_ops vector_hnsw_cosine_ops vector_hnsw_l1_ops; do
  require_fixed sql/pgcontext--0.1.0.sql \
    "CREATE OPERATOR CLASS pgcontext.${opclass}" \
    "installed dense HNSW opclass"
done
for metric in L2 NegativeInnerProduct Cosine L1; do
  require_fixed crates/context-pg/src/hnsw_am_validation.rs \
    "HnswScoreMetric::${metric}" \
    "dense HNSW metric dispatch"
done
require_fixed crates/context-pg/src/hnsw_am.rs \
  'stored_config(self, expected_metric: HnswScoreMetric, ef_search: usize)' \
  'persisted HNSW metric/config load'
require_fixed crates/context-pg/src/hnsw_am_page_storage.rs \
  'exact_strategy: false' \
  'page-backed HNSW work accounting'
if grep -R -Fq -- 'exact_strategy: true' \
    "${REPO_ROOT}/crates/context-pg/src/hnsw_am.rs" \
    "${REPO_ROOT}/crates/context-pg/src/hnsw_am_page_storage.rs"; then
  echo "dense HNSW source introduced a silent exact-strategy path" >&2
  exit 1
fi

require_fixed crates/context-pg/src/table_search.rs \
  'let Some(hnsw_index_oid) = registered_vector.hnsw_index_oid else {' \
  'filtered ANN attached-index selection'
require_fixed crates/context-pg/src/table_search.rs \
  'WHERE {filter_sql}' \
  'filtered ANN registered-predicate materialization'
require_fixed crates/context-pg/src/table_search.rs \
  'array_agg(heap_tid::text ORDER BY ordinal) AS heap_tids' \
  'filtered ANN source-derived candidate mask'
require_fixed crates/context-pg/src/table_search.rs \
  'CROSS JOIN LATERAL pgcontext._hnsw_masked_candidates(' \
  'filtered ANN page-backed candidate consumer'
require_fixed crates/context-pg/src/table_search.rs \
  'args.push(hnsw_index_oid.into());' \
  'filtered ANN attached-index binding'
if awk '
    /fn search_registered_table_filtered\(/ { in_filtered_ann = 1 }
    /fn search_registered_table_filtered_exact\(/ { exit }
    in_filtered_ann { print }
  ' "${REPO_ROOT}/crates/context-pg/src/table_search.rs" \
  | grep -Eq 'ARRAY\[|vec!\['; then
  echo "filtered ANN source contains a hardcoded candidate collection" >&2
  exit 1
fi
require_fixed tests/heavy/filtered_ann_recall.sh \
  'IF page_visits <= 0 OR node_reads <= 0 OR candidate_count <= 0 OR exact_strategy THEN' \
  'filtered ANN persisted-work assertion'
require_fixed tests/heavy/pgvector_hnsw_lifecycle.sh \
  'pgvector_hnsw_lifecycle_complete' \
  'dense HNSW lifecycle trace'
require_fixed docs/user_guide/indexes.md \
  'This is an intentional product and operations boundary' \
  'IVFFlat difference rationale'
require_fixed docs/user_guide/indexes.md \
  'keep existing pgvector IVFFlat indexes' \
  'IVFFlat migration guidance'
require_fixed docs/user_guide/security.md \
  '## PostgreSQL-Native Operational Boundary' \
  'PostgreSQL-native difference guidance'
require_fixed docs/user_guide/storage.md \
  'This is an intentional' \
  'rebuildable-artifact difference rationale'
require_fixed docs/user_guide/storage.md \
  '## Snapshot, Export, And Import Procedure' \
  'rebuildable-artifact migration guidance'

"${REPO_ROOT}/scripts/generate-sql-object-inventory.sh" --check
