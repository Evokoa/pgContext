#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
VENV="${REPO_ROOT}/target/pgvector-benchmark-venv"
cd "${REPO_ROOT}"

if [[ ! -x "${VENV}/bin/python" ]]; then
  sfw uv venv --python 3.12 "${VENV}"
fi

if ! "${VENV}/bin/python" -c 'import certifi, fastembed, psycopg, qdrant_client' >/dev/null 2>&1; then
  sfw uv pip install --python "${VENV}/bin/python" \
    'fastembed==0.7.3' \
    'psycopg[binary]==3.2.9' \
    'qdrant-client==1.18.0' \
    'certifi==2025.8.3'
fi

if [[ "${1:-}" == "test" ]]; then
  shift
  exec "${VENV}/bin/python" -m unittest discover \
    -s benchmarks/pgvector_comparison -p 'test_*.py' "$@"
fi

exec "${VENV}/bin/python" benchmarks/pgvector_comparison/benchmark.py "$@"
