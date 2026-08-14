#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "${repository_root}"

adapter="tools/real-semantic-models/semantic_models.py"
requirements="tools/real-semantic-models/requirements.txt"

grep -q 'c5ee24cb16019beea0893ab7796b1df96625c6b8' "${adapter}"
grep -q 'ea78891063587eb050ed4166b20062eaf978037c' "${adapter}"
grep -q 'model_qint8_arm64.onnx' "${adapter}"
grep -q 'operator_downloaded_only' "${adapter}"
grep -q 'validate_model_identity' "${adapter}"
grep -q '^onnxruntime==' "${requirements}"

if git ls-files | grep -Eq '\.(onnx|safetensors|npy|npz)$'; then
  echo "real-model source path must not track model weights or generated arrays" >&2
  exit 1
fi

for script in scripts/install-real-semantic-models.sh \
              scripts/run-real-semantic-model-smoke.sh \
              scripts/run-real-semantic-postgres-smoke.sh; do
  [[ -x "${script}" ]] || {
    echo "real-model script is not executable: ${script}" >&2
    exit 1
  }
done

echo "real semantic model source smoke passed"
