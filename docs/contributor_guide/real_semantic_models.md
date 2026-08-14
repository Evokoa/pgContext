# Optional real semantic model tests

The default test suite uses deterministic fixtures because CI must remain
network-free, reproducible, and small. A separate promotion test downloads two
revision-pinned Apache-2.0 MiniLM ONNX models:

- `cross-encoder/ms-marco-MiniLM-L6-v2` exercises P12 reranking.
- `sentence-transformers/all-MiniLM-L6-v2` exercises P13 embeddings.

The arm64 installer selects the roughly 23 MB quantized ONNX artifact for each
model. Other architectures use the portable ONNX artifact. Model weights,
tokenizer files, virtual environments, and reports live under the ignored
`target/real-semantic-models` directory. They are not included in source or
release packages.

Install and run the local CPU smoke test explicitly:

```sh
scripts/install-real-semantic-models.sh
scripts/run-real-semantic-model-smoke.sh
```

After installing pgContext into a local PostgreSQL 17 or 18 installation, run
the database-bound promotion smoke separately:

```sh
PG_MAJOR=17 scripts/run-real-semantic-postgres-smoke.sh
```

It creates an isolated temporary PostgreSQL cluster, prepares a P12 envelope,
scores it with the real cross-encoder, and finalizes the response through
PostgreSQL. It also embeds source-linked chunks, builds a 384-dimension
`pgcontext_hnsw` index, retrieves the relevant occurrence, and verifies that
its citation object survived. The temporary cluster is removed on exit.

The installer requires `uv` and Python 3.12. It verifies exact model revisions
and records each downloaded ONNX artifact's byte length and SHA-256 digest in
`target/real-semantic-models/installed-models.json`. The smoke report records
the models, revisions, digests, platform, cold-start time, warm inference time,
RSS, rerank scores, embedding cosine scores, and embedding dimensions.

The adapter can also score one `rerank_envelope_v3` object from standard input:

```sh
target/real-semantic-models/venv/bin/python \
  tools/real-semantic-models/semantic_models.py rerank < envelope.json
```

Register or prepare that envelope with model name
`cross-encoder/ms-marco-MiniLM-L6-v2` and numeric model revision `1`. The
installed manifest binds that wire identity to the exact Hugging Face revision
and artifact digest. The adapter rejects any mismatched identity instead of
echoing it in a response.

For source-linked chunks, `embed` accepts objects shaped as
`{"chunks":[{"occurrence_id":1,"citation":{...},"text":"..."}]}`. Its output
preserves the occurrence and citation objects beside a normalized 384-dimension
embedding. This is a test adapter, not an automatic P13 database job runner.

These smokes are initial real-model promotion checks, not Stable certification.
Stable promotion still needs a retained held-out quality set, source/ACL/RLS
churn, timeout and cancellation tests, hosted CPU/platform evidence, and a
decision about whether a real inference runtime belongs inside
`pgcontext-worker` or remains an application-managed adapter. P13 also still
needs a production embedding-job integration; this smoke does not replace the
automatic chunking schema's deterministic fixture embedding.
