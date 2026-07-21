# Release Gates

## Performance gates (dispositioned 2026-07-17)

V1 defined five measurable performance gates. Every
disposition below closes on a `git_dirty: false` harness artifact archived
under `benchmarks/pgvector_comparison/results/` and summarized with full
context in [the benchmark page](../benchmarks/pgvector.md):

| Gate | Meaning | Disposition |
|---|---|---|
| G1 | 1M matched-recall query curves vs pgvector | **Closed** — strict Pareto domination (`sweep-1m-fdbbf527.json`) |
| G2 | Filtered ANN vs pgvector across selectivity | **Closed** — full-recall adaptive search at every selectivity vs pgvector's 0.34-0.49 recall ceiling (`filtered-sweep-100k-1a70f2eb.json`) |
| G3 build | Build time vs pgvector at 1M | **Closed** — 0.52x pgvector serial at 8 workers (`cp10-build-parallelism-1m-d1b65517.json`) |
| G3 memory | 32-client serving RSS parity | Improved (8.4 → 6.2 GiB), not yet parity with pgvector's 4.4 GiB; owned by the serving-memory roadmap |
| G4 | Write churn stability | **Failed, owned** — 1-2 updates/s degrading with 4.4x index growth (`churn-partial-100k-c9ff4d76.json`); owner is the segmented-index write path (Phase 2), exit gate ≥500 updates/s |
| G5 | Reproducibility gate on a second environment | Local reduced gate passed; first green scheduled CI run pending an operator push |

The V1 release ships with G4 documented as a known limitation (bulk-load
and read-mostly positioning) rather than hidden; write-heavy trial use is
deferred to the segmented-index phase by an explicit warning in
[the benchmark page](../benchmarks/pgvector.md).

## Release-engineering gates

The GitHub V1 gate is PostgreSQL 17-only and has four layers:

1. clean-source quality/security and the independent unsafe/FFI audit;
2. source and Docker installation using the packaged HNSW/filter demo;
3. PGXN, Homebrew, multi-architecture OCI, and immutable promotion contracts;
4. reproducible source payload, checksums, SBOM, provenance, documentation, and
   the publication handoff.

Run the bounded local gate from a clean checkout:

```sh
PG_CONFIG=/path/to/postgresql-17/bin/pg_config \
  release/checks/open-source-readiness.sh
```

Publication is intentionally separate. The manual `Release` workflow prepares
without public mutations; its protected publish mode requires the exact tag,
candidate SHA, prepare run, and accepted manifest digest. See the
[release process](release_process.md), [release matrix](release_matrix.md), and
[release tooling](../../release/README.md).
