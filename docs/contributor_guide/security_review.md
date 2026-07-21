# Security Review

The release security review must produce auditable evidence before the release
candidate can be marked complete. The review covers PostgreSQL security-definer
paths, hostile `search_path` behavior, source-table ACL and RLS boundaries,
SQLSTATE stability, telemetry privacy, and unsafe-code comments.

Run the security review report runner for the PostgreSQL major under review:

```sh
scripts/run-security-review-report.sh --pg-major 17
```

The runner writes `summary.tsv`, `report.md`, and one log per gate under
`target/security-review/` by default. Use `--dry-run` only to validate report
wiring. A final release review must run the real gates and reconcile every
failed or skipped row before checking off the release checklist.

The report includes:

- hostile `search_path` and shadow-catalog pgrx tests;
- telemetry privacy pgrx tests that reject vector, payload, filter, and
  query-text storage;
- source-table ACL and collection ownership pgrx tests;
- point-mutation ACL denial pgrx tests;
- source-table RLS and split-owner ACL pgrx tests;
- SQLSTATE contract pgrx tests;
- unsafe `SAFETY:` comment checks;
- heavy RLS/ACL boundary coverage;
- heavy-wrapper SQLSTATE contract coverage for the configured PostgreSQL major.

Security-definer functions must set a safe `search_path`, fully qualify
extension catalog access, resolve user source tables through validated metadata,
and check the SQL session user before exposing source rows or mutating
collection-owned metadata. New security-definer functions need matching
catalog classification, SQLSTATE, hostile-input, and ACL/RLS coverage before
they can be treated as release-ready.

Before publishing a repository or release candidate, scan the complete Git
history rather than only the checked-out files:

```sh
gitleaks git . --config .gitleaks.toml --redact=100 --no-banner --no-color
```

The 2026-07-11 open-source review used gitleaks 8.30.1 across 573 commits and
reported no leaks. CI repeats the full-history scan on every pull request and
push to `main`.

The current independent unsafe-boundary findings and remaining sanitizer
environment gates are recorded in
[`unsafe_ffi_audit_2026-07-11.md`](unsafe_ffi_audit_2026-07-11.md).
