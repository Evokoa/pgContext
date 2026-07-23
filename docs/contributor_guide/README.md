# pgContext Contributor Guide

Welcome to the pgContext Contributor Guide! We are thrilled to have you here. pgContext is developed as a modern Rust 2024 Cargo workspace featuring a tight PostgreSQL integration layer alongside highly reusable pure Rust crates.

Whether you're fixing bugs, optimizing vector operations, or simply looking to understand our architecture, these guides will help you get started safely and quickly.

## Guides

- [Architecture](architecture.md)
- [Vector engine architecture](vector_engine.md)
- [Repository map](repository_map.md)
- [Storage and memory](storage_memory.md)
- [Scripts and generated contracts](scripts.md)
- [Testing](testing.md)
- [Benchmark methodology](benchmark_methodology.md)
- [Fuzzing](fuzzing.md)
- [HNSW callback boundary](hnsw_callback_contract.md)
- [PostgreSQL integration module organization](postgres_module_organization.md)
- [Security review](security_review.md)
- [Release gate matrix](release_matrix.md)
- [Release gates](release_gates.md)
- [Release process](release_process.md)
- [Unsafe review](unsafe_review.md)
- [2026-07-11 unsafe and FFI audit](unsafe_ffi_audit_2026-07-11.md)

## Local Setup

Install the pinned Rust toolchain from `rust-toolchain.toml`, then run the
workspace gates:

```sh
cargo fmt --check
cargo clippy --workspace --exclude context-pg --all-targets --all-features -- -D warnings
cargo clippy -p context-pg --all-targets --features pg17 -- -D warnings
cargo test --workspace --exclude context-pg --all-features
cargo check -p context-pg --features pg17
scripts/run-v1-pgrx-tests.sh
cargo pgrx schema -p context-pg pg17 --out /tmp/pgcontext--0.2.0.generated.sql
scripts/check-extension-sql-artifact.sh --pg-major 17
scripts/check-hnsw-vacuum.sh
scripts/check-hnsw-restart.sh
cargo doc --workspace --no-deps
scripts/check-parity-matrix.sh
tests/shell/check_parity_matrix_smoke.sh
tests/shell/run_benchmark_report_smoke.sh
tests/shell/run_release_artifact_report_smoke.sh
tests/shell/check_extension_sql_artifact_smoke.sh
tests/shell/run_security_review_report_smoke.sh
tests/shell/run_postgres_matrix_gates_smoke.sh
scripts/check-source-hygiene.sh
scripts/check-hnsw-callback-guards.sh
tests/shell/check_hnsw_callback_guards_smoke.sh
scripts/run-unsafe-hardening-report.sh --pg-major 17 --plan
tests/shell/run_unsafe_hardening_report_smoke.sh
```

The PostgreSQL adapter is checked separately because pgrx crates link against
PostgreSQL extension symbols instead of a normal host test binary. SQL-facing
behavior should use pgrx or SQL regression tests once the relevant facade exists.

Supply-chain gates are part of CI:

```sh
cargo audit
cargo deny check
scripts/run-supply-chain-report.sh
```

## Dependency Policy

Do not add dependencies casually. New dependency additions must use the
repository safety wrapper for package-manager install or add commands, such as
`sfw cargo add crate-name`. Routine commands like `cargo test`, `cargo build`,
and `cargo doc` run normally.

Each new dependency needs a clear reason, a license/advisory review, and a
small commit that makes the resulting graph auditable.

## Development Expectations

Every milestone starts with tests that define the behavior. Keep PostgreSQL
integration in `context-pg`; reusable algorithms and domain types belong in the
pure Rust crates. Update public documentation and rustdoc in the same change as
behavioral code.

Unsafe code is denied by default. Only `context-core`, `context-pg`, and `context-storage` may
opt in when PostgreSQL FFI or validated mmap work requires it, and every unsafe
block must have a local `SAFETY:` explanation.
