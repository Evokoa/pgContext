# pgContext vs. pgvector vs. Qdrant benchmark harness

This harness generates real 384-dimensional MiniLM embeddings from SciFact and
compares pgContext 0.1.0 with pgvector 0.8.5 in separate databases on one
PostgreSQL 17 server and Qdrant 1.18.2 over local gRPC.

See the [methodology, results, caveats, and reproduction guide](../../docs/benchmarks/pgvector.md).

```sh
docker run --rm --name pgcontext-bench-qdrant \
  -p 6333:6333 -p 6334:6334 qdrant/qdrant:v1.18.2

benchmarks/pgvector_comparison/run.sh test
benchmarks/pgvector_comparison/run.sh prepare
PGCONTEXT_HNSW_EF_SEARCH=48 \
  benchmarks/pgvector_comparison/run.sh run --trials 3
```

Additional lanes:

```sh
# Latency-vs-recall Pareto curves per system (writes sweep.json).
benchmarks/pgvector_comparison/run.sh sweep --ef-values 16,24,32,48,64,96

# Filtered ANN at 1%, 10%, and 50% selectivity (writes filtered-sweep.json).
benchmarks/pgvector_comparison/run.sh filtered-sweep

# Multi-client throughput and per-backend resident memory
# (writes concurrency.json).
benchmarks/pgvector_comparison/run.sh concurrency --workers 1,8,32

# Seeded synthetic corpus for scale lanes beyond SciFact.
benchmarks/pgvector_comparison/run.sh prepare --synthetic 100000 \
  --output-dir target/pgvector-comparison-100k

# Update-churn stability: rewrite N% of rows per round, VACUUM, re-measure
# (writes churn.json; PostgreSQL systems only).
benchmarks/pgvector_comparison/run.sh churn --rounds 5 --churn-percent 5

# Cold-cache: restart PostgreSQL and record first-query latencies
# (writes cold-cache.json; PostgreSQL systems only).
PGCONTEXT_BENCH_RESTART_CMD="pg_ctl -D /opt/homebrew/var/postgresql@17 restart -w" \
  benchmarks/pgvector_comparison/run.sh cold-cache
```

The harness uses fixed database and collection names, so never run two
invocations against the same server concurrently — a second run drops and
recreates the databases the first one is still using.

Generated datasets and full results are written under
`target/pgvector-comparison/`. Compact historical results live in `results/`.
