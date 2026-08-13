# Internal virtual beam engine

Phase 16 adds a provider-neutral, vector-only beam kernel to `context-query`.
It is an internal composition boundary for later topology work, not a SQL API,
planner mode, or user-selectable search strategy.

## Contract

The engine consumes already-authorized seed occurrences and asks a vector
provider for bounded expansion batches. Each state keeps its occurrence and
point identity, optional HNSW identity, parent state, transition, path-pattern
state, hop, opaque authorization context, and separate vector, transition,
accumulated, exact, and final ranking scores. Authorization tokens never appear
in returned hits or diagnostics.

One compact arena owns parent state. Dominance keys include occurrence,
path-pattern state, and authorization context. Duplicate, dominated, cyclic,
beam-width, and hop pruning are deterministic. Provider output is untrusted:
over-returned work, unknown parents, unsupported topology identities,
non-finite scores, and inconsistent exact-rerank accounting fail closed.
Canonical output order is score descending, then occurrence identity.

P16 represents topology identities in the type boundary but rejects topology
expansion. The engine therefore remains HNSW/vector-only. P17 owns the pure
topology kernel, P18 owns optional topology residency, and P19 owns the first
mixed vector/topology execution path.

## Bounds and termination

The default beam width and provider batch are 32; both have a maximum of 256.
One run admits at most 65,536 states and 65,536 dominance keys, 10,000,000
vector expansions, 10,000,000 exact reranks, 64 hops, 16 MiB of parent-arena
state, 256 MiB of total retained state, 10,000 final results, and 60 seconds.
Allocation projections cover arena capacity, dominance state, frontier state,
provider requests and responses, result identities, hits, and reconstructed
paths before growth, including live request/response buffers. Canonical sorts
are allocation-free. Cancellation is checked before seed work, before every
provider batch, and after every provider response; elapsed time is rechecked
immediately after the provider returns and before response admission.

Only `exhausted` is complete. Cancellation or exhaustion of admitted states,
visited keys, vector work, exact reranks, parent bytes, retained bytes, or
elapsed time returns a typed incomplete outcome. Provider and contract errors
return no partial output.

## Certification and maturity

The frozen `virtual_beam_vector_v1` manifest is
`02fe7d744c484dcf`. An independent P15 graph-off oracle over 128 deterministic
queries freezes ordered occurrence IDs, point IDs, bit-exact scores, and HNSW
work at `36e3f0bf52ddbbac`. The same cursor results then pass through the virtual
beam with an empty vector provider. The release gate requires exact output/work
parity, p50 latency no worse than 1.10 times the direct cursor path, retained
bytes no worse than 1.10 times direct, and process RSS no greater than 256 MiB.
An additional independent small-graph reference traversal covers provider
expansion, dominance replacement, cycles, canonical ties, path reconstruction,
and provider-order invariance; its frozen hash is `0d87b67f96b88dd5`.

The capability is **Internal**. It changes no SQL object, GUC, planner default,
index format, or PostgreSQL MVCC, ACL, RLS, and exact-source-recheck behavior.
