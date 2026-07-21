## What this changes

<!-- One or two sentences. Focus on why, not a restatement of the diff. -->

## Testing

<!-- Commands you ran and their result. See CONTRIBUTING.md for the full
     pre-PR checklist. -->

- [ ] `cargo fmt --check`
- [ ] `cargo clippy --workspace --exclude context-pg --all-targets --all-features -- -D warnings`
- [ ] `cargo clippy -p context-pg --all-targets --features pg17 -- -D warnings`
- [ ] `cargo test --workspace --exclude context-pg --all-features`
- [ ] Relevant `pgcontext` pg_tests (`cargo pgrx test` or `scripts/run-v1-pgrx-tests.sh`)

## Documentation

- [ ] Public SQL-facing behavior changes are reflected in `docs/user_guide/`
- [ ] N/A — internal change with no public behavior change
