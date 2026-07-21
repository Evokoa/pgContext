# Testing

Use the smallest layer that can prove a behavior:

1. pure Rust unit/property tests for parsing, metrics, planning, and formats;
2. PostgreSQL/pgrx tests for SQL, catalogs, visibility, privileges, and access
   methods;
3. shell contract tests for generators, installers, workflows, and reports;
4. heavy lifecycle tests for restart, WAL, VACUUM, backup/restore, concurrency,
   corruption, recall, and resource bounds.

The minimum ordinary change gate is documented in [CONTRIBUTING](../../CONTRIBUTING.md).
HNSW/storage changes also run their focused callback, restart, VACUUM, recall,
and corruption gates. Expensive fuzz, sanitizer, Miri, performance, and
multi-major campaigns are selected by risk and the
[release matrix](release_matrix.md); they are not implied by a passing unit
suite.
