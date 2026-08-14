#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
runtime_root="${PGCONTEXT_SEMANTIC_MODEL_DIR:-${repository_root}/target/real-semantic-models}"
python="${runtime_root}/venv/bin/python"
adapter="${repository_root}/tools/real-semantic-models/semantic_models.py"
report="${PGCONTEXT_REAL_MODEL_REPORT:-${runtime_root}/real-model-smoke.json}"

[[ -x "${python}" ]] || {
  echo "optional runtime is missing; run scripts/install-real-semantic-models.sh" >&2
  exit 1
}

mkdir -p "$(dirname "${report}")"
"${python}" "${adapter}" --root "${runtime_root}" smoke >"${report}"
"${python}" -m json.tool "${report}"
