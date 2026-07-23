# Contributing to pgContext

pgContext's PostgreSQL 17 V1 source build is complete. Contributions should
be small, tested, and aligned with the crate boundaries in the workspace.

Use GitHub issues or discussions for public design and support questions. Send
potential security vulnerabilities privately to `team@evokoa.com` as described
in `SECURITY.md`.

Before opening a change, run:

```sh
cargo fmt --check
cargo clippy --workspace --exclude context-pg --all-targets --all-features -- -D warnings
cargo clippy -p context-pg --all-targets --features pg17 -- -D warnings
cargo test --workspace --exclude context-pg --all-features
cargo check -p context-pg --features pg17
scripts/run-v1-pgrx-tests.sh
cargo pgrx schema -p context-pg pg17 --out /tmp/pgcontext--0.2.0.generated.sql
cargo doc --workspace --no-deps
scripts/check-crate-boundaries.sh
scripts/check-public-docs.py --check
tests/shell/check_public_docs_smoke.sh
scripts/check-parity-matrix.sh
tests/shell/check_parity_matrix_smoke.sh
tests/shell/run_benchmark_report_smoke.sh
tests/shell/run_release_artifact_report_smoke.sh
tests/shell/run_security_review_report_smoke.sh
tests/shell/run_postgres_matrix_gates_smoke.sh
tests/shell/run_platform_build_report_smoke.sh
tests/shell/run_supply_chain_report_smoke.sh
scripts/check-source-hygiene.sh
scripts/run-fuzz-smoke.sh
```

Fuzz targets live under `fuzz/` and use `cargo-fuzz`. Install `cargo-fuzz`
outside this repository if needed. Run the bounded, fixed-seed smoke suite for
ordinary development:

```sh
scripts/run-fuzz-smoke.sh
scripts/run-fuzz-smoke.sh --target cursor_state
```

The registry in `fuzz/smoke-targets.txt` fixes the per-target input bound and
requires a checked-in seed corpus. The smoke runner copies those seeds under
`target/` before invoking libFuzzer, so local mutations never alter the source
corpus. Long-duration campaigns and their release evidence remain a separate
post-freeze workflow.

Property tests use `proptest` and run with the normal crate test commands.

Public behavior changes must include user-facing documentation. Public Rust APIs
need rustdoc that explains errors, panics, and safety contracts where relevant.

Dependency additions must go through `sfw` for package-manager install or add
commands and should be isolated in their own commit when practical.
