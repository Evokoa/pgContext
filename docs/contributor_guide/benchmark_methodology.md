# Benchmark Methodology

Benchmarks must use fixed data, fixed seeds, and explicit hardware/software
metadata. Release candidates should be comparable across commits without
depending on ad hoc local fixtures.

The end-to-end competitor harness and its first measured pgvector baseline are
documented in [pgContext vs. pgvector benchmark](../benchmarks/pgvector.md).
Keep competitor results separate from pure-Rust microbenchmarks, and always
report recall with ANN latency.

## Fixed Datasets

Use `context-test` for benchmark fixtures. It exposes iterator-backed datasets
so large release workloads can be streamed into Rust benchmarks, SQL fixtures,
or heavy-test loaders without allocating the full dataset at once.

| Dataset | Rows | Dimensions | Tenants | Seed | Intended use |
|---|---:|---:|---:|---:|---|
| Small | 1,000 | 32 | 10 | `0x706763745f736d6c` | Local smoke runs and benchmark development |
| Medium | 100,000 | 64 | 100 | `0x706763745f6d6564` | CI trend checks and exact-search baselines |
| Large | 1,000,000 | 128 | 1,000 | `0x706763745f6c7267` | Release-candidate latency, memory, and recall reports |

Each row has:

- one-based `point_id`;
- stable `source_key` in the form `bench-000000000001`;
- deterministic dense vector values in `[-1, 1)`;
- deterministic `tenant_id` buckets for filter-selectivity benchmarks;
- deterministic text payloads for hybrid dense plus full-text benchmarks.

Each dataset also has a fixed query vector derived from the dataset seed. Do
not replace these seeds to make a benchmark look better. Add a new named
dataset or document a deliberate methodology change instead.

## Required Measurements

Exact-search baselines should record latency and memory before approximate
paths are compared. HNSW reports should include build time, query latency,
memory per vector, index size, and recall against exact top-k fixtures for each
`m`, `ef_construction`, and `ef_search` setting under review.

Quantized HNSW reports should include the same HNSW fields plus codebook size
and exact-reranked recall. Filtered ANN reports should include selectivity band,
candidate survival rate, bitmap memory where applicable, and exact-vs-filtered
recall. Hybrid reports should separate dense-only, text-only, sparse-only,
fused dense plus text, fused dense plus sparse, and fully empty branches.
Late-interaction ANN reports should include token candidate collection latency,
exact MaxSim rerank latency, token graph memory, vector payload bytes, projected
MaxSim comparisons, candidate source-key count, and recall against exact MaxSim.

## Reporting

Benchmark reports must include:

- commit SHA and whether the worktree was clean;
- Rust toolchain, PostgreSQL version, target OS, and CPU model;
- dataset name, row count, dimensions, tenant count, and seed;
- exact command line and feature flags;
- warmup/sample settings;
- baseline commit or named release candidate;
- explicit note for any threshold waiver.

Slowdowns beyond the documented threshold require review. Treat missing exact
baselines, changed seeds, or unreported hardware metadata as invalid benchmark
evidence for release decisions.

## Delta Thresholds

Release benchmark jobs should compare each metric against the previous accepted
baseline with `context-test`'s benchmark delta policy:

- latency/search/build elapsed time: review required above `10%` regression;
- memory/index/vector/codebook bytes: review required above `5%` regression;
- recall: review required above `0.01` absolute recall drop.

Thresholds are not automatic failures when the release owner approves the
tradeoff, but the benchmark report must name the regressed metric, baseline
value, current value, and waiver reason. Missing or invalid baselines are not
waivers; rerun the benchmark with complete metadata instead.

## Recall Gates

Automated recall gates live in `context-test` and compare approximate candidate
paths against exact top-k on the fixed small dataset:

- HNSW `m=32`, `ef_construction=128`, `ef_search=64`: minimum recall `0.95`.
- Scalar/SQ8-style quantized candidates with exact rerank: minimum recall
  `0.95`.
- Binary sign-code candidates with exact rerank: minimum recall `0.75`.
- Late-interaction token-HNSW candidates with exact MaxSim rerank: minimum
  recall `0.95`.

Run them with:

```sh
cargo test -p context-test --test recall_gates
```

Lowering a threshold is a release-risk waiver and must be called out in the
benchmark report.

## Exact Baseline Runner

Run the dependency-free exact-search baseline harness with:

```sh
cargo bench -p context-test --bench exact_search_baseline
```

The runner prints one line per dataset with row count, dimensions, seed,
dense-vector payload bytes, build time, search time, and the top point ID. The
small and medium datasets are included by default so local runs stay bounded;
large release-candidate runs should use the same `context-test` workload APIs in
the release benchmark job.

Run the bounded pure-Rust HNSW baseline harness with:

```sh
cargo bench -p context-test --bench hnsw_baseline
```

The HNSW runner uses the small fixed dataset across multiple
`m`/`ef_construction`/`ef_search` settings and prints build time, search time,
vector bytes, graph bytes, bytes per vector, and recall against exact top-k.
Release-candidate jobs should run the same measurement shape against larger
datasets before publishing benchmark reports.

Run the bounded quantized-candidate baseline harness with:

```sh
cargo bench -p context-test --bench quantized_baseline
```

The quantized runner records binary sign-code candidates, scalar/SQ8-style
byte-code candidates, and the current product-quantization prototype. Each mode
prints candidate budget, codebook bytes, elapsed time, and exact-reranked recall
against exact top-k.

Run the bounded filtered ANN baseline harness with:

```sh
cargo bench -p context-test --bench filtered_ann_baseline
```

The filtered runner uses deterministic tenant buckets as stand-ins for
low/medium/high selectivity filters plus an empty filter. It reports allowed
point count, candidate survival rate, packed bitmap build time, bitmap memory,
search time, and recall against a filtered exact oracle.

Run the bounded hybrid branch baseline harness with:

```sh
cargo bench -p context-test --bench hybrid_baseline
```

The hybrid runner uses framework-free `context-hybrid` batches for dense-only,
text-only, sparse-only, fused dense plus text, fused dense plus sparse, and
fully empty branch cases. It reports input candidates, non-empty branch count,
output count, elapsed time, and the top fused point ID.

Run the bounded late-interaction ANN baseline harness with:

```sh
cargo bench -p context-test --bench late_interaction_ann_baseline
```

The late-interaction runner uses deterministic multi-vector points and token
HNSW candidates, deduplicates source keys, then exact-reranks candidates with
MaxSim. It reports token candidate collection latency, exact rerank latency,
token graph bytes, vector payload bytes, projected comparisons, and recall
against exact MaxSim. This is algorithmic release evidence; SQL access-method
and heavy-test evidence are still required before treating the production gate
as complete.

## Release Report Runner

Use the release benchmark report runner to capture benchmark and recall evidence
with commit, worktree, host, CPU, Rust, Cargo, baseline, sample-setting, command,
boundary, status, and log metadata:

```sh
scripts/run-benchmark-report.sh \
  --baseline previous-release-candidate \
  --postgres "PostgreSQL 17" \
  --features "context-test release benchmark profile" \
  --waiver "none"
```

The runner writes `summary.tsv`, `report.md`, and one log per benchmark under
`target/benchmark-reports/` by default. Use `--dry-run` to validate report
wiring without executing the benchmarks, `--bench NAME` to rerun one benchmark,
and `--no-recall` only when recall evidence is captured separately in the
release notes.

Release benchmark evidence is approved only when the report summary says
`Approval: complete`. Dry-run rows, failed rows, omitted recall gates, an
unnamed baseline, dirty worktree state, or a threshold waiver keep the report at
`Approval: incomplete`; attach those reports only as diagnostic evidence, not as
the release benchmark sign-off.
