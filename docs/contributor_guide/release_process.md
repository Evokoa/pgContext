# Release Process

This project treats release gates as part of the implementation, not a separate
cleanup phase.

## Required Gates

Run the relevant gates before committing a milestone:

```sh
cargo fmt --check
cargo clippy --workspace --exclude context-pg --all-targets --all-features -- -D warnings
cargo clippy -p context-pg --all-targets --features pg17 -- -D warnings
cargo test --workspace --exclude context-pg --all-features
cargo check -p context-pg --features pg17
scripts/run-v1-pgrx-tests.sh
cargo doc --workspace --no-deps
scripts/check-extension-sql-artifact.sh --pg-major 17
scripts/check-public-docs.py --check
tests/shell/check_public_docs_smoke.sh
scripts/check-parity-matrix.sh
tests/shell/check_parity_matrix_smoke.sh
tests/shell/run_fast_release_gate_report_smoke.sh
tests/shell/run_benchmark_report_smoke.sh
tests/shell/run_release_artifact_report_smoke.sh
tests/shell/check_extension_sql_artifact_smoke.sh
tests/shell/run_security_review_report_smoke.sh
tests/shell/run_postgres_matrix_gates_smoke.sh
scripts/check-source-hygiene.sh
gitleaks git . --config .gitleaks.toml --redact=100 --no-banner --no-color
```

Run HNSW restart, vacuum, fuzz, sanitizer, Miri, benchmark, or recall gates when
the changed code touches those risk areas.

`scripts/check-extension-sql-artifact.sh` normalizes pgrx connected-object
ordering before comparing the checked-in SQL artifact, because cargo-pgrx can
emit equivalent object blocks in different orders. It is a SQL-surface drift
guard, not a byte-for-byte reproducibility proof; release artifact reports still
own per-major reproducibility and signing evidence.

Capture the fast release-gate report from a clean tree before marking the
release candidate complete:

```sh
scripts/run-fast-release-gate-report.sh \
  --pg-major 17 \
  --out-dir target/fast-release-gates/release-candidate
```

The report stays incomplete if any baseline gate fails, runs in dry-run mode,
omits a log, cannot identify `cargo-pgrx`, or is produced from a dirty tree.

Run the security review report before marking a release candidate complete:

```sh
scripts/run-security-review-report.sh --pg-major 17
```

Repeat for supported PostgreSQL majors when security behavior depends on
version-specific SQL, privilege, or lifecycle behavior.

V1 publication supports PostgreSQL 17 only. PostgreSQL 15, 16, and 18 are
post-V1 certification targets; their incomplete matrix rows do not block the
PG17 source launch and must not be advertised as supported.

Benchmark reports must compare latency, memory/size, and recall metrics against
the previous accepted baseline. Regressions above the documented benchmark
methodology thresholds require an explicit review note before the slower or
larger behavior can be accepted.

Fuzz campaign reports must name every target, duration, corpus path, toolchain,
and crash artifact disposition. Do not treat a campaign as complete while a
panic, timeout, sanitizer finding, or unreduced artifact remains unexplained.

## Public Contracts

SQL-visible errors must map to stable SQLSTATE categories. Diagnostic output
should use typed status enums and structured counters instead of strings that
clients need to parse. Any monitoring, SQLSTATE, diagnostic, or compatibility
change needs a regression test or expected-output update.

Public Rust APIs should have rustdoc with `# Errors` and `# Safety` sections
where applicable. Review new public enums and structs for `#[non_exhaustive]`
unless the type is intentionally closed.

## Unsafe and Fuzz Policy

Unsafe code is permitted only where PostgreSQL FFI or validated mmap work
requires it. Every unsafe block needs a nearby `SAFETY:` explanation. Parser,
SQL renderer, binary loader, mmap, and custom AM changes should add unit,
property, fuzz, or heavy regression coverage at the lowest layer that can prove
the behavior.

Run the unsafe review checklist before release and for any PostgreSQL FFI or
storage-boundary change:

```sh
scripts/check-unsafe-safety-comments.sh
cargo +nightly miri test -p context-storage
RUSTFLAGS="-Zsanitizer=address" cargo +nightly pgrx test -p context-pg pg17
```

If Miri or sanitizer gates are not practical locally, record the blocker and run
them in CI or on the release host before marking the release candidate complete.

Run the containerized Linux gate from a clean tree before publishing artifacts:

```sh
PG_MAJOR=17 scripts/release-linux-container-gates.sh
```

Capture the combined macOS/Linux platform build report before marking platform
coverage complete:

```sh
scripts/run-platform-build-report.sh \
  --platform macos \
  --out-dir target/platform-builds/macos

scripts/run-platform-build-report.sh \
  --platform linux \
  --out-dir target/platform-builds/linux

scripts/run-platform-build-report.sh \
  --merge-report target/platform-builds/macos/report.md \
  --merge-report target/platform-builds/linux/report.md \
  --out-dir target/platform-builds/release-candidate
```

Build and inspect the complete unsigned V1 source payload before publishing:

```sh
release/build-packages.sh --out-dir target/release-payload v0.1.0
scripts/verify-release-payload.py \
  --tag v0.1.0 \
  --candidate-sha "$(git rev-parse HEAD)" \
  target/release-payload
```

V1 uses a GitHub-verified signed annotated tag, published SHA-256 checksums, a
Sigstore attestation for the source archive, the immutable candidate SHA in
provenance, and an attested OCI manifest digest. Multi-major combined artifact
approval remains post-V1 roadmap work.

Capture advisory and license evidence before final release approval:

```sh
scripts/run-supply-chain-report.sh \
  --out-dir target/supply-chain/release-candidate
```

## Dependencies

New dependencies must be added through `sfw` for supported package managers,
have a clear reason, and pass advisory and license review. Routine build, test,
doc, and check commands run without `sfw`.

### Known advisory exception

`pgrx 0.19.1` currently brings in `serde_cbor 0.11.2`. RustSec
`RUSTSEC-2021-0127` classifies that crate as unmaintained; it does not report a
known vulnerability. `deny.toml` records the exception so it remains visible
and reviewable. Remove the exception when the selected pgrx release no longer
depends on `serde_cbor`, or immediately if RustSec publishes a vulnerability
that changes the risk.

## Stability and Deprecation

Prefer additive SQL and Rust API changes. If a public behavior needs to change,
document the compatibility impact, keep migration guidance close to the change,
and make diagnostics clear enough for operators to understand the new behavior.
Stable SQLSTATE changes, return-column changes, status-value semantic changes,
and removed stable SQL objects are breaking changes and require an extension
version bump with upgrade and rollback notes.
