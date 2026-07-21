#!/usr/bin/env bash
# Reduced-lane performance regression gate.
#
# Runs the SciFact comparison against pgvector only (Qdrant skipped) and
# fails when pgContext regresses on machine-independent ratios:
#   - ANN recall@10 must stay >= 0.985 at ef_search=48;
#   - pgContext ANN p50 must stay <= 1.10x pgvector's at these settings;
#   - the filtered lane must keep a 100% full-result rate.
#
# Usage: scripts/bench-regression.sh [--dsn "<libpq dsn>"]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

DSN="host=/tmp port=5432 dbname=postgres"
if [[ "${1:-}" == "--dsn" ]]; then
  DSN="$2"
fi

OUTPUT_DIR="target/pgvector-comparison"
if [[ ! -f "${OUTPUT_DIR}/corpus.npy" ]]; then
  benchmarks/pgvector_comparison/run.sh prepare --output-dir "${OUTPUT_DIR}"
fi

PGCONTEXT_BENCH_SKIP_QDRANT=1 \
PGCONTEXT_HNSW_EF_SEARCH=48 \
PGVECTOR_HNSW_EF_SEARCH=40 \
benchmarks/pgvector_comparison/run.sh run \
  --output-dir "${OUTPUT_DIR}" \
  --dsn "${DSN}" \
  --queries 100 --warmup 20 --trials 1 >/dev/null

"${REPO_ROOT}/target/pgvector-benchmark-venv/bin/python" - <<'EOF'
import json
import sys

report = json.load(open("target/pgvector-comparison/results.json"))
pgcontext = report["systems"]["pgcontext"]
pgvector = report["systems"]["pgvector"]

failures = []
recall = pgcontext["ann"]["recall_at_10"]
if recall < 0.985:
    failures.append(f"pgContext ANN recall@10 {recall:.4f} < 0.985")
ratio = pgcontext["ann"]["p50_ms"] / pgvector["ann"]["p50_ms"]
if ratio > 1.10:
    failures.append(
        f"pgContext ANN p50 is {ratio:.2f}x pgvector "
        f"({pgcontext['ann']['p50_ms']:.4f}ms vs {pgvector['ann']['p50_ms']:.4f}ms)"
    )
full_rate = pgcontext["filtered_ann_10_percent"]["full_result_rate"]
if full_rate < 1.0:
    failures.append(f"pgContext filtered full-result rate {full_rate:.2f} < 1.0")

if failures:
    print("bench regression gate FAILED:")
    for failure in failures:
        print(f"  - {failure}")
    sys.exit(1)
print(
    "bench regression gate passed: "
    f"recall={recall:.4f}, p50 ratio={ratio:.2f}, full-result rate={full_rate:.2f}"
)
EOF
