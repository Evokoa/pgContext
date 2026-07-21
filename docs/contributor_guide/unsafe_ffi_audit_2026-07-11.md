# Unsafe and FFI Open-Source Readiness Audit

Date: 2026-07-11

Scope: PostgreSQL 17 HNSW access-method callbacks, callback-local pointer
capabilities, page-record decoding, memory-context cleanup, Generic WAL
completion, and validated segment storage.

## Result

An independent follow-up review found three code-level blockers in the initial
candidate: unchecked PostgreSQL line pointers, production Generic-WAL calls
outside the linear finish boundary, and unsynchronized competing HNSW writers.
The candidate was reopened and all three findings were remediated before this
result was accepted. This remains a bounded source-launch review, not a claim
that all production sanitizer certification is complete.

The executable inventory identifies 16 PostgreSQL callbacks and 75 named unsafe
boundary items. A broader syntax scan finds 249 unsafe expressions, including
the guarded call sites and tests around those named boundaries. Every unsafe
expression passed the repository's nearby `SAFETY:`-comment check.

## Findings

- Every HNSW PostgreSQL callback uses `extern "C-unwind"` with `#[pg_guard]`
  and delegates to a safe inner function recorded in the callback contract.
- Raw callback pointers are converted into non-clonable `NonNull`-backed shared,
  mutable, or counted-slice capabilities. Null pointers and negative or invalid
  counts fail before creating Rust references.
- Page reads validate `pd_lower`, `pd_upper`, `pd_special`, the complete
  one-based line-pointer range, `LP_NORMAL`, item offset, and item length before
  dereferencing or copying a payload. Owned payloads are decoded only after the
  buffer is released. Record decoders then validate checked size arithmetic,
  dimensions, layers, neighbor counts, reserved bytes, and trailing bytes.
- Rust scan state is owned by a PostgreSQL memory-context drop slot and can be
  released at most once on normal completion or context reset.
- Generic WAL completion requires a linear finish permit after page ordering,
  validation, and fallible staging work. A source guard enforces exactly one
  direct `GenericXLogFinish` call, inside the permit implementation.
- HNSW insert acquires a database-local, transaction-scoped advisory lock keyed
  by index OID before reading allocator state. The lock covers graph rewiring,
  record append, and metapage publication, so competing writers cannot reserve
  the same node ID from one snapshot.
- Source-table visibility, TID decoding, and candidate hydration remain in
  PostgreSQL-owned callback lifetimes; no borrowed PostgreSQL pointer is stored
  in the pure Rust graph.

## Executed Evidence

The following rows ran through
`scripts/run-unsafe-hardening-report.sh --pg-major 17`:

| Gate | Result | Evidence |
|---|---|---|
| Callback source inventory | Pass | 16 callbacks and 75 named unsafe items |
| Unsafe safety comments | Pass | All workspace unsafe expressions covered |
| `context-storage` Miri segment suite | Pass | 18 tests |
| PostgreSQL HNSW ASan | Environment blocked | macOS requires the ASan runtime to be injected before compiler and PostgreSQL processes |
| PostgreSQL HNSW TSan | Environment blocked | The local nightly sysroot was not rebuilt with matching sanitizer ABI flags |

Additional remediations were executed directly while the candidate was
reopened:

| Regression | Result | Evidence |
|---|---|---|
| Corrupt page-item ranges | Pass | Pure boundary cases plus an aligned BLCKSZ physical-page fixture with invalid flags and line-pointer bounds |
| Competing HNSW writers | Pass | A held per-index advisory lock deterministically blocked both writers; 24 rows at four rounds and both writer ranges were visible after release |

The sanitizer rows failed before compiling or executing pgContext code. They
are unresolved release-environment gates, not observed memory-safety failures.
Run them on the Linux release host with a sanitizer-compatible nightly sysroot
before making a production certification claim.

## Residual Risk

PostgreSQL proves pointer validity, aliasing, lock ownership, and buffer lifetime
through C callback contracts that Rust cannot verify statically. Changes to AM
signatures, pgrx bindings, page layouts, memory contexts, or Generic WAL order
therefore require another inventory review, focused lifecycle tests, and Linux
sanitizer evidence.
