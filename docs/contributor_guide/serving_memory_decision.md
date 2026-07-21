# Serving-Memory Model Decision (2026-07-16)

Status: **decided — hybrid shared base plus backend-local delta.** This decision record captures why the hybrid model was chosen on
2026-07-16, after reviewing the measurements below.

## The problem being decided

The HNSW engine serves warm traversals from a packed graph generation: an
immutable, contiguous in-memory image of the graph that each backend builds
from the authoritative PostgreSQL index pages and reuses until the metapage
publication identity changes. Packing is what makes single-client latency
Pareto-dominant over pgvector — and it is backend-local, which is the root
cause of every negative result in the July 2026 benchmark suite.

## Measurements forcing the decision

All from clean commits on Apple M4 Pro, PostgreSQL 17.9, 100k×384 synthetic
corpus (archived under `benchmarks/pgvector_comparison/results/`):

| Failure mode | Evidence | Artifact commit |
|---|---|---|
| Memory × connections | 32 clients hold 8.4 GiB of packed generations total (~260 MiB each) vs pgvector's 4.4 GiB shared-buffer footprint | `86330dcd` |
| Throughput inversion | Aggregate QPS falls 758 → 539 from 8 → 32 clients while pgvector scales 1,741 → 2,298 (4.3× pgContext) | `86330dcd` |
| Cold start | First query after restart: 553 ms vs 0.42 ms steady (~1,300×); every backend repacks on first touch | `847e0581` |
| Invalidation repack | First query after a churn round: 557 ms; every insert/update batch invalidates every backend's pack | churn round-1 observation, 2026-07-16 |

## The decision

**Hybrid: a shared read-mostly packed generation plus a small backend-local
delta.**

- The packed base generation lives once in dynamic shared memory under a
  global budget, so resident memory stops scaling with connections and cold
  start packs once per server, not once per backend.
- Incremental changes apply as a bounded backend-local (or shared,
  implementation's choice) delta instead of invalidating the whole pack, so
  churn stops causing full repack storms.
- Delta compaction folds accumulated changes back into a new shared base
  generation on a policy trigger (size ratio or invalidation count).

Rejected alternatives, for the record:

- *Fully shared DSM generations* solves the same three failure modes but
  couples every invalidation to shared-memory lifecycle and locking on the
  hot path; the delta tier exists precisely to keep invalidations cheap.
- *Budgeted per-backend LRU* is the smallest change but retains memory
  duplication and per-backend cold cliffs, which the measurements show are
  the dominant production risks.

## Constraints the implementation must keep

1. PostgreSQL index pages and WAL remain the only authoritative graph state;
   packed bases and deltas are rebuildable caches.
2. Over-budget or unavailable shared memory degrades to page-native
   traversal — never an error, never a silent exact scan.
3. Cache behavior is observable: SQL-visible counters for base packs, delta
   applications, compactions, and fallbacks (shared-registry telemetry).
4. The churn, cold-cache, and concurrency lanes in
   `benchmarks/pgvector_comparison/` are the acceptance tests: gates G3 and
   G4 close on their artifacts, not on design intent.
