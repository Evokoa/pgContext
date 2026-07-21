#!/usr/bin/env bash
# Recognized-dataset benchmark, all systems in matched Docker deployment.
#
# Brings up two containers -- PostgreSQL 17 with pgContext + pgvector, and
# Qdrant -- both reached by the client over a Docker-published localhost port,
# so the transport boundary is the same for every system (the fairness fix:
# native-PG-vs-Dockerized-Qdrant asymmetry is gone). Then runs bench_ann_hdf5.py
# against an ann-benchmarks HDF5 file using its precomputed ground truth.
#
# Smoke:  MAX_ROWS=20000 MAX_QUERIES=200 EF_VALUES=64 bash run-ann-hdf5.sh
# Full:   bash run-ann-hdf5.sh   (whole dataset, default ef sweep)
set -uo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
cd "${REPO_ROOT}"

HDF5="${HDF5:-target/ann-datasets/glove-100-angular.hdf5}"
OUT="${OUT:-target/ann-bench/glove-100-angular-$(date -u +%Y%m%dT%H%M%SZ 2>/dev/null || echo run).json}"
EF_VALUES="${EF_VALUES:-64,128,256,512}"
SYSTEMS="${SYSTEMS:-pgcontext,pgvector,qdrant}"
MAX_ROWS="${MAX_ROWS:-0}"
MAX_QUERIES="${MAX_QUERIES:-0}"
KEEP_UP="${KEEP_UP:-0}"

PG_IMAGE="${PG_IMAGE:-pgcontext-bench:local}"
QDRANT_IMAGE="${QDRANT_IMAGE:-qdrant/qdrant:v1.18.2}"
PG_NAME=pgc-bench-pg
QDRANT_NAME=pgc-bench-qdrant
PG_PORT="${PG_PORT:-5433}"
VENV="${REPO_ROOT}/target/pgvector-benchmark-venv/bin/python"

mkdir -p "$(dirname "${OUT}")"
LOG="$(dirname "${OUT}")/run.log"

say() { printf '%s\n' "$*" | tee -a "${LOG}"; }

cleanup() {
    if [[ "${KEEP_UP}" != "1" ]]; then
        docker rm -f "${PG_NAME}" "${QDRANT_NAME}" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT

say "ann-benchmarks HDF5 run"
say "  dataset  ${HDF5}"
say "  systems  ${SYSTEMS}   ef ${EF_VALUES}"
say "  smoke    max_rows=${MAX_ROWS:-full} max_queries=${MAX_QUERIES:-full}"
say "  output   ${OUT}"

[[ -f "${HDF5}" ]] || { say "missing dataset ${HDF5}"; exit 2; }
[[ -x "${VENV}" ]] || { say "missing benchmark venv"; exit 2; }
docker image inspect "${PG_IMAGE}" >/dev/null 2>&1 || { say "missing image ${PG_IMAGE}"; exit 2; }

say "starting containers"
docker rm -f "${PG_NAME}" "${QDRANT_NAME}" >/dev/null 2>&1 || true
docker run -d --name "${QDRANT_NAME}" -p 6333:6333 -p 6334:6334 "${QDRANT_IMAGE}" >/dev/null
docker run -d --name "${PG_NAME}" -e POSTGRES_PASSWORD=postgres \
    -p "${PG_PORT}:5432" --shm-size=6g "${PG_IMAGE}" >/dev/null

say "waiting for PostgreSQL"
for _ in $(seq 1 60); do
    docker exec "${PG_NAME}" pg_isready -U postgres >/dev/null 2>&1 && break
    sleep 2
done
docker exec "${PG_NAME}" pg_isready -U postgres >/dev/null 2>&1 || { say "PG never became ready"; docker logs "${PG_NAME}" 2>&1 | tail -20 | tee -a "${LOG}"; exit 3; }

say "waiting for Qdrant"
for _ in $(seq 1 60); do
    curl -fsS "http://localhost:6333/readyz" >/dev/null 2>&1 && break
    sleep 2
done

# Both extensions must install side by side; fail loudly here rather than
# mid-benchmark. Uses the coexist-transformed SQL baked into the image.
say "verifying extensions coexist"
if ! docker exec "${PG_NAME}" psql -U postgres -v ON_ERROR_STOP=1 \
    -c "CREATE EXTENSION IF NOT EXISTS vector" \
    -c "CREATE EXTENSION IF NOT EXISTS pgcontext" \
    -c "SELECT extname, extversion FROM pg_extension WHERE extname IN ('vector','pgcontext')" 2>&1 | tee -a "${LOG}" | grep -q pgcontext; then
    say "extension coexistence check FAILED"
    exit 4
fi

PG_DSN="host=localhost port=${PG_PORT} dbname=postgres user=postgres password=postgres"
say "running benchmark"
"${VENV}" benchmarks/pgvector_comparison/bench_ann_hdf5.py \
    --hdf5 "${HDF5}" --pg-dsn "${PG_DSN}" \
    --systems "${SYSTEMS}" --ef-values "${EF_VALUES}" \
    --max-rows "${MAX_ROWS}" --max-queries "${MAX_QUERIES}" \
    --output "${OUT}" 2>&1 | tee -a "${LOG}"
status=${PIPESTATUS[0]}

say ""
if [[ "${status}" -eq 0 && -f "${OUT}" ]]; then
    say "=== SUMMARY ==="
    "${VENV}" - "${OUT}" <<'PY' 2>&1 | tee -a "${LOG}"
import json, sys
r = json.load(open(sys.argv[1]))
print(f"{r['dataset']} | {r['metric']} | {r['corpus_rows']} x {r['dimensions']} | {r['queries']} queries")
for sysname, res in r["results"].items():
    if "error" in res:
        print(f"  {sysname}: ERROR {res['error']}"); continue
    seg = f" segments={res['segments_count']}" if "segments_count" in res else ""
    print(f"  {sysname}: build {res['build_s']:.1f}s{seg}")
    for p in res["curve"]:
        print(f"    ef={p['ef_search']:5d}  recall@10={p['recall_at_10']:.4f}  p50={p['p50_ms']:.3f}ms")
PY
else
    say "benchmark exited ${status} (see ${LOG})"
fi
say ""
say "output: ${OUT}"
say "log: ${LOG}"
