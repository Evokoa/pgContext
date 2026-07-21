# Unsafe Review

Unsafe code is allowed only for architecture-specific vector kernels,
PostgreSQL FFI, pgrx/PostgreSQL allocation contracts, buffer/page access, and
validated artifact mappings where a safe Rust API cannot express the boundary.
Every unsafe block in `context-core`, `context-pg`, and `context-storage` must
have a nearby `SAFETY:` comment that states the caller
contract, pointer ownership, lifetime, alignment, initialization, and cleanup
reasoning that makes the block valid.

## Checklist

Before release or any unsafe change:

- run `scripts/check-unsafe-safety-comments.sh`;
- run `scripts/check-hnsw-callback-guards.sh` and its adversarial shell smoke
  suite when HNSW callback or routine wiring changes;
- confirm each `unsafe extern` callback documents the PostgreSQL caller
  contract and does not unwind across a non-unwind ABI;
- confirm every raw pointer dereference has a live owner, non-null expectation,
  and lifetime bounded to the PostgreSQL callback or validated byte slice;
- confirm PostgreSQL memory allocated through pgrx is returned to PostgreSQL or
  dropped exactly once;
- confirm page, tuple, and mmap byte reads use validated lengths and alignment
  checks before interpreting bytes;
- confirm SQL-visible failures map to typed diagnostics instead of panics;
- run the narrow unit/property/fuzz/heavy tests that exercise the boundary.

## Current Review

`context-core` isolates AArch64 NEON loads in `metric_kernels`; complete-chunk
bounds and scalar-tail tests cover non-multiple-of-four dimensions.

`context-storage` owns its read-only OS mapping in `mmap_file`. Construction
checks the file size and mapping result, validates the mapped header, payload
bounds, alignment, and checksum before exposing a borrow, and unmaps the exact
owned range once in `Drop`. Segment regression tests, property tests, and the
`segment_loader` fuzz target cover header length, payload length, checksum,
version, endian, and corruption cases. The practical Miri gate is:

```sh
cargo +nightly miri test -p context-storage
```

`context-pg` uses unsafe code for PostgreSQL extension and index access-method
FFI, mainly in `hnsw_am.rs`. The current review requires local `SAFETY:`
comments for unsafe blocks plus pgrx and heavy tests that cover HNSW build,
insert, scan, vacuum, crash/restart, and corruption paths. Miri is not practical
for PostgreSQL backend FFI, so sanitizer coverage should run through pgrx on a
nightly toolchain when changing those callbacks:

The HNSW access-method entrypoints, safe functions, borrowed-input contracts,
and retention rules are enumerated in the
[HNSW Callback Boundary Contract](./hnsw_callback_contract.md). Review that
inventory whenever the routine gains, removes, or changes a callback.

```sh
RUSTFLAGS="-Zsanitizer=address" cargo +nightly pgrx test -p context-pg pg17
```

If sanitizer or Miri cannot run in the local environment, record the toolchain
or platform blocker in the release report and run the gate in CI or on the
release host before declaring the release candidate complete.

The executable hardening manifest keeps those commands, their owners, and the
static unsafe guards in one fixed order. Product-build work should inspect it
without starting a long campaign:

```sh
scripts/run-unsafe-hardening-report.sh --pg-major 17 --plan
scripts/run-unsafe-hardening-report.sh --pg-major 17 --dry-run \
  --out-dir target/unsafe-hardening/dry-run
tests/shell/run_unsafe_hardening_report_smoke.sh
```

`--plan` is side-effect free and emits the canonical TSV rows. `--dry-run`
writes the same five rows and per-row logs without invoking Cargo, Miri,
sanitizers, or PostgreSQL. The no-flag form executes every row and is owned by
the frozen-SHA hardening phase. When a later product slice adds an unsafe owner,
validated mmap view, callback surface, or subprocess harness, extend the runner
and its smoke fixture in that same slice; do not defer the executable row until
release evidence collection.
