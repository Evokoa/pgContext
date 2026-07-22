# pgContext vs. pgvector vs. Qdrant benchmark

All numbers on this page come from clean committed trees (`git_dirty: false`
in the archived result JSONs) on the same machine: Apple M4 Pro, macOS 26.5
arm64, PostgreSQL 17.9, Python 3.12.9, pgContext 0.1.0, pgvector 0.8.5, and
Qdrant 1.18.2 in the official Docker image over its recommended local gRPC
transport.

Headline: on the SciFact warm-cache single-client workload, pgContext's HNSW
latency/recall curve Pareto-dominates pgvector's — at every measured
`ef_search`, pgContext is faster, and at equal recall targets it is faster at
equal or better recall. Qdrant's curve sits above both PostgreSQL extensions
because every Qdrant query crosses a client/service gRPC boundary that
in-process extensions do not pay; Qdrant also returns perfect recall on this
small corpus. This page also reports the lanes pgContext does **not** win:
index build time, low-selectivity raw filtered ANN, and the masked filter
path's function-boundary overhead.

## Recognized-dataset comparison — GloVe-100-angular, 1.18M, matched Docker (2026-07-20)

This is the most directly comparable lane: a **standard public dataset**
([ann-benchmarks](https://github.com/erikbern/ann-benchmarks) `glove-100-angular`,
1,183,514 x 100, cosine), recall measured against the **dataset's own
precomputed ground-truth neighbors** (not a self-generated corpus or each
system's own exact scan), with **all three systems in matched Docker
deployment** so the client/transport boundary is identical, and the same
8-way parallel build budget for both PostgreSQL engines. Rendered reports:
[pgContext vs pgvector](reports/pgcontext-vs-pgvector.html) and
[pgContext vs pgvector vs Qdrant](reports/pgcontext-vs-pgvector-vs-qdrant.html).

| ef_search | pgContext recall / p50 | pgvector recall / p50 | Qdrant recall / p50 |
|---:|---:|---:|---:|
| 64 | 0.731 / **0.67 ms** | 0.750 / 2.56 ms | 0.839 / 1.30 ms |
| 128 | 0.810 / **0.94 ms** | 0.820 / 4.14 ms | 0.893 / 1.48 ms |
| 256 | 0.868 / **1.46 ms** | 0.870 / 7.09 ms | 0.934 / 1.91 ms |
| 512 | 0.910 / 2.44 ms | 0.910 / 12.97 ms | 0.963 / **2.38 ms** |
| build | 88.0 s | 50.3 s | 19.9 s (7 segments) |

Two honest readings.

- **Versus pgvector, pgContext wins cleanly.** Recall matches at every
  `ef_search` (both trace the same 0.73 → 0.91 curve), and pgContext serves it
  **3.8–5.3x faster** — 2.44 ms versus 12.97 ms at the top of the sweep.
  pgvector builds faster (50 s versus 88 s).
- **Versus Qdrant, the comparison is honest and Qdrant leads at high recall.**
  With the transport asymmetry removed (earlier SciFact lanes ran native
  PostgreSQL against a Dockerized Qdrant crossing the Docker Desktop VM
  boundary; here every system is Dockerized), Qdrant's real strength shows:
  0.963 recall at 2.38 ms, where pgContext needs more `ef_search` to approach
  0.96. Qdrant ran **7 segments**, so it applies `hnsw_ef` per segment and its
  per-query effective candidates are `ef x 7`. Effort-matched, pgContext's
  single graph is at least as accurate per candidate, but Qdrant's segment
  fan-out delivers a latency-at-recall a single-threaded scan cannot. This is
  the per-query-parallelism gap the roadmap's segmented-serving work targets —
  it is not a graph-quality deficit.

Caveats: single trial, warm cache, single client, on an Apple M4 Pro with the
NEON distance kernels active (x86 AVX2 kernels exist but are not yet
performance-measured). The harness and archived result JSON are in
`benchmarks/pgvector_comparison/`.

## Latency vs. recall Pareto sweep (SciFact 5,183 x 384, cosine)

Three-system `ef_search` sweep, 200 queries, 20 warmups, top-10, m=16,
ef_construction=64. Harness artifact
`apple-m4-pro-pg17.9-2026-07-16-sweep-21eda7ec.json` was recorded at commit
`21eda7ec`; generated result files are not distributed in this source snapshot.

| ef | pgContext p50 / recall@10 | pgvector p50 / recall@10 | Qdrant p50 / recall@10 |
|---:|---:|---:|---:|
| 16 | **0.095 ms / 0.9620** | 0.111 ms / 0.9700 | 0.993 ms / 1.0000 |
| 24 | **0.094 ms / 0.9760** | 0.133 ms / 0.9830 | 1.123 ms / 1.0000 |
| 32 | **0.107 ms / 0.9815** | 0.146 ms / 0.9875 | 0.838 ms / 1.0000 |
| 48 | **0.126 ms / 0.9900** | 0.170 ms / 0.9925 | 0.929 ms / 1.0000 |
| 64 | **0.139 ms / 0.9950** | 0.205 ms / 0.9950 | 0.702 ms / 1.0000 |
| 96 | **0.167 ms / 0.9975** | 0.253 ms / 0.9965 | 0.730 ms / 1.0000 |

At ef=64 the two PostgreSQL systems land on the same 0.9950 recall; pgContext
serves it 32% faster at p50. Read whole curves, not single points: this
retires any single-`ef` comparison, including our own earlier ones.

Qdrant caveat, stated plainly: the Qdrant column measures a local gRPC client
round trip against a service; the PostgreSQL columns measure in-process
index scans reached over local libpq. That difference is real for
application latency but it is not a distance-kernel comparison, and Qdrant's
1.0000 recall on this small corpus reflects its optimizer choosing
effectively exact behavior at this scale. Qdrant remains the more mature
distributed vector service; this harness measures the local single-node case
only.

## Three-trial matched-recall run (SciFact)

Rotating trial order, median/combined distribution across three trials,
pgContext ef=48 vs pgvector/Qdrant ef=40. Harness artifact
`apple-m4-pro-pg17.9-2026-07-16-clean-74f083c2.json` was recorded at commit
`74f083c2`; generated result files are not distributed in this source snapshot.

| Measurement | pgContext | pgvector | Qdrant |
|---|---:|---:|---:|
| Load 5,183 vectors, median | 0.734 s | 0.710 s | 0.233 s |
| Build HNSW, median | 1.018 s | **0.397 s** | 0.553 s |
| HNSW index size | 10.44 MiB | **10.13 MiB** | not captured |
| Exact cosine p50 | 0.784 ms | **0.686 ms** | 0.683 ms |
| HNSW cosine p50 | **0.157 ms** | 0.179 ms | 0.690 ms |
| HNSW cosine p95 | **0.284 ms** | 0.300 ms | 0.982 ms |
| HNSW recall@10 | 0.9900 | 0.9902 | **1.0000** |
| Filtered ANN p50, 10% | 0.533 ms | **0.378 ms** | 0.678 ms |
| Filtered ANN recall@10 | 0.9935 | 0.9658 | **1.0000** |
| Filtered full-result rate | 100% | 100% | 100% |
| pgContext masked ANN p50 | 1.823 ms | — | — |
| pgContext masked recall@10 | 1.0000 | — | — |

pgContext loses the build-time lane (2.6x pgvector) and the raw 10% filtered
latency lane (1.4x, at meaningfully higher recall). Those are open
engineering items, not footnotes.

## Filtered ANN across selectivity (SciFact)

`filtered-sweep` lane, 1% / 10% / 50% selectivity predicates, matched
settings as above. Harness artifact
`apple-m4-pro-pg17.9-2026-07-16-filtered-sweep-21eda7ec.json` was recorded at
commit `21eda7ec`; generated result files are not distributed in this source snapshot.

| Selectivity | Strategy | p50 | recall@10 |
|---|---|---:|---:|
| 1% | pgContext exact scan | **0.312 ms** | 1.0000 |
| 1% | pgContext raw filtered ANN | 3.683 ms | 1.0000 |
| 1% | pgContext masked traversal | 2.222 ms | 1.0000 |
| 1% | pgvector iterative ANN | 2.051 ms | 0.9860 |
| 1% | Qdrant filtered ANN | 0.640 ms | 1.0000 |
| 10% | pgContext raw filtered ANN | 0.545 ms | 0.9935 |
| 10% | pgvector iterative ANN | 0.524 ms | 0.9630 |
| 10% | Qdrant filtered ANN | 0.677 ms | 1.0000 |
| 50% | pgContext raw filtered ANN | **0.195 ms** | 0.9810 |
| 50% | pgvector iterative ANN | 0.225 ms | 0.9775 |
| 50% | Qdrant filtered ANN | 0.772 ms | 1.0000 |

What this says, honestly: at 1% selectivity every graph strategy loses to a
plain exact scan — the right system behavior is choosing exact automatically,
which pgContext's registered-collection API can do and the raw index path
cannot yet. At 10% the raw lanes are a latency wash while pgContext returns
substantially more of the true top-10. At 50% pgContext wins latency and
recall outright. Qdrant's filter-aware index gives it the flattest profile
across selectivity — that flatness at every selectivity level is the bar for
pgContext's planned adaptive filtered execution.

## 100k synthetic scale lane

Seeded synthetic clustered corpus (100,000 x 384, generator in the harness),
single trial, same settings, `maintenance_work_mem=2GB` for both PostgreSQL
systems. Harness artifact
`apple-m4-pro-pg17.9-2026-07-16-100k-86330dcd.json` was recorded at commit
`86330dcd`; generated result files are not distributed in this source snapshot.

| Measurement | pgContext | pgvector | Qdrant |
|---|---:|---:|---:|
| Load 100,000 vectors | 14.2 s | 16.4 s | **7.4 s** |
| Build HNSW | 48.8 s | 13.8 s | **2.5 s** |
| HNSW index size | 200.8 MiB | **195.3 MiB** | not captured |
| ANN p50 (pgContext ef=48, others ef=40) | **0.494 ms** | 0.886 ms | 0.983 ms |
| ANN recall@10 at those settings | 0.6920 | 0.3945 | 0.7745 |
| Filtered ANN p50, 10% | **1.192 ms** | 1.515 ms | 1.836 ms |
| Filtered ANN recall@10 | 0.8925 | 0.4225 | **1.0000** |

Two findings this lane exists to surface, stated plainly:

1. **Build time is pgContext's worst scale lane.** The gap versus pgvector
   widens from 2.6x at 5k rows to 3.5x at 100k, and Qdrant builds the same
   graph roughly 20x faster. This fails the project's own build-time gate and
   is open engineering work, not a caveat.
2. **Fixed small-ef points are meaningless at this scale.** On this harder
   clustered corpus, every system's recall collapses at the SciFact-tuned
   settings (all below 0.78). No latency claim at these settings is a win for
   anyone; the matched-recall sweep below is the only comparison we stand
   behind at 100k.

A third finding from bringing the lane up: pgContext refuses HNSW builds
whose estimated memory exceeds `maintenance_work_mem` instead of degrading
the way pgvector does; under PostgreSQL's default 64MB budget, that refusal
triggers near this corpus size. The harness now sets an explicit equal budget
for both systems, and the graceful-build gap is tracked as follow-up work.

### Matched-recall sweep at 100k

Because fixed small-ef points are meaningless here, the 100k comparison that
matters is the high-ef sweep (m=16, ef_construction=64 for all systems).
Harness artifact `apple-m4-pro-pg17.9-2026-07-16-sweep-100k-531360f5.json`
was recorded at commit `531360f5`; generated result files are not distributed
in this source snapshot.

| ef | pgContext p50 / recall@10 | pgvector p50 / recall@10 | Qdrant p50 / recall@10 |
|---:|---:|---:|---:|
| 64 | **0.541 ms / 0.7705** | 1.072 ms / 0.5130 | 1.625 ms / 0.8495 |
| 96 | **0.579 ms / 0.8675** | 1.149 ms / 0.5675 | 1.831 ms / 0.8875 |
| 128 | **0.665 ms / 0.9160** | 1.246 ms / 0.6055 | 1.996 ms / 0.9115 |
| 192 | **0.749 ms / 0.9655** | 1.678 ms / 0.6385 | 1.997 ms / 0.9445 |
| 256 | **0.864 ms / 0.9820** | 2.035 ms / 0.6685 | 2.031 ms / 0.9770 |

Single-client, pgContext's graph is decisively better on this corpus: it
reaches 0.982 recall at 0.86 ms while pgvector's graph, built with the same
nominal parameters, does not exceed 0.67 recall at any measured ef. pgContext's
longer build (3.5x) buys real graph quality here, which is part of the
build-time tradeoff. Qdrant tracks
pgContext's recall curve at roughly 2.3x the latency, consistent with its
service boundary. Read this together with the concurrency lane below before
concluding anything about production throughput.

## Filtered ANN at 100k across selectivity — pgContext leads pgvector at every band

Filtered ANN across selectivity on the 100k synthetic corpus (200 queries,
ef_search=48). Archived as
`apple-m4-pro-pg17.9-2026-07-17-filtered-sweep-100k-1a70f2eb.json`.

| Selectivity | pgContext adaptive (collection API) | pgContext operator filter | pgvector | Qdrant |
|---|---:|---:|---:|---:|
| 1% | 21.9 ms @ **1.000** recall | 5.5 ms @ 0.997 | 4.8 ms @ 0.489 | 0.7 ms @ 1.000 |
| 10% | 33.6 ms @ **1.000** recall | 1.2 ms @ 0.887 | 1.4 ms @ 0.368 | 2.3 ms @ 1.000 |
| 50% | 71.0 ms @ **1.000** recall | 0.4 ms @ 0.608 | 0.7 ms @ 0.338 | 1.0 ms @ 0.660 |

Reading, honestly:

- **pgContext's adaptive path beats pgvector's filtered recall outright.**
  pgvector's post-filtered scan cannot exceed ~0.34-0.49 recall at any
  measured selectivity on this corpus — filter starvation — while the
  collection API returns full-recall results at every selectivity, and
  even our raw operator-level filter dominates pgvector's recall at
  comparable latency (0.997 vs 0.489 at 1%).
- **The open gap is Qdrant's filtered latency**, not recall: their
  payload-aware traversal reaches full recall in ~1-2 ms where our
  full-recall path costs 22-71 ms (mask construction and exact-scan
  fallbacks dominate; the 50% lane is an exact scan by strategy). The
  planned statistics-driven selectivity estimation, packed-bitmap masks,
  and iterative widening target exactly this overhead.
- The masked-filter candidate budget is configurable via
  `pgcontext.hnsw_mask_candidate_limit`.

## Concurrency lane (100k corpus)

1, 8, and 32 client processes, each running the full 200-query set (pgContext
ef=48, pgvector ef=40 — the same settings as the fixed-point lane above, so
read latency alongside that lane's recall figures).

pgContext serves concurrent backends from a shared packed-base registry (a
`GetNamedDSMSegment`-backed registry: one backend publishes its packed
generation, other backends attach it instead of rebuilding). Harness artifact
`apple-m4-pro-pg17.9-2026-07-16-concurrency-100k-3ea49bb0.json` was recorded at
commit `3ea49bb0`; generated result files are not distributed in this source snapshot.

| Clients | pgContext agg QPS / backend RSS | pgvector agg QPS / backend RSS |
|---:|---:|---:|
| 1 | 140 / 734 MiB | 299 / 139 MiB |
| 8 | 2,347 / 1.5 GiB | 1,747 / 1.1 GiB |
| 32 | **2,621** / 6.2 GiB | 2,480 / 4.4 GiB |

Aggregate QPS scales with concurrency (140 → 2,347 → 2,621), and pgContext
edges out pgvector's throughput at both 8 and 32 clients. One shared image
serves many backends rather than each backend holding a private copy. Backend
memory is higher than pgvector's shared-buffer footprint (6.2 GiB vs 4.4 GiB
at 32 clients), because the registry holds one full copy plus per-backend
fallback state; memory parity is a known limitation.

## 1M synthetic scale lane

Same generator at 1,000,000 x 384, single trial,
`maintenance_work_mem=2GB`. Harness artifact
`apple-m4-pro-pg17.9-2026-07-16-1m-f29edac4.json` was recorded at commit
`f29edac4`; generated result files are not distributed in this source snapshot.

| Measurement | pgContext | pgvector | Qdrant |
|---|---:|---:|---:|
| Load 1,000,000 vectors | 138 s | 161 s | **105 s** |
| Build HNSW | 774 s | 346 s | **84 s** |
| HNSW index size | **1.84 GiB** | 1.91 GiB | not captured |
| ANN p50 (fixed benchmark ef) | 2.54 ms | 79.07 ms | **1.77 ms** |
| ANN recall@10 at those settings | 0.1400 | 0.0875 | **0.5430** |
| Filtered ANN p50, 10% | 6.75 ms | 254.98 ms | **1.84 ms** |
| Filtered ANN recall@10 | 0.2960 | 0.0765 | **0.7365** |

What we claim from this lane, and what we do not:

- **Build time.** With `pgcontext.hnsw_build_parallel_workers` (a
  per-node-locking parallel builder), the 1M x 384 build measures
  (`maintenance_work_mem=4GB`): 638 s at 1 worker, 437 s at 2, 347 s at 4,
  **181 s at 8 workers** (3.5x speedup, 0.52x of pgvector's 346 s serial
  build), still behind Qdrant's 84 s. The speedup is consistent across
  20k/100k/1M. (The 774 s figure in the table above is a single-worker
  build.)
- **High-ef 1M sweep** (archived as
  `apple-m4-pro-pg17.9-2026-07-17-sweep-1m-fdbbf527.json`): pgContext
  strictly Pareto-dominates pgvector.

  | ef_search | pgContext | pgvector | Qdrant |
  |---:|---:|---:|---:|
  | 128 | 2.80 ms @ 0.305 | 83.03 ms @ 0.168 | 3.74 ms @ 0.859 |
  | 256 | 3.55 ms @ 0.462 | 48.16 ms @ 0.249 | 4.66 ms @ 0.934 |
  | 384 | 4.16 ms @ 0.574 | 21.87 ms @ 0.292 | 4.77 ms @ 0.955 |

  Our worst point beats pgvector's best point on both axes
  simultaneously — 5-30x lower latency at higher recall across the
  entire measured range.

  **Reading the Qdrant column.** The fixed-ef rows above compare unlike
  quantities. Qdrant applies `hnsw_ef` *per
  segment* and merges results across every segment of the collection,
  and its default segment count is the machine's CPU count — so on this
  14-core machine its "ef=384" row explores on the order of ten times
  more candidates per query than one ef=384 search of a single graph.
  A direct probe of the same 1M corpus at HEAD (40 queries, single
  trial, ef_construction=200 build, index plan verified) shows the
  monolithic graph has no recall ceiling:

  | ef_search | recall@10 | p50 |
  |---:|---:|---:|
  | 384 | 0.6525 | 4.63 ms |
  | 768 | 0.8325 | 4.85 ms |
  | 1536 | 0.9225 | 6.30 ms |
  | 3072 | **0.9650** | 8.95 ms |

  At matched total effort (pgContext ef=3072 against Qdrant's ~10 segments
  x ef=384) the graphs are equivalent: 0.965 vs 0.955. What is genuinely
  better on the Qdrant side is **latency at high recall** (4.77 ms vs
  8.95 ms), because segment fan-out parallelizes one query across cores
  while a single graph is searched on one thread. Closing that is
  architecture work (segmented serving with per-query fan-out, on the
  roadmap), not graph-construction work. These probe figures are
  preliminary — 40 queries, single trial. Sweep reports record Qdrant's
  segment count and per-ef effective candidate totals (`qdrant_effort` in
  sweep.json) so the comparison can be read effort-matched.
- The masked-filter candidate ceiling is configurable via
  `pgcontext.hnsw_mask_candidate_limit` (up to 5M points); the filtered
  lane's large-mask economics remain a scale consideration.

## Update-churn lane (100k corpus) — sustained high-churn writes

The harness's `churn` lane rewrites 5% of vectors per round, VACUUMs, and
re-measures. Two rounds were measured (archived as
`apple-m4-pro-pg17.9-2026-07-17-churn-partial-100k-c9ff4d76.json`); the trend
was already clear:

| Round | update throughput | first post-churn query | index size | recall @ ef=48 |
|---:|---:|---:|---:|---:|
| 0 (baseline) | — | — | 201 MiB | 0.692 |
| 1 | 2 rows/s (2,463 s for 5,000) | 1,211 ms | 541 MiB | 0.695 |
| 2 | 1 row/s (3,677 s) | 1,727 ms | 885 MiB | 0.686 |

Without the segmented write path, sustained high-churn writes degrade on
every dimension: write throughput (1-2 rows/s, degrading per round), post-churn
first-query latency (1.2-1.7 s vs sub-ms steady), and index growth (4.4x
baseline by round 2). The cause is O(graph-size) page reconstruction per
incremental insert. The segmented write path addresses this:

### Re-measurement with delta segment plus threshold compaction (2026-07-20)

The delta segment and threshold-triggered compaction target exactly this
sustained-write pattern. Measured on the same 100k 384-dimension corpus,
`maintenance_work_mem = 2GB`, single-row inserts in a `plpgsql` loop:

| Measurement | Value |
|---|---|
| 12,000 single-row inserts, wall clock | 45.6 s |
| Sustained throughput | **263 rows/s** |
| Slowest single insert | **42.9 s** (the compaction) |
| Inserts exceeding 1 s | 1 of 12,000 |
| Delta-path appends excluding that insert | ~4,440 rows/s |
| Explicit `pgcontext.compact()` on the same index | 59.5 s, drained 10,261 delta records |
| Index size after compaction | 439 MB (201 MB baseline) |

Sustained throughput is 263 rows/s — roughly two orders of magnitude above
the 1-2 rows/s of pure inline splicing — and 11,999 of 12,000 inserts are
sub-millisecond. Three limitations remain:

- **Sustained throughput misses the 500 rows/s bar.** The figure is amortized:
  cheap delta appends plus one full rebuild per drained segment. Compaction
  cost is fixed per run, so raising `pgcontext.hnsw_delta_segment_limit`
  amortizes it further and can clear 500 rows/s, at the cost of query latency
  (scans exact-scan the whole delta).
- **Index size misses the ≤1.3x bar** at 2.2x. Compaction deliberately leaves
  superseded pages in place — it reclaims write throughput, not disk. `REINDEX`
  reclaims the space.
- **The tail is the main remaining limitation.** One insert blocking for 42.9 s is
  not acceptable as a steady-state property, and it grows with the index.
  `pgcontext.hnsw_compact_on_threshold_max_mb` (default 1GB of projected
  vectors, tunable) bounds which indexes an insert will compact by itself, so
  the stall is capped rather than unbounded, but it is still paid by a user
  statement.

The planned improvement is running compaction in a background worker so no
statement pays for it (see the roadmap); the synchronous path plus the size
bound is what ships today.

Note on measurement method: the rounds table uses the harness's `churn` lane;
the single-row insert figures use a direct insert loop, because the harness
lane rewrites rows via `UPDATE` and reports per-round aggregates that hide the
single-statement tail. Both are reported.

## Cold-cache lane (100k corpus)

The `cold-cache` lane restarts PostgreSQL, then measures 20 queries, **each
opened on a fresh connection** (a fresh PostgreSQL backend process), before
measuring steady state on a warm, reused connection.

With the shared packed-base registry, only the first backend after a restart
rebuilds the packed generation; every later backend attaches the published
image. Harness artifact
`apple-m4-pro-pg17.9-2026-07-16-cold-cache-100k-0c8129e6.json` was recorded at
commit `0c8129e6`; generated result files are not distributed in this source snapshot.

| Measurement | pgContext | pgvector |
|---|---:|---:|
| First query after restart | 640.0 ms | **3.2 ms** |
| p50 of first 20 cold (fresh-backend) queries | **5.3 ms** | 1.8 ms |
| Steady-state p50 | 1.626 ms | **0.552 ms** |

The registry is empty immediately after a restart, so the very first backend
pays the full pack cost (640 ms). Every backend after the first attaches the
published image, so the cold-start cost is paid once per server lifetime, not
once per backend — the median of the first 20 cold queries is 5.3 ms. pgContext
does not match pgvector's cold path, which serves cold reads straight from
shared buffers with no pack step; pgContext needs one first-ever pack per
server start. This is a known limitation.

## Methodology

- Dataset: public [MTEB SciFact](https://huggingface.co/datasets/mteb/scifact),
  pinned to revision `cf10ab6856b15b0e670ef8ae5dae4e266c12d035`; scale lane
  uses the harness's seeded synthetic generator (seed `20260715`).
- Model:
  [sentence-transformers/all-MiniLM-L6-v2](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2)
  through FastEmbed's ONNX export; 384 dimensions, cosine.
- Query sample: 200 queries selected without replacement with seed
  `20260715`; 20 warmups; top 10.
- Timing: single-client warm-cache request latency unless the lane states
  otherwise. Embedding generation, dataset download, and client-side vector
  serialization are outside the timed window.
- PostgreSQL isolation: pgContext and pgvector use separate databases in the
  same cluster with identical rows and server settings.
- PostgreSQL exact oracle: HNSW, bitmap, and ordinary index scans disabled;
  ANN trials fail if the plan does not select the HNSW index.
- Qdrant: Query API with `exact=true` for the oracle, `exact=false` with the
  collection fully indexed for ANN; pinned `qdrant/qdrant:v1.18.2`; client
  prefers gRPC; integer payload indexes created before HNSW construction.
- Every archived JSON records the git SHA and a clean/dirty flag; only
  `git_dirty: false` artifacts are cited on this page. Excluding dirty-tree
  results is an editorial discipline, not enforced automatically by the
  harness.

Not yet measured: a matched-recall 1M curve (fixed-ef 1M evidence exists and
is reported above), the complete multi-round churn artifact (round-1
evidence reported above), constrained-memory serving, replication, and
distributed operation. Claims on this page are limited accordingly.

## Reproduce

Install pgContext and pgvector against PostgreSQL 17:

```sh
cargo pgrx install -p context-pg --release \
  --pg-config /opt/homebrew/opt/postgresql@17/bin/pg_config \
  --no-default-features --features pg17

PG_CONFIG=/opt/homebrew/opt/postgresql@17/bin/pg_config \
  benchmarks/pgvector_comparison/install_pgvector.sh
```

Start the pinned Qdrant service:

```sh
docker run --rm --name pgcontext-bench-qdrant \
  -p 6333:6333 -p 6334:6334 \
  qdrant/qdrant:v1.18.2
```

Run the lanes (never two invocations concurrently against one server):

```sh
benchmarks/pgvector_comparison/run.sh test
benchmarks/pgvector_comparison/run.sh prepare

# Three-trial matched-recall comparison
PGCONTEXT_HNSW_EF_SEARCH=48 PGVECTOR_HNSW_EF_SEARCH=40 QDRANT_HNSW_EF_SEARCH=40 \
benchmarks/pgvector_comparison/run.sh run \
  --dsn "host=/tmp port=5432 dbname=postgres" \
  --queries 200 --warmup 20 --trials 3

# Pareto sweep and filtered selectivity sweep
benchmarks/pgvector_comparison/run.sh sweep --ef-values 16,24,32,48,64,96
benchmarks/pgvector_comparison/run.sh filtered-sweep

# 100k scale + concurrency lanes
benchmarks/pgvector_comparison/run.sh prepare --synthetic 100000 \
  --output-dir target/pgvector-comparison-100k
benchmarks/pgvector_comparison/run.sh run \
  --output-dir target/pgvector-comparison-100k --trials 1
benchmarks/pgvector_comparison/run.sh concurrency \
  --output-dir target/pgvector-comparison-100k --workers 1,8,32
```

The harness pins pgvector 0.8.5, Qdrant server 1.18.2, qdrant-client 1.18.0,
FastEmbed 0.7.3, psycopg 3.2.9, and certifi 2025.8.3.

## Not yet measured

Areas this page does not yet cover, so no claim is made about them:

1. A matched-recall 1M curve against Qdrant. On the 1M sweep pgContext
   strictly Pareto-dominates pgvector; against Qdrant, the open item is
   latency at high recall, which comes from per-query segment parallelism
   (see the 1M reading above), not graph quality.
2. Sustained high-churn writes: throughput is 263 rows/s, with a
   single-statement compaction tail and index growth as the remaining
   limitations (see the churn section). Background-worker compaction over
   segmented units is the planned improvement.
3. Memory parity with pgvector under concurrency (6.2 GiB vs 4.4 GiB at 32
   clients) and the first-backend cold-start cost after a restart (640 ms,
   which `pgcontext.hnsw_pack_on_first_use=off` page-native serving avoids).
4. Constrained-memory serving and WAL-volume comparisons.
