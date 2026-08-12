# P12 worker runtime and license decision

Status: selected and certified for the P12 provider-neutral contract

P12 deliberately certifies the worker protocol without selecting a general
transformer runtime. PostgreSQL owns authorization and final membership;
`pgcontext-worker` owns a bounded, versioned request/response protocol and one
operator-provided fixture adapter.

## Runtime spike

The compatibility spike considered these Rust-side inference options:

- `tract`: not selected. Its ONNX/operator coverage and transitive runtime
  footprint would require a separately licensed transformer bundle and a new
  platform/operator compatibility program.
- Candle: not selected. It does not by itself freeze the tokenizer, model
  operators, or redistribution rights needed for a stable general-purpose
  reranker claim.
- Rust ONNX Runtime bindings (`ort`): not selected. They introduce a native
  runtime ABI, redistribution, and per-platform packaging surface that is not
  required to prove the provider-neutral contract.

The selected `linear_pair_v1` adapter is a private pure-Rust certification
fixture. It accepts only `ascii_tokens_v1`, a 40-byte `PGLPAIR1` artifact, and
the exact `rerank_envelope_v3` / `rerank_response_v3` /
`rerank_failure_v1` contracts. The artifact is operator-provided, verified by
length and SHA-256 before use, and is not a transformer-compatibility claim.

## Worker dependency and artifact licenses

The worker has no provider SDK, network client, Python runtime, model download,
or bundled weights. Its direct dependency inventory is:

| Component | Purpose | License contract |
|---|---|---|
| pgContext local crates (`context-core`, `context-query`) | validated identities and envelopes | Apache-2.0 |
| `serde`, `serde_json` | bounded manifest and wire decoding | MIT OR Apache-2.0 |
| `sha2` | artifact and content digests | MIT OR Apache-2.0 |
| `tokio` | supervised current-thread I/O and tracked blocking scoring | MIT |
| `linear_pair_v1` fixture artifact | contract certification only | Apache-2.0, operator-provided only |

The immutable P12 certification manifest records the artifact digest, SPDX
identifier, HTTPS license URL, distribution policy, protocol identifiers,
token ceilings, deadline, retry/breaker policy, and required platform matrix.
Adding a real model runtime is a later adapter decision and must bring its own
operator coverage, artifact license inventory, platform evidence, and retained
quality/cost report without changing PostgreSQL's authority contract.
