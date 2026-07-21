#!/usr/bin/env bash
# Serial overnight benchmark lanes with automatic result archiving.
#
# Usage:
#   scripts/bench-overnight.sh churn        # full churn lane, 100k corpus
#   scripts/bench-overnight.sh churn-quick  # reduced churn (2 rounds, 1%)
#   scripts/bench-overnight.sh sweep-1m     # high-ef Pareto sweep, 1M corpus
#   scripts/bench-overnight.sh all          # churn then sweep-1m
#
# Rules enforced here:
#   - refuses to run from a dirty tree (published evidence must be clean);
#   - lanes run strictly serially (the harness uses fixed database and
#     collection names, so concurrent invocations corrupt each other);
#   - every produced JSON is archived under
#     benchmarks/pgvector_comparison/results/ stamped with date and SHA.
#
# The sweep-1m lane needs the pinned Qdrant service running first:
#   docker run -d --rm --name pgcontext-bench-qdrant \
#     -p 6333:6333 -p 6334:6334 qdrant/qdrant:v1.18.2
# churn lanes are PostgreSQL-only and do not need Qdrant.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

if [[ -n "$(git status --porcelain)" ]]; then
  echo "refusing to run: working tree is dirty; commit first so the archived" >&2
  echo "evidence records git_dirty=false" >&2
  exit 1
fi

DSN="${PGCONTEXT_BENCH_DSN:-host=/tmp port=5432 dbname=postgres}"
SHA="$(git rev-parse --short HEAD)"
STAMP="$(date +%Y-%m-%d)"
RESULTS_DIR="benchmarks/pgvector_comparison/results"
DIR_100K="target/pgvector-comparison-100k"
DIR_1M="target/pgvector-comparison-1m"

archive() {
  local produced="$1" label="$2"
  local destination="${RESULTS_DIR}/apple-m4-pro-pg17.9-${STAMP}-${label}-${SHA}.json"
  cp "${produced}" "${destination}"
  echo "archived ${destination}"
}

ensure_corpus() {
  local directory="$1" rows="$2"
  if [[ ! -f "${directory}/corpus.npy" ]]; then
    benchmarks/pgvector_comparison/run.sh prepare \
      --synthetic "${rows}" --output-dir "${directory}"
  fi
}

lane_churn() {
  local rounds="$1" percent="$2" label="$3"
  ensure_corpus "${DIR_100K}" 100000
  PGCONTEXT_HNSW_EF_SEARCH=48 PGVECTOR_HNSW_EF_SEARCH=40 \
    benchmarks/pgvector_comparison/run.sh churn \
    --output-dir "${DIR_100K}" --dsn "${DSN}" \
    --queries 200 --warmup 20 --rounds "${rounds}" --churn-percent "${percent}"
  archive "${DIR_100K}/churn.json" "${label}"
}

lane_sweep_1m() {
  ensure_corpus "${DIR_1M}" 1000000
  benchmarks/pgvector_comparison/run.sh sweep \
    --output-dir "${DIR_1M}" --dsn "${DSN}" \
    --queries 200 --warmup 20 --ef-values 128,256,384
  archive "${DIR_1M}/sweep.json" "sweep-1m"
}

case "${1:-all}" in
  churn) lane_churn 5 5 "churn-100k" ;;
  churn-quick) lane_churn 2 1 "churn-quick-100k" ;;
  sweep-1m) lane_sweep_1m ;;
  all)
    lane_churn 5 5 "churn-100k"
    lane_sweep_1m
    ;;
  *)
    echo "unknown lane: $1 (expected churn, churn-quick, sweep-1m, or all)" >&2
    exit 1
    ;;
esac
echo "done; remember to commit the archived JSONs"
