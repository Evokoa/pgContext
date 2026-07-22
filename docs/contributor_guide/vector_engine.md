# Vector Engine Architecture

This document describes the vector engine implemented in pgContext 0.1.0. It covers both PostgreSQL-page HNSW and
rebuildable mmap generations. Exact source-table scoring remains the correctness
oracle for public retrieval results.

## Query path

1. PostgreSQL parses the packed, versioned `vector` datum. Distance functions
   borrow its float payload without allocating a second vector.
2. Dense L2, L1, dot, and fused cosine dot/norm calculations dispatch to
   four-lane AArch64 NEON kernels when available and use allocation-free scalar
   kernels elsewhere.
3. HNSW descent and base-layer search use binary heaps and a dense visited set.
   Masked-out nodes remain connectors; only mask-eligible nodes enter the result
   heap.
4. A sparse candidate mask enables ACORN-like second-hop exploration when an
   excluded connector would otherwise stop useful expansion.
5. Registered table search counts a bounded filter result once. Small result
   sets cross over to exact scoring; larger result sets build one reusable mask
   and perform one filter-aware HNSW traversal.
6. Public table-backed results are joined to the authoritative source row and
   rechecked for current MVCC visibility, ACL/RLS, predicate truth, deletion,
   and exact distance.

The raw SQL access method cannot reinterpret an arbitrary PostgreSQL `WHERE`
clause as a pgContext mask. Filter-aware traversal is a feature of the
registered collection API; raw SQL uses normal PostgreSQL post-filtering.

## PostgreSQL-page graph

HNSW node records contain the dense vector and all adjacency layers needed for
traversal. Append-only directory records map a logical node or adjacency
revision to an exact PostgreSQL `(block, slot)` locator. A scan therefore reads
a visited node directly instead of rescanning every index page.

The metapage carries a monotone directory epoch. Each PostgreSQL backend caches
the resolved directory in an `Rc` keyed by index OID, epoch, and metapage LSN.
The first scan after a mutation loads the directory; warm scans reuse it without
rescanning or cloning directory pages. Build, insert, and VACUUM mutation paths
advance the epoch only after publishing locator changes.

Construction uses the standard hierarchical shape:

- base layer degree is bounded by `2 * m`;
- upper layers are bounded by `m`;
- `ef_construction` controls bounded candidate exploration;
- the diversity heuristic rejects a candidate when a previously selected
  neighbor is closer to it than the new node is;
- rewires are bidirectional and pruned with the same deterministic heuristic.

Planner costing delegates the base estimates to PostgreSQL's generic index
cost estimator, rejects unordered use, and scales startup work from row count,
`m`, and `ef_search`. It does not advertise a fixed near-zero cost.

## Immutable mmap generations and mutable data

An mmap build creates an actual HNSW graph from the registered source vectors
and serializes contiguous node IDs, point IDs, full-precision vectors, and base
neighbors inside a checksummed segment. Serving opens the file with a real
read-only OS mapping, validates the outer header and inner graph before exposing
borrowed bytes, pins the generation for the query, and traverses the stored
graph. Legacy edgeless test artifacts retain an exact fallback for format
compatibility.

Published mmap generations are immutable. Inserts with point IDs above the
generation high-water mark form a mutable source-table tail. Search exact-scores
that tail, merges it with graph candidates, deduplicates by point ID, and then
performs the authoritative source recheck. Generation retirement remains
reader-pin aware.

Updates to rows already represented in an immutable generation are corrected
when those rows reach source recheck, but an arbitrary update cannot add a
previously unselected old point to the ANN candidate set. Operators should
compact/rebuild after material old-row vector updates. Deletes are always
removed by source recheck.

## Quantization

Binary, scalar, and product quantizers plus full-precision reranking are
implemented as reusable primitives. Scalar encoding uses constant work per
dimension. Deterministic scalar-range and product-codebook training is owned by
the pure index crate; identical ordered samples produce identical codebooks,
and persisted binary codes validate fixed dimensions and padding. Source-built
mmap artifacts use payload v2 to bind the trained codebook and per-node codes to
the graph generation. Their ANN traversal reconstructs the encoded navigation
vectors, requires an oversampled candidate set, and exact-reranks from live
source rows. Payload v1 remains readable. PostgreSQL-page and packed-image
navigation still use full-precision vectors pending their versioned formats.

## Compatibility

The packed SQL vector datum and HNSW metapage/directory formats are specific to
pgContext 0.1.0. Vector columns and HNSW indexes built by any pre-release
prototype
must be reloaded or rebuilt when moving to this engine revision. Segment files
remain rebuildable acceleration artifacts, never the authoritative copy of
application data.

## Evidence

The implementation is covered by pure Rust unit/property suites, unsafe and
callback inventory guards, PostgreSQL release-mode integration tests for warm
direct reads, source-built mmap navigation, and mutable-tail merging, plus the
fixed-input [pgContext vs. pgvector benchmark](../benchmarks/pgvector.md).
