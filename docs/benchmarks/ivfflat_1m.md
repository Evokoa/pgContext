# Historical Native IVFFlat v3 One-Million-Row Correctness Run

This retained development run predates the clean v4 format. It documents a v3
implementation checkpoint only: it is not evidence for the current format, a
release-mode latency claim, or a substitute for the pending matched 1M/10M
HNSW/pgvector certification lane.

## Environment and workload

- PostgreSQL 17 on Apple arm64.
- Development (unoptimized) extension build.
- 1,000,000 authoritative source rows with eight dense dimensions.
- 316 centroid lists, `maintenance_work_mem = 8MB`, and four deterministic v3
  backend-local assignment workers. Current v4 builds use native PostgreSQL
  parallel workers instead.
- Twenty deterministic exact-oracle queries, `probes = 32`, candidate budget
  500,000, and top-k 10.

## Result

| Measurement | Result |
|---|---:|
| Verified historical clean-v3 SQ8 index size | 16,384,000 bytes |
| SQ8 posting pages | 1,995 |
| Codec pages / code width | 1 / 8 bytes |
| Empty lists | 0 |
| Maximum-list skew ratio | 2.199992 |
| SQ8 recall@10 against exact source scoring | 1.0 (minimum and mean) |
| SQ8 development-build p50 | 309.322 ms |
| SQ8 development-build p95 | 402.9994 ms |

The historical v3 verifier passed every metapage, centroid, directory, codebook, posting
checksum, extent, and revision binding. A prior full-precision run at the same
list count occupied 40,878,080 bytes and 4,986 posting pages. A preliminary
100,000-candidate run stopped at the configured hard ceiling as designed;
raising the ceiling to 500,000 admitted the recorded query set. The result
therefore demonstrates deterministic bounded failure, compact SQ8 storage, and
exact-oracle candidate quality at this operating point.

Do not compare the latency values with release benchmarks: the extension was
not optimized, this run did not use matched pgvector or HNSW indexes, and the
v3 artifact cannot validate v4. Current full-precision, SQ8, and PQ behavior is
covered by focused PG17/18 exact-rerank and lifecycle tests, not by this retained
scale result. Stable performance promotion remains gated on the reproducible
release-mode 1M/10M comparison lane.
