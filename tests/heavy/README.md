# Heavy Test Harness

Heavy tests run against a real PostgreSQL cluster managed by `cargo pgrx`.
They are deterministic and destructive for the configured `DBNAME`; each script
drops and recreates that database.

Expected runtime is seconds for smoke, drop, HNSW vacuum, filtered recall, and
concurrent scripts; backup, physical replay, and upgrade scripts can take a few
minutes depending on local PostgreSQL startup and artifact-copy speed.

## Prerequisites

- Pinned Rust toolchain installed from `rust-toolchain.toml`
- PostgreSQL 17 and pgrx configured locally
- `psql` available on `PATH`

## Environment

- `PG_VERSION`: pgrx PostgreSQL version label, default `pg17`
- `PG_FEATURE`: context-pg feature used for installation, default `pg17`
- `PG_CONFIG`: path to `pg_config`, default Homebrew PostgreSQL 17
- `PGHOST`: PostgreSQL host, default `localhost`
- `PGPORT`: PostgreSQL port, default `28817`
- `DBNAME`: database name; each script has a safe default

## Failure Triage

Scripts exit on the first failing command or SQL assertion. Re-run the failing
script with the same `DBNAME` to reproduce from a clean database. PostgreSQL
cluster logs are written by pgrx under the configured pgrx data directory; scripts
that create secondary clusters preserve their temporary data and log paths on
failure.

## Lifecycle Scripts

- `fresh_install_smoke.sh`: starts pgrx PostgreSQL, installs the extension,
  creates a clean database, executes the quickstart collection/search/query
  flow, drops the user table, and drops the extension.
- `drop_extension_survival.sh`: proves a user-owned source table that does not
  depend on extension-owned types survives `DROP EXTENSION pgcontext`.
- `upgrade_matrix.sh`: installs the current release and each checked-in previous
  SQL release when present, stages previous install SQL into PostgreSQL's
  extension directory for the run, loads representative state, validates
  extension update behavior, verifies default privileges, and proves
  install/update do not mutate or index unrelated user-owned tables.
- `backup_restore.sh`: builds representative source and pgContext catalog
  state, dumps the database, restores into a clean database, and validates the
  restored query, catalog, telemetry, migration, and HNSW metadata state.
- `cross_version_import.sh`: dumps representative source, dense, sparse,
  telemetry, model, migration, and HNSW state for each installable SQL version,
  restores it into a clean database, updates to the current extension version,
  and validates imported metadata and query behavior.
- `physical_backup_wal_replay.sh`: takes a streaming physical base backup,
  starts the copied data directory on an alternate port, validates indexed and
  non-indexed collections, forces an immediate stop, restarts, and verifies WAL
  replay preserves query behavior.
- `crash_restart_hnsw.sh`: exercises insert, update, delete, VACUUM, REINDEX,
  and exact-oracle order for dense, halfvec, sparsevec, and bitvec HNSW metrics,
  restarts the pgrx PostgreSQL cluster, and rechecks every metric's index-backed
  order.
- `mapped_hnsw_lifecycle_cleanup.sh`: proves mapped index generations survive
  rolled-back DDL and prepared-transaction abort, while committed DROP INDEX,
  prepared-transaction commit, cascading DROP TABLE, explicit and
  session-teardown temporary-index drops, and DROP DATABASE are reclaimed. The
  gate also proves crash-durable markers, bounded/fair retries across fresh
  backends, and progress past stale publication temps with 33 unresolved
  prepared drops. It temporarily enables prepared transactions on its isolated
  pgrx server and restores the normal launch configuration on exit.
- `pgvector_hnsw_lifecycle.sh`: the bounded V1 dense-metric launch gate; forces
  L2, inner-product, cosine, and L1 index plans through DML, VACUUM, REINDEX,
  restart, and exact-order comparison.
- `hnsw_vacuum.sh`: exercises update, delete, insert, `VACUUM (ANALYZE)`,
  `REINDEX INDEX`, `index_status`, `vacuum_advice`, and ordered HNSW lookup
  after cleanup and rebuild.
- `hnsw_relation_kinds.sh`: pins logged, unlogged, and temporary HNSW index
  persistence catalog values and forced ordered lookup behavior.
- `concurrent_read_write.sh`: runs concurrent psql reader and writer sessions
  against an HNSW-indexed table, then verifies inserted rows are indexed.
- `filtered_ann_recall.sh`: compares filtered HNSW order against an exact
  filtered top-k fixture through `pgcontext.recall_check`, including no-match
  filters.
- `late_interaction_ann_serving.sh`: validates late-interaction ANN token
  candidate serving with HNSW token-table candidates, deduplicated source keys,
  exact MaxSim source-table rerank, deleted-point filtering, and comparison
  budget rejection.
- `build_job_resumability.sh`: validates backend-local build-job interruption,
  retry progress preservation, restart abandonment recovery, mmap artifact
  serving readiness, and source-table recheck after update/delete plus VACUUM.
- `artifact_publication_rollback.sh`: validates that a rolled-back mmap artifact
  publication leaves no visible generation, cleanup reconciles its orphan file,
  and a later committed publication becomes serving-ready.
- `rls_acl_boundary.sh`: validates source-table ACL and forced RLS boundaries
  against pgContext search from owner and non-owner roles.
- `large_exact_search.sh`: loads a deterministic exact-search collection,
  compares `pgcontext.search` against a direct SQL distance oracle, verifies
  filter no-match behavior, and checks representative bad-path SQLSTATEs. Set
  `LARGE_EXACT_FULL=1` to run the million-row release mode.
- `partitioned_collections.sh`: validates search, filtered search, count, facet,
  deleted point exclusion, and dropped-partition source-row exclusion for a
  list-partitioned source table.
- `low_memory_build.sh`: forces a constrained HNSW construction-budget failure,
  verifies no failed index is left behind, then builds with small valid HNSW
  settings and checks ordered index lookup.
- `corrupt_artifact_detection.sh`: runs the storage segment-format gate covering
  malformed headers, checksum mismatches, truncated payloads, atomic replacement,
  and import/export rejection for corrupted rebuildable artifacts.
- `sqlstate_contract.sh`: runs the pgrx SQLSTATE contract module against the
  configured PostgreSQL version.

Run from the repository root:

```sh
tests/heavy/fresh_install_smoke.sh
tests/heavy/drop_extension_survival.sh
tests/heavy/upgrade_matrix.sh
tests/heavy/backup_restore.sh
tests/heavy/cross_version_import.sh
tests/heavy/physical_backup_wal_replay.sh
tests/heavy/crash_restart_hnsw.sh
tests/heavy/mapped_hnsw_lifecycle_cleanup.sh
tests/heavy/hnsw_vacuum.sh
tests/heavy/concurrent_read_write.sh
tests/heavy/filtered_ann_recall.sh
tests/heavy/late_interaction_ann_serving.sh
tests/heavy/build_job_resumability.sh
tests/heavy/artifact_publication_rollback.sh
tests/heavy/rls_acl_boundary.sh
tests/heavy/large_exact_search.sh
tests/heavy/partitioned_collections.sh
tests/heavy/low_memory_build.sh
tests/heavy/corrupt_artifact_detection.sh
tests/heavy/sqlstate_contract.sh
```
