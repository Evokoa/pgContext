#!/usr/bin/env python3
"""Optional real-model adapter and smoke certification for pgContext.

Model weights are downloaded only by the explicit ``download`` command into an
ignored target directory. They are never part of the source or release payload.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import resource
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

os.environ.setdefault("TRANSFORMERS_NO_ADVISORY_WARNINGS", "1")

import numpy as np
import onnxruntime as ort
from huggingface_hub import snapshot_download
from transformers import AutoTokenizer


RERANK_REPOSITORY = "cross-encoder/ms-marco-MiniLM-L6-v2"
RERANK_REVISION = "c5ee24cb16019beea0893ab7796b1df96625c6b8"
EMBED_REPOSITORY = "sentence-transformers/all-MiniLM-L6-v2"
EMBED_REVISION = "ea78891063587eb050ed4166b20062eaf978037c"
LICENSE_SPDX = "Apache-2.0"
TOKENIZER_FILES = (
    "config.json",
    "special_tokens_map.json",
    "tokenizer.json",
    "tokenizer_config.json",
    "vocab.txt",
)
MAX_CANDIDATES = 512
MAX_QUERY_BYTES = 64 * 1024
MAX_TEXT_BYTES = 32 * 1024


@dataclass(frozen=True)
class ModelSpec:
    name: str
    repository: str
    revision: str
    maximum_tokens: int
    wire_revision: int

    def onnx_file(self) -> str:
        machine = platform.machine().lower()
        if machine in {"arm64", "aarch64"}:
            return "onnx/model_qint8_arm64.onnx"
        return "onnx/model.onnx"


RERANK_SPEC = ModelSpec("reranker", RERANK_REPOSITORY, RERANK_REVISION, 512, 1)
EMBED_SPEC = ModelSpec("embedder", EMBED_REPOSITORY, EMBED_REVISION, 256, 1)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def install_root(argument: str | None) -> Path:
    raw = argument or os.environ.get(
        "PGCONTEXT_SEMANTIC_MODEL_DIR", "target/real-semantic-models"
    )
    return Path(raw).resolve()


def download_one(spec: ModelSpec, root: Path) -> dict[str, Any]:
    cache = root / "hub"
    patterns = [*TOKENIZER_FILES, spec.onnx_file(), "LICENSE", "README.md"]
    snapshot = Path(
        snapshot_download(
            repo_id=spec.repository,
            revision=spec.revision,
            cache_dir=cache,
            allow_patterns=patterns,
        )
    )
    model = snapshot / spec.onnx_file()
    if not model.is_file():
        raise RuntimeError(f"download did not produce {spec.onnx_file()}")
    return {
        "name": spec.name,
        "repository": spec.repository,
        "revision": spec.revision,
        "wire_model": spec.repository,
        "wire_model_revision": spec.wire_revision,
        "license_spdx": LICENSE_SPDX,
        "snapshot": str(snapshot),
        "onnx_file": spec.onnx_file(),
        "artifact_bytes": model.stat().st_size,
        "artifact_sha256": sha256_file(model),
        "maximum_tokens": spec.maximum_tokens,
    }


def download_models(root: Path) -> dict[str, Any]:
    root.mkdir(parents=True, exist_ok=True)
    models = [download_one(RERANK_SPEC, root), download_one(EMBED_SPEC, root)]
    manifest = {
        "schema_version": 1,
        "distribution": "operator_downloaded_only",
        "models": models,
    }
    manifest_path = root / "installed-models.json"
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    return manifest


def load_manifest(root: Path) -> dict[str, Any]:
    path = root / "installed-models.json"
    if not path.is_file():
        raise RuntimeError("models are not installed; run the download command first")
    manifest = json.loads(path.read_text())
    if manifest.get("schema_version") != 1:
        raise RuntimeError("unsupported installed-model manifest")
    for model in manifest.get("models", []):
        artifact = Path(model["snapshot"]) / model["onnx_file"]
        if not artifact.is_file() or sha256_file(artifact) != model["artifact_sha256"]:
            raise RuntimeError(f"model artifact digest mismatch: {model.get('name')}")
    return manifest


def model_entry(manifest: dict[str, Any], name: str) -> dict[str, Any]:
    for entry in manifest["models"]:
        if entry["name"] == name:
            return entry
    raise RuntimeError(f"installed-model manifest has no {name}")


class OnnxTextModel:
    def __init__(self, entry: dict[str, Any]) -> None:
        snapshot = Path(entry["snapshot"])
        self.tokenizer = AutoTokenizer.from_pretrained(snapshot, local_files_only=True)
        options = ort.SessionOptions()
        options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
        self.session = ort.InferenceSession(
            str(snapshot / entry["onnx_file"]),
            sess_options=options,
            providers=["CPUExecutionProvider"],
        )
        self.maximum_tokens = int(entry["maximum_tokens"])
        self.input_names = {item.name for item in self.session.get_inputs()}

    def inputs(self, first: list[str], second: list[str] | None = None) -> dict[str, Any]:
        encoded = self.tokenizer(
            first,
            second,
            padding=True,
            truncation=True,
            max_length=self.maximum_tokens,
            return_tensors="np",
        )
        return {
            name: np.asarray(value, dtype=np.int64)
            for name, value in encoded.items()
            if name in self.input_names
        }


def stable_sigmoid(value: float) -> float:
    if value >= 0:
        return 1.0 / (1.0 + math.exp(-value))
    exponential = math.exp(value)
    return exponential / (1.0 + exponential)


class Reranker(OnnxTextModel):
    def score(self, query: str, documents: list[str]) -> list[float]:
        outputs = self.session.run(None, self.inputs([query] * len(documents), documents))
        logits = np.asarray(outputs[0], dtype=np.float64).reshape(-1)
        if logits.size != len(documents):
            raise RuntimeError("reranker returned an unexpected output shape")
        return [stable_sigmoid(float(value)) for value in logits]


class Embedder(OnnxTextModel):
    def encode(self, texts: list[str]) -> np.ndarray:
        inputs = self.inputs(texts)
        outputs = self.session.run(None, inputs)
        names = [item.name for item in self.session.get_outputs()]
        sentence = next(
            (np.asarray(value) for name, value in zip(names, outputs) if name == "sentence_embedding"),
            None,
        )
        if sentence is None:
            tokens = np.asarray(outputs[0], dtype=np.float32)
            if tokens.ndim != 3:
                raise RuntimeError("embedder returned an unexpected output shape")
            mask = np.asarray(inputs["attention_mask"], dtype=np.float32)[..., None]
            sentence = (tokens * mask).sum(axis=1) / np.maximum(mask.sum(axis=1), 1.0)
        sentence = np.asarray(sentence, dtype=np.float32)
        norms = np.linalg.norm(sentence, axis=1, keepdims=True)
        if np.any(norms == 0) or not np.all(np.isfinite(sentence)):
            raise RuntimeError("embedder returned an invalid vector")
        return sentence / norms


def validate_request(request: dict[str, Any]) -> None:
    if request.get("version") != 3 or int(request.get("request_id", 0)) <= 0:
        raise ValueError("invalid rerank request identity")
    query = request.get("query")
    candidates = request.get("candidates")
    if not isinstance(query, str) or len(query.encode()) > MAX_QUERY_BYTES:
        raise ValueError("rerank query is invalid or oversized")
    if not isinstance(candidates, list) or not 1 <= len(candidates) <= MAX_CANDIDATES:
        raise ValueError("rerank candidates are invalid or oversized")
    occurrences: set[int] = set()
    for candidate in candidates:
        occurrence = int(candidate.get("occurrence_id", 0))
        text = candidate.get("text")
        if occurrence <= 0 or occurrence in occurrences:
            raise ValueError("rerank occurrence identity is invalid or duplicated")
        if not isinstance(text, str) or len(text.encode()) > MAX_TEXT_BYTES:
            raise ValueError("rerank candidate text is invalid or oversized")
        occurrences.add(occurrence)


def validate_model_identity(request: dict[str, Any], entry: dict[str, Any]) -> None:
    if (
        request.get("model") != entry["wire_model"]
        or request.get("model_revision") != entry["wire_model_revision"]
    ):
        raise ValueError("rerank model identity does not match the installed artifact")


def rerank_request(root: Path, request: dict[str, Any]) -> dict[str, Any]:
    validate_request(request)
    entry = model_entry(load_manifest(root), "reranker")
    validate_model_identity(request, entry)
    reranker = Reranker(entry)
    scores = reranker.score(request["query"], [item["text"] for item in request["candidates"]])
    ranked = sorted(
        zip(request["candidates"], scores),
        key=lambda item: (-item[1], int(item[0]["occurrence_id"])),
    )
    return {
        "version": request["version"],
        "request_id": request["request_id"],
        "model": entry["wire_model"],
        "model_revision": entry["wire_model_revision"],
        "scores": [
            {"occurrence_id": item["occurrence_id"], "score": score}
            for item, score in ranked
        ],
    }


def embed_chunks(root: Path, payload: dict[str, Any]) -> dict[str, Any]:
    chunks = payload.get("chunks")
    if not isinstance(chunks, list) or not 1 <= len(chunks) <= MAX_CANDIDATES:
        raise ValueError("embedding chunks are invalid or oversized")
    texts = [chunk.get("text") for chunk in chunks]
    if any(not isinstance(text, str) or len(text.encode()) > MAX_TEXT_BYTES for text in texts):
        raise ValueError("embedding text is invalid or oversized")
    entry = model_entry(load_manifest(root), "embedder")
    vectors = Embedder(entry).encode(texts)
    return {
        "model": entry["repository"],
        "revision": entry["revision"],
        "artifact_sha256": entry["artifact_sha256"],
        "dimensions": int(vectors.shape[1]),
        "chunks": [
            {
                "occurrence_id": chunk.get("occurrence_id"),
                "citation": chunk.get("citation"),
                "embedding": vector.tolist(),
            }
            for chunk, vector in zip(chunks, vectors)
        ],
    }


def smoke(root: Path) -> dict[str, Any]:
    manifest = load_manifest(root)
    started = time.perf_counter()
    reranker = Reranker(model_entry(manifest, "reranker"))
    embedder = Embedder(model_entry(manifest, "embedder"))
    cold_millis = (time.perf_counter() - started) * 1000.0
    query = "Which planet is known as the Red Planet?"
    documents = [
        "Bananas are yellow fruit grown in tropical climates.",
        "Mars is known as the Red Planet because iron minerals oxidize on its surface.",
        "Saturn is a gas giant recognized by its rings.",
    ]
    warm_started = time.perf_counter()
    rerank_scores = reranker.score(query, documents)
    vectors = embedder.encode([query, *documents])
    warm_millis = (time.perf_counter() - warm_started) * 1000.0
    similarities = vectors[1:] @ vectors[0]
    best_rerank = int(np.argmax(rerank_scores))
    best_embedding = int(np.argmax(similarities))
    if best_rerank != 1 or best_embedding != 1:
        raise RuntimeError("real models failed the held-out semantic smoke assertion")
    manifest_models = [
        {
            key: model[key]
            for key in (
                "name",
                "repository",
                "revision",
                "wire_model",
                "wire_model_revision",
                "license_spdx",
                "artifact_bytes",
                "artifact_sha256",
            )
        }
        for model in manifest["models"]
    ]
    return {
        "schema_version": 1,
        "status": "pass",
        "provider": "CPUExecutionProvider",
        "os": platform.system().lower(),
        "architecture": platform.machine().lower(),
        "models": manifest_models,
        "cold_start_millis": cold_millis,
        "warm_pair_millis": warm_millis,
        "rss_bytes": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        * (1 if platform.system() == "Darwin" else 1024),
        "rerank_scores": rerank_scores,
        "embedding_cosine_scores": similarities.tolist(),
        "embedding_dimensions": int(vectors.shape[1]),
    }


def read_json_stdin() -> dict[str, Any]:
    payload = json.load(sys.stdin)
    if not isinstance(payload, dict):
        raise ValueError("input must be a JSON object")
    return payload


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", help="ignored runtime/model directory")
    subcommands = parser.add_subparsers(dest="command", required=True)
    subcommands.add_parser("download", help="download both revision-pinned ONNX models")
    subcommands.add_parser("rerank", help="score one rerank_envelope_v3 JSON object from stdin")
    subcommands.add_parser("embed", help="embed source-linked chunk JSON from stdin")
    subcommands.add_parser("smoke", help="run retained real rerank and embedding assertions")
    arguments = parser.parse_args()
    root = install_root(arguments.root)
    if arguments.command == "download":
        result = download_models(root)
    elif arguments.command == "rerank":
        result = rerank_request(root, read_json_stdin())
    elif arguments.command == "embed":
        result = embed_chunks(root, read_json_stdin())
    else:
        result = smoke(root)
    json.dump(result, sys.stdout, separators=(",", ":"), sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, ValueError) as error:
        print(f"real semantic model error: {error}", file=sys.stderr)
        raise SystemExit(1) from error
