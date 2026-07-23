#!/usr/bin/env bash
# 1M recall/build diagnostics: decide why pgContext's 1M graph quality trails
# Qdrant's (0.574 vs 0.955 recall at ~4.5ms) before committing to a fix.
#
# Three arms over one shared corpus, pgContext only — the comparison is against
# our own builds, so reloading pgvector on every arm would double the runtime
# and answer nothing:
#
#   a  baseline    8 build workers, ef_construction=64
#      Reproduces the archived 1M figures at HEAD. Not redundant: the archived
#      numbers predate the read-path changes in this release, so without this
#      arm a difference in b or c could be those changes rather than the knob.
#   b  serial      1 build worker, ef_construction=64
#      Does the per-node-locking parallel builder produce a worse graph?
#   c  high effort 8 build workers, ef_construction=200
#      Is construction effort simply too low for 1M at the matched setting?
#
# Reading the result, against arm a's recall:
#   b improves        -> parallel build degrades graph quality; fix concurrent
#                        pruning, no architecture change needed.
#   c improves        -> recall is purchasable with build time, which makes
#                        build throughput the binding constraint.
#   neither improves  -> the monolithic graph is the limit; segmentation is the
#                        recall mechanism, not just a write-path feature.
#
# Sweeps at ef_search 128/256/384 to match the archived 1M sweep, so the
# recall/latency curves compare directly against docs/benchmarks/pgvector.md.
#
# Arms are selectable and resumable (ARMS=, FORCE=), so a failure in hour two
# does not cost the arms that already succeeded.
set -euo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "${REPO_ROOT}"

CORPUS_ROWS="${CORPUS_ROWS:-1000000}"
EF_VALUES="${EF_VALUES:-128,256,384}"
OUT_DIR="${OUT_DIR:-target/bench-1m-diagnostics}"
LOG_DIR="${LOG_DIR:-${OUT_DIR}/logs}"
ARMS="${ARMS:-a,b,c}"
# pgcontext.hnsw_build_parallel_workers defaults to 1, so "leave it unset" is
# a serial build -- which would make the baseline arm identical to the serial
# arm and test nothing. Both parallel arms therefore set this explicitly, and
# 8 is the count the archived 181s baseline was measured at.
BUILD_WORKERS="${BUILD_WORKERS:-8}"
FORCE="${FORCE:-0}"
MIN_FREE_GB="${MIN_FREE_GB:-15}"
SKIP_INSTALL="${SKIP_INSTALL:-0}"

# Homebrew PostgreSQL 17 is where the archived comparison runs were measured.
export PGCONTEXT_BENCH_DSN="${PGCONTEXT_BENCH_DSN:-host=localhost port=5432 dbname=postgres}"
# The archived parallel-build run used 4GB. pgContext refuses builds whose
# estimated memory exceeds this rather than degrading, so it must be explicit.
export PGCONTEXT_BENCH_MAINTENANCE_WORK_MEM="${PGCONTEXT_BENCH_MAINTENANCE_WORK_MEM:-4GB}"
export PGCONTEXT_BENCH_SYSTEMS=pgcontext

mkdir -p "${LOG_DIR}"
STAMP="$(date -u +%Y-%m-%dT%H%M%SZ)"
SUMMARY="${OUT_DIR}/summary-${STAMP}.txt"

note() { printf '%s\n' "$*" | tee -a "${SUMMARY}"; }

note "1M recall/build diagnostics"
note "  commit    $(git rev-parse --short HEAD)"
note "  corpus    ${CORPUS_ROWS} x 384"
note "  ef_search ${EF_VALUES}"
note "  arms      ${ARMS}"
note "  dsn       ${PGCONTEXT_BENCH_DSN}"
note "  mwm       ${PGCONTEXT_BENCH_MAINTENANCE_WORK_MEM} (build budget)"

admin_dsn_postgres() {
    printf '%s' "${PGCONTEXT_BENCH_DSN}" | sed 's/dbname=[^ ]*/dbname=postgres/'
}

if ! psql "$(admin_dsn_postgres)" -tAc "SELECT 1" >/dev/null 2>&1; then
    echo "cannot reach ${PGCONTEXT_BENCH_DSN}" >&2
    exit 2
fi

free_gb=$(df -g "${REPO_ROOT}" | awk 'NR==2 {print $4}')
if (( free_gb < MIN_FREE_GB )); then
    echo "only ${free_gb}GB free, need ${MIN_FREE_GB}GB for ${CORPUS_ROWS} rows" >&2
    exit 2
fi
note "  disk      ${free_gb}GB free"
note ""

if [[ "${SKIP_INSTALL}" != "1" ]]; then
    # The extension under test must be this tree, not whatever was installed
    # last. Release profile: a debug build makes every timing meaningless.
    note "installing pgcontext (release) into the benchmark PostgreSQL"
    BREW_PG_CONFIG="${BREW_PG_CONFIG:-/opt/homebrew/opt/postgresql@17/bin/pg_config}"
    cargo pgrx install --release -p context-pg --pg-config "${BREW_PG_CONFIG}" \
        >"${LOG_DIR}/install.log" 2>&1
    # cargo-pgrx writes the raw generated SQL; the checked-in artifact carries
    # the canonical fixed-schema artifact, and this database has pgvector installed.
    cp sql/pgcontext--0.2.0.sql \
       "$("${BREW_PG_CONFIG}" --sharedir)/extension/pgcontext--0.2.0.sql"
    note "  installed"
    note ""
fi

if [[ ! -f "${OUT_DIR}/corpus.npy" ]]; then
    note "preparing ${CORPUS_ROWS}-row corpus (one time, shared by all arms)"
    benchmarks/pgvector_comparison/run.sh prepare \
        --synthetic "${CORPUS_ROWS}" --output-dir "${OUT_DIR}" \
        >"${LOG_DIR}/prepare.log" 2>&1
    note "  done"
else
    note "reusing corpus at ${OUT_DIR}/corpus.npy"
fi
note ""

summarize_arm() {
    local arm_dir="$1" label="$2"
    target/pgvector-benchmark-venv/bin/python - "${arm_dir}/sweep.json" "${label}" \
        <<'PY' | tee -a "${SUMMARY}"
import json, sys
report = json.load(open(sys.argv[1]))
label = sys.argv[2]
config = report.get("configuration", {})
print(f"  ef_construction={config.get('hnsw_ef_construction')} "
      f"workers={config.get('pgcontext_build_parallel_workers')}")
for system, curve in report.get("curves", {}).items():
    for point in curve:
        p50 = point.get("p50_ms")
        recall = point.get("recall_at_10")
        if p50 is None or recall is None:
            continue
        print(f"  {label} ef={point.get('ef_search')}: "
              f"{p50:.2f} ms @ recall {recall:.4f}")
PY
}

run_arm() {
    local key="$1" label="$2" ef_construction="$3" workers="$4"
    local arm_dir="${OUT_DIR}/${label}"

    if [[ ",${ARMS}," != *",${key},"* ]]; then
        note "arm ${label}: skipped (not in ARMS=${ARMS})"
        note ""
        return 0
    fi

    mkdir -p "${arm_dir}"
    if [[ -f "${arm_dir}/sweep.json" && "${FORCE}" != "1" ]]; then
        note "arm ${label}: already complete, reusing (FORCE=1 to redo)"
        summarize_arm "${arm_dir}" "${label}"
        note ""
        return 0
    fi

    # Hard-link rather than copy: three arms should not cost three corpora.
    local artifact
    for artifact in corpus.npy queries.npy dataset.json; do
        [[ -f "${arm_dir}/${artifact}" ]] \
            || ln "${OUT_DIR}/${artifact}" "${arm_dir}/${artifact}"
    done

    note "arm ${label}: ef_construction=${ef_construction} workers=${workers:-default}"
    local started
    started=$(date +%s)
    if ! PGCONTEXT_BENCH_EF_CONSTRUCTION="${ef_construction}" \
         PGCONTEXT_BENCH_BUILD_WORKERS="${workers}" \
         benchmarks/pgvector_comparison/run.sh sweep \
             --ef-values "${EF_VALUES}" --output-dir "${arm_dir}" \
             >"${LOG_DIR}/${label}.log" 2>&1
    then
        # Never abort the whole run for one arm: the others are independent
        # and their results are still worth having in the morning.
        note "  FAILED after $(( $(date +%s) - started ))s — ${LOG_DIR}/${label}.log"
        note "  last lines:"
        tail -5 "${LOG_DIR}/${label}.log" | sed 's/^/    /' | tee -a "${SUMMARY}"
        note ""
        return 0
    fi
    note "  completed in $(( $(date +%s) - started ))s"
    grep -oE "build_seconds[^,]*" "${LOG_DIR}/${label}.log" | head -1 \
        | sed 's/^/  /' | tee -a "${SUMMARY}" || true
    summarize_arm "${arm_dir}" "${label}"
    note ""
}

run_arm a "a-baseline-parallel-ef64"  64  "${BUILD_WORKERS}"
run_arm b "b-serial-ef64"             64  "1"
run_arm c "c-parallel-ef200"         200  "${BUILD_WORKERS}"

note "raw reports under ${OUT_DIR}/<arm>/sweep.json"
note "build logs under ${LOG_DIR}"
note ""
note "Compare each arm's recall against arm a at matched ef_search:"
note "  b higher  -> parallel builder degrades graph quality"
note "  c higher  -> construction effort is the lever; build throughput binds"
note "  neither   -> monolithic graph is the limit; segmentation is the fix"
note ""
note "Summary: ${SUMMARY}"
