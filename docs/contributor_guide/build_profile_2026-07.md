# HNSW Build Phase Profile

This profile records where bulk-build time actually
goes, measured with `pgcontext.hnsw_build_stats()` (phase timing recorded by
the build callback) on Apple M4 Pro, PostgreSQL 17.9, release build,
`m=16`, `ef_construction=64`, 384-dimensional uniform-random vectors,
`maintenance_work_mem` sized above the estimate.

| Corpus | Graph phase (heap scan + in-memory construction) | Write phase (snapshots + page writes + Generic WAL) | Split |
|---:|---:|---:|---|
| 5,000 rows | 1,843 ms | 100 ms | 95% / 5% |
| 100,000 rows | 41,109 ms | 3,102 ms | 93% / 7% |

Uniform-random vectors are a hard case for HNSW neighbor selection, so the
absolute numbers run above the SciFact/clustered benchmark lanes; the phase
*split* is the finding.

## Conclusion

In-memory graph construction dominates build time at both scales; page
writes plus Generic-WAL emission are under 10%. Build-performance work
should therefore parallelize graph
construction — the approach pgvector takes with parallel builds — rather
than optimize WAL batching. Closing the build-time gap with pgvector (currently ~2.2-3.5x) requires
roughly a 2x construction speedup, consistent with what construction
parallelism delivers elsewhere.

## Reproduce

```sql
CREATE INDEX ... USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops);
SELECT * FROM pgcontext.hnsw_build_stats();
```

`graph_millis` and `write_millis` describe the most recent bulk build in the
calling backend; the same split is logged at `DEBUG1` during the build.

## Parallel build: per-node locking

`pgcontext.hnsw_build_parallel_workers` enables a `ConcurrentHnswBuilder`
built around per-node locking rather than one whole-graph lock: each node's per-layer neighbor list has its own `Mutex`,
published vectors are immutable (read with no lock), and a single small
`Mutex<BuilderRegistry>` serializes only id assignment, backbone-chain
linkage, and entry-point publication — pointer writes and small `Vec`
moves, no distance math, so its hold time is negligible regardless of
corpus size. Concurrent commits touching disjoint neighbor sets now run
fully in parallel; a commit only blocks another thread when they touch the
*same* node, holding at most one node lock at a time (ruling out
lock-ordering deadlocks by construction — no thread ever needs a second
node's lock while holding a first).

This measures as a real, scaling win:

| Workers | 20,000 rows × 384 dims, `CREATE INDEX` wall time | Speedup |
|---:|---:|---:|
| 1 (baseline) | 5,541 ms / 5,564 ms (repeat) | 1.0x |
| 2 | 2,622 ms | 2.11x |
| 4 | 1,819 ms / 2,709 ms (repeat) | 2.0-3.0x |
| 8 | 1,601 ms / 1,672 ms (repeat) | 3.3-3.5x |

Apple M4 Pro (10 performance + 4 efficiency cores), PostgreSQL 17.9, release
build, same corpus shape as the phase-split table above. Reproduced across
repeated runs; diminishing returns past 4-8 workers are expected (residual
registry-lock and thread-coordination overhead, plus this corpus's build
being fast enough in absolute terms that per-thread setup starts to matter).
ANN results after an 8-worker parallel build exactly matched the seqscan
exact-oracle top-10 on a spot-check query, and the automated recall test
(`concurrent_build_reaches_reasonable_recall_against_exact_search`) holds
recall ≥0.8 against exact search.

One correctness subtlety the per-node design handles: [`Self::wire`] clones a node's neighbor list
under its lock to add reciprocal edges on the *other* side, but between
that clone and the reciprocal add, a concurrent insert can re-prune the
first node's list — leaving `neighbor -> node` published with no matching
`node -> neighbor`. Rather than hold two node locks across the add
(reintroducing lock-ordering risk), `ConcurrentHnswBuilder::finish` runs a
single-threaded repair pass that drops any one-directional edge before
final validation; with one effective worker this is a no-op (proved by the
existing exact-match-with-sequential test), and a many-worker/many-round
stress test now exercises the interleavings that produce the asymmetry.

At 8 workers this is 3.3-3.5x faster than the sequential default at the
measured scale, past the ~2x target. `pgcontext.hnsw_build_parallel_workers`
defaults to `1` (no behavior change; the scan inserts directly), so it is
opt-in. Lock-contention behavior at 20k rows may not predict 1M-row behavior;
the larger-scale build sweeps in the benchmark lanes measure that.
