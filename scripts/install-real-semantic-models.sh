#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
runtime_root="${PGCONTEXT_SEMANTIC_MODEL_DIR:-${repository_root}/target/real-semantic-models}"
venv="${runtime_root}/venv"
requirements="${repository_root}/tools/real-semantic-models/requirements.txt"
adapter="${repository_root}/tools/real-semantic-models/semantic_models.py"

command -v uv >/dev/null || {
  echo "uv is required to install the optional real-model runtime" >&2
  exit 1
}

mkdir -p "${runtime_root}"
uv venv --python 3.12 "${venv}"

package_command=(uv pip install --python "${venv}/bin/python" --requirement "${requirements}")
if [[ -n "${PGCONTEXT_PACKAGE_MANAGER_WRAPPER:-}" ]]; then
  "${PGCONTEXT_PACKAGE_MANAGER_WRAPPER}" "${package_command[@]}"
else
  "${package_command[@]}"
fi

"${venv}/bin/python" "${adapter}" --root "${runtime_root}" download
