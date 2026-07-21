#!/usr/bin/env bash
# One-command overnight benchmark queue: runs every large/slow gate still
# open, in sequence, and
# keeps going past a failing lane so one bad lane doesn't waste the rest of
# the night. Everything is logged to one file plus a final PASS/FAIL
# summary; nothing is auto-committed — review target/overnight-queue-logs/
# and benchmarks/pgvector_comparison/results/ in the morning and commit
# what you want to keep.
#
# Usage: scripts/overnight-queue.sh
#
# Lanes, in order (churn and the 1M sweep run as one bench-overnight.sh
# invocation, since that script refuses a dirty tree on every call and its
# own archive step would dirty the tree for a second call; this lane also
# gets one automatic retry, since its long COPY-heavy load step observed a
# one-off dropped connection in testing that a plain rerun cleared):
#   1. Full churn (100k) + 1M high-ef Pareto sweep (needs Qdrant) ~75-100 min
#   2. Filtered sweep at 100k (needs Qdrant)                      ~5-10 min
#   3. CP10 build-parallelism sweep at 100k                       ~2-5 min
#   4. CP10 build-parallelism sweep at 1M                         ~15-30 min
#   5. Reduced regression gate                                    ~1-2 min
#
# Refuses to *start* from a dirty tree (same rule as bench-overnight.sh):
# archived evidence must record git_dirty=false. Note that lane 1's own
# archive step (writing into the git-tracked results/ directory) leaves the
# tree dirty for every lane after it — expected, not a bug. Each archived
# JSON records its own actual git_dirty state; anything stamped
# git_dirty=true just means "review before citing this one in docs," it did
# not fail to run.
#
# Every lane function below propagates failure explicitly (`|| return 1`
# after each critical step) rather than relying on `set -e`: this script
# deliberately does NOT set -e at the top level, since run_lane needs a
# failing lane to report FAIL and move on, not abort the whole run. Without
# the explicit checks, a failing step in the middle of a lane function would
# silently fall through to that function's last command (which usually
# succeeds trivially) and get reported as a false PASS.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

if [[ -n "$(git status --porcelain)" ]]; then
  echo "refusing to start: working tree is dirty; commit first so every" >&2
  echo "lane's archived evidence records git_dirty=false" >&2
  exit 1
fi

DSN="${PGCONTEXT_BENCH_DSN:-host=/tmp port=5432 dbname=postgres}"
SHA="$(git rev-parse --short HEAD)"
STAMP="$(date +%Y-%m-%d)"
RUN_STAMP="$(date +%Y-%m-%d_%H%M%S)"
RESULTS_DIR="benchmarks/pgvector_comparison/results"
LOG_DIR="target/overnight-queue-logs"
mkdir -p "${LOG_DIR}" "${RESULTS_DIR}"
LOG_FILE="${LOG_DIR}/overnight-${RUN_STAMP}.log"

# Everything printed from here on (by this script or anything it calls)
# goes to both the terminal and the log file exactly once. Lane functions
# below must NOT also redirect their own commands into LOG_FILE -- that
# would write those lines a second time.
exec > >(tee -a "${LOG_FILE}") 2>&1

if [[ -x /opt/homebrew/opt/postgresql@17/bin/psql ]]; then
  PSQL=/opt/homebrew/opt/postgresql@17/bin/psql
else
  PSQL=psql
fi

SUMMARY=()

log() {
  echo "[$(date '+%H:%M:%S')] $*"
}

run_lane() {
  local name="$1"
  shift
  log "=== START: ${name} ==="
  local start end elapsed status
  start=$(date +%s)
  if "$@"; then
    status="PASS"
  else
    status="FAIL (exit=$?)"
  fi
  end=$(date +%s)
  elapsed=$(( (end - start) / 60 ))
  log "=== END: ${name} -> ${status} (${elapsed} min) ==="
  SUMMARY+=("${name}: ${status} (${elapsed} min)")
}

# Like run_lane, but retries up to max_attempts times on failure before
# reporting FAIL. Only worth using for lanes whose failures are plausibly
# transient (a dropped connection, a flaky external dependency) rather than
# a deterministic bug -- retrying a deterministic failure just wastes the
# same amount of time twice for the same result.
run_lane_retry() {
  local name="$1"
  local max_attempts="$2"
  shift 2
  local attempt=1
  while (( attempt <= max_attempts )); do
    local label="${name}"
    (( max_attempts > 1 )) && label="${name} (attempt ${attempt}/${max_attempts})"
    log "=== START: ${label} ==="
    local start end elapsed status
    start=$(date +%s)
    if "$@"; then
      status="PASS"
    else
      status="FAIL (exit=$?)"
    fi
    end=$(date +%s)
    elapsed=$(( (end - start) / 60 ))
    log "=== END: ${label} -> ${status} (${elapsed} min) ==="
    if [[ "${status}" == "PASS" ]]; then
      SUMMARY+=("${name}: ${status} (${elapsed} min, attempt ${attempt})")
      return 0
    fi
    if (( attempt == max_attempts )); then
      SUMMARY+=("${name}: ${status} (${elapsed} min, all ${max_attempts} attempts failed)")
      return 1
    fi
    log "retrying ${name} (a failure here is often a transient connection drop, not a deterministic bug)"
    attempt=$(( attempt + 1 ))
  done
}

ensure_qdrant() {
  if ! command -v docker >/dev/null 2>&1; then
    log "docker not found; skipping Qdrant startup (sweep-1m and filtered-sweep will fail)"
    return
  fi
  if docker ps --format '{{.Names}}' 2>/dev/null | grep -qx pgcontext-bench-qdrant; then
    log "Qdrant already running"
    return
  fi
  log "starting Qdrant..."
  docker run -d --rm --name pgcontext-bench-qdrant \
    -p 6333:6333 -p 6334:6334 qdrant/qdrant:v1.18.2
  sleep 5
}

lane_filtered_sweep_100k() {
  local dir="target/pgvector-comparison-100k"
  if [[ ! -f "${dir}/corpus.npy" ]]; then
    benchmarks/pgvector_comparison/run.sh prepare --synthetic 100000 --output-dir "${dir}" || return 1
  fi
  benchmarks/pgvector_comparison/run.sh filtered-sweep \
    --output-dir "${dir}" --dsn "${DSN}" --queries 200 --warmup 20 || return 1
  cp "${dir}/filtered-sweep.json" \
    "${RESULTS_DIR}/apple-m4-pro-pg17.9-${STAMP}-filtered-sweep-100k-${SHA}.json" || return 1
  echo "archived ${RESULTS_DIR}/apple-m4-pro-pg17.9-${STAMP}-filtered-sweep-100k-${SHA}.json"
}

# CP10 build-parallelism sweep: not part of the Python comparison harness
# (it's a pgContext-only build-time measurement, see
# docs/contributor_guide/build_profile_2026-07.md), so this drives it
# directly over psql, matching the manual verification run that produced
# the 20k-row numbers already in that doc. Reuses one loaded table per
# scale and just drops/rebuilds the index per worker count, which is what
# makes this lane fast relative to the corpus-generation cost.
#
# maintenance_work_mem must be raised for the CP10 sweep sessions: the
# 20k-row manual verification run never needed to touch it (well under the
# 64MB session default), so this went untested at 100k/1M until the first
# real overnight run, where the default budget failed partway through a
# 100k-row build. Sized generously (not tightly computed) so a slightly
# larger corpus doesn't reopen the same failure mode.
cp10_build_parallelism_sweep() {
  local rows="$1"
  local label="$2"
  local work_mem_mb="$3"
  local dbname="pgcontext_bench_cp10_${label}"
  log "--- CP10 build-parallelism sweep: ${rows} rows (${label}), maintenance_work_mem=${work_mem_mb}MB ---"

  "${PSQL}" -d postgres -v ON_ERROR_STOP=1 \
    -c "DROP DATABASE IF EXISTS ${dbname};" \
    -c "CREATE DATABASE ${dbname};" || return 1

  "${PSQL}" -d "${dbname}" -v ON_ERROR_STOP=1 <<SQL || return 1
CREATE EXTENSION pgcontext;
CREATE TABLE build_probe (id bigint PRIMARY KEY, embedding vector(384) NOT NULL);
INSERT INTO build_probe
SELECT n,
       (SELECT '[' || string_agg((((n * 2654435761 + d * 40503) % 1000))::text, ',') || ']'
          FROM generate_series(1, 384) d)::vector
  FROM generate_series(1, ${rows}) n;
SQL

  local results_file="${RESULTS_DIR}/apple-m4-pro-pg17.9-${STAMP}-cp10-build-parallelism-${label}-${SHA}.json"
  local entries=()
  local workers timing_output psql_status ms
  for workers in 1 2 4 8; do
    timing_output=$("${PSQL}" -d "${dbname}" -v ON_ERROR_STOP=1 <<SQL
\timing on
SET maintenance_work_mem = '${work_mem_mb}MB';
SET pgcontext.hnsw_build_parallel_workers = ${workers};
CREATE INDEX build_probe_hnsw ON build_probe USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops);
DROP INDEX build_probe_hnsw;
SQL
)
    psql_status=$?
    echo "${timing_output}"
    if [[ ${psql_status} -ne 0 ]]; then
      log "workers=${workers}: FAILED (psql exit=${psql_status})"
      "${PSQL}" -d postgres -c "DROP DATABASE IF EXISTS ${dbname};"
      return 1
    fi
    ms=$(echo "${timing_output}" | grep -A1 "CREATE INDEX" | grep -oE "Time: [0-9.]+ ms" | grep -oE "[0-9.]+" | head -1)
    if [[ -z "${ms}" ]]; then
      log "workers=${workers}: FAILED (could not parse CREATE INDEX timing from psql output)"
      "${PSQL}" -d postgres -c "DROP DATABASE IF EXISTS ${dbname};"
      return 1
    fi
    log "workers=${workers}: ${ms} ms"
    entries+=("    \"${workers}\": {\"milliseconds\": ${ms}}")
  done
  "${PSQL}" -d postgres -c "DROP DATABASE IF EXISTS ${dbname};"

  local dirty=false
  [[ -n "$(git status --porcelain)" ]] && dirty=true
  {
    echo "{"
    echo "  \"lane\": \"cp10-build-parallelism\","
    echo "  \"corpus_rows\": ${rows},"
    echo "  \"dimensions\": 384,"
    echo "  \"maintenance_work_mem_mb\": ${work_mem_mb},"
    echo "  \"git_sha\": \"${SHA}\","
    echo "  \"git_dirty\": ${dirty},"
    echo "  \"date\": \"${STAMP}\","
    echo "  \"workers\": {"
    local IFS=$'\n'
    echo "${entries[*]}" | sed '$!s/$/,/'
    echo "  }"
    echo "}"
  } > "${results_file}"
  echo "archived ${results_file}"
}

log "overnight queue starting; commit=${SHA}; full log at ${LOG_FILE}"

ensure_qdrant

# bench-overnight.sh refuses to run at all from a dirty tree, and its own
# archive step (copying JSON into benchmarks/pgvector_comparison/results/)
# dirties the tree — so churn and sweep-1m must run as ONE invocation
# (`all`), not two. A second, separate invocation would see the first
# invocation's archived file as an uncommitted change and refuse to start.
run_lane_retry "full churn + 1M high-ef sweep" 2 scripts/bench-overnight.sh all
run_lane "filtered sweep (100k)" lane_filtered_sweep_100k
run_lane "CP10 build-parallelism sweep (100k)" cp10_build_parallelism_sweep 100000 100k 512
run_lane "CP10 build-parallelism sweep (1M)" cp10_build_parallelism_sweep 1000000 1m 4096
run_lane "reduced regression gate" scripts/bench-regression.sh

log "=== SUMMARY ==="
for line in "${SUMMARY[@]}"; do
  log "  ${line}"
done
log "full log: ${LOG_FILE}"
log "archived JSONs (if any): ${RESULTS_DIR}"
log "note: lane 1's archive step leaves the tree dirty for every lane after"
log "  it, by design (see script header) -- check each JSON's own git_dirty"
log "  field, don't assume true means that lane failed"
log "nothing was committed automatically; review and commit what you want to keep"

for line in "${SUMMARY[@]}"; do
  if [[ "${line}" == *"FAIL"* ]]; then
    exit 1
  fi
done
exit 0
