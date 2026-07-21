# Fuzzing

Fuzz targets live under `fuzz/` and focus on parser, renderer, loader, mmap,
and candidate-mask boundaries that accept untrusted or SQL-visible input.

## Targets

| Target | Boundary | Release checklist coverage | Seed corpus |
|---|---|---|---|
| `filter_json` | JSON filter parser and filter-budget enforcement | filter JSON; JSONB path handling | `fuzz/corpus/filter_json` |
| `sql_predicate` | resolved filter SQL rendering and placeholder accounting | SQL predicate rendering | `fuzz/corpus/sql_predicate` |
| `vector_text` | dense, half, sparse, and bit vector text round trips | vector text | `fuzz/corpus/vector_text` |
| `segment_loader` | segment decode, mmap validation, and encode/decode checks | segment loading; mmap views | `fuzz/corpus/segment_loader` |
| `candidate_mask` | HNSW candidate-mask budget validation | candidate masks | `fuzz/corpus/candidate_mask` |

Seed corpora should include the minimized input for every fuzz-found panic,
crash, timeout, or sanitizer finding before the fix lands. Keep corpora small
and focused; use comments in the commit message or report to explain large
generated seeds instead of committing noisy bulk output.

## Quick Build Check

The fuzz crate can be type-checked on the stable project toolchain:

```sh
cargo check --manifest-path fuzz/Cargo.toml --bins
```

This does not run libFuzzer. It only proves the targets compile.

## Campaigns

Actual fuzz campaigns require a nightly Rust toolchain because `cargo fuzz`
enables sanitizer flags that use unstable `-Z` options. Release candidates
should use the campaign runner so every target writes a log, Markdown report,
and TSV summary under `target/fuzz-campaigns/`:

```sh
scripts/run-fuzz-campaigns.sh
```

The release-gates workflow exposes the same runner as a manual
`workflow_dispatch` path. Set `run_fuzz_campaign=true` and leave
`fuzz_duration_seconds=86400` for release-candidate evidence; use `fuzz_jobs`
only to control how many targets run concurrently. The workflow uploads the
report as `release-fuzz-campaign-report` from
`target/fuzz-campaigns/release-candidate`.

The default duration is 24 hours per target. Use `--duration SECONDS` only for
smoke checks or explicitly approved shorter low-risk patch-release campaigns.
Use `--target NAME` to rerun one target after a fix, `--jobs N` to run multiple
targets concurrently while preserving per-target logs and artifact directories,
and `--dry-run` to verify the target list and report wiring without starting
libFuzzer.

Each report records the command, target, corpus path, commit SHA, host OS,
boundary, status, exit code, requested duration, measured elapsed duration, and
per-target log path. It also lists the release checklist boundary coverage and
counts any files left in the libFuzzer artifact directories. The report approval
is `complete` only when all default targets pass from a clean worktree with no
dry-run rows, no failures, no short elapsed rows, no crash artifacts, and at
least the default 24-hour duration per target. Short runs, target subsets, dirty
worktrees, dry-runs, and partial target runs are useful diagnostics, but their
reports stay `incomplete` and cannot satisfy the release-candidate fuzz gate.
`--jobs` only changes scheduling; it does not lower duration, target, clean-tree,
or artifact requirements. Non-dry-run campaigns refuse dirty worktrees unless
`--allow-dirty` is used for diagnostic evidence, and that override also keeps
approval incomplete.

A campaign is incomplete until every crash artifact is either fixed with a
committed corpus seed or tracked as a release-blocking finding.
