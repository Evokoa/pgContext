# Rollback and Repair Plan

PostgreSQL source tables are authoritative. pgContext catalogs and acceleration
artifacts can be repaired or rebuilt, but user-owned source rows should be
recovered with ordinary PostgreSQL backup and restore procedures.

## Extension Upgrade Failure

If `ALTER EXTENSION pgcontext UPDATE` fails:

- leave the transaction aborted and inspect the PostgreSQL error and SQLSTATE;
- retry only after fixing the reported catalog, privilege, or dependency issue;
- restore from backup when the failed upgrade was part of a larger migration
  transaction that also changed application schema;
- do not manually edit pgContext catalog tables unless a release note gives an
  exact repair statement for the affected version.

Upgrade scripts must not scan user data, mutate user source tables, or start
index builds. After a successful retry, run smoke checks for collection
metadata, exact search, filters, telemetry, and deployed indexes.

## Failed Index Build

If an HNSW or future pgContext index build fails:

- drop the failed index relation if PostgreSQL left one behind;
- keep serving exact search or the previous valid index path;
- lower memory or candidate-budget settings only when the release notes or
  `pgcontext.vacuum_advice` indicate the failure is resource-related;
- rebuild with `CREATE INDEX` or `REINDEX`, then validate with exact search and
  `pgcontext.recall_check` before controlled rollout. Keep exact search when the
  experimental HNSW path does not meet measured workload requirements.

## Corrupt or Missing Artifacts

If diagnostics report `index_corrupt`, `artifact_missing`, or a related typed
status:

- treat the artifact as disposable acceleration state;
- keep PostgreSQL source tables online;
- use `pgcontext.retire_artifact_segment(artifact_id)` to retire manifests and
  attempt cleanup for generated, root-confined experimental segment files that
  should be discarded before a rebuild;
- rebuild the artifact from source tables and pgContext catalog metadata;
- use PostgreSQL backup/restore for authoritative data recovery, not artifact
  files.

## Compatibility Regression

If a release changes a documented SQLSTATE, return column, status value, or
stable API behavior unexpectedly:

- pin clients to the last known good extension version;
- record the failing SQL, expected output, actual output, SQLSTATE, server
  version, and extension version;
- check release notes for an explicit waiver or migration path;
- treat unwaived stable-surface regressions as release blockers.

Clients should branch on documented SQLSTATEs and typed status fields instead
of parsing free-form PostgreSQL messages.
