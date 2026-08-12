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
- `semantic_rerank_contract.sh`: runs the frozen eight-query provider-neutral
  rerank workload, invokes the digest-verified no-network worker, validates
  quality, warm/cold latency, RSS, and cost ceilings, and rechecks ACL, forced
  RLS, and source churn. `ROW_COUNT=1000000` is required on PG17 and PG18;
  `ROW_COUNT=10000000` is the retained scheduled release command.
- `document_chunking_worker.sh`: loads a deterministic source corpus, publishes
  a frozen 256-document workload through fenced jobs (including one real
  claim → external worker → stage → publish path), verifies stale-source
  hiding and catalog isolation, and records per-transaction publication
  latency plus persistent-worker token throughput/RSS. `ROW_COUNT` is the
  source-corpus/cardinality lane; `processed_documents` is the explicitly
  bounded publication workload.
  Run `ROW_COUNT=1000000` on PG17 and PG18; `ROW_COUNT=10000000` is the retained
  scheduled release-scale command.
- `document_chunking_recovery.sh`: proves restart durability, expired-lease
  takeover, stale-token fencing, and ready-generation visibility after a second
  restart.
- `document_chunking_non_superuser.sh`: runs registration, enqueue, claim,
  publication, current reads, and retained-profile fallback as a real top-level
  non-superuser with source `SELECT` but no private-catalog privileges.
- `hnsw_replica_promotion.sh`: validates the HNSW exact oracle and the current
  automatic-chunk generation after streaming-replica promotion.
- `build_job_resumability.sh`: validates backend-local build-job interruption,
  retry progress preservation, restart abandonment recovery, supervised dynamic
  worker launch/publication/idle shutdown, disabled-worker fail-open behavior,
  delayed-commit post-commit wake-up, terminated-worker lease takeover, and
  concurrent first-publication serialization, plus reverse-order overlapping
  set-based point mutations without deadlock,
  mmap artifact serving readiness, and source-table recheck after update/delete
  plus VACUUM.
- `artifact_publication_rollback.sh`: validates that a rolled-back mmap artifact
  publication leaves no visible generation, cleanup reconciles its orphan file,
  and a later committed publication becomes serving-ready.
- `automatic_observability.sh`: validates asynchronous automatic telemetry for
  successful, errored, cancelled, budget-exhausted, concurrent-update,
  fallback, missing, rebuild-required, quantized, and corrupt executions; it
  also gates privacy, queue health, disabled-vs-enabled latency, and idle worker
  reclamation.
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
- `indexed_lexical_hybrid.sh`: registers weighted field and JSON-path lexical
  sources, compares the exact path against a direct PostgreSQL `ts_rank_cd`
  oracle, proves GIN and GiST candidate paths return the same ranked answer,
  checks that the canonical index expression is planner-matchable, exercises
  bounded headline hydration and dense+lexical fusion, verifies dump/restore OID
  refresh across a table rewrite, and confirms the complete exact fallback
  survives an index drop. Set `ROW_COUNT` to scale the corpus.
- `adaptive_prefix_recall.sh`: registers a Matryoshka-certified profile and
  proves that every declared prefix reproduces the full-vector ordered answer
  with and without a registered filter, that exhaustive widening is selected
  when it fits, and that candidate saturation selects exact fallback before
  prefix work. It also checks disabled and uncertified controls and reports
  prefix, termination, candidate, recheck, and latency evidence. `ROW_COUNT`
  accepts 500 through 9,960; the frozen Phase 10 manifest records the 1M/10M
  no-go decisions where the scheduler performs zero prefix work.
- `adaptive_prefix_scale.sh`: runs the frozen live scan-based no-go frontier.
  `ROW_COUNT=1000000` proves fail-closed elapsed-budget cancellation with zero
  prefix expansion; the pure manifest pins the 1,000,000-comparison allowance
  and `recheck_budget` decision. `ROW_COUNT=10000000` preserves the larger
  command/report contract and likewise requires fail-closed termination. Run
  the 1M lane on both PG17 and PG18; the 10M lane is an explicit release-scale
  command rather than a default matrix gate.
- `multi_model_coverage.sh`: builds A-only, B-only, dual-covered, and uncovered
  versioned rows and eight held-out queries over independent 4D and 8D model
  spaces. It compares A-only, B-only, equal-weight fused, filtered-partial, and
  degraded curves at the same 102-candidate allowance. The report includes 40
  raw samples, dataset/workload hashes, latency and work summaries, environment
  evidence, and an explicit pass/no-go quality decision. Run
  `ROW_COUNT=1000000` on PG17 and PG18. `ROW_COUNT=10000000` preserves the
  scheduled release-scale command and the same evidence contract.
- `multi_model_rls_acl.sh`: proves multi-model branches preserve forced RLS,
  registered filters, collection membership, and source-table `SELECT`
  revocation without returning a permissive partial result.
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
tests/heavy/automatic_observability.sh
tests/heavy/rls_acl_boundary.sh
tests/heavy/large_exact_search.sh
tests/heavy/partitioned_collections.sh
tests/heavy/low_memory_build.sh
tests/heavy/corrupt_artifact_detection.sh
tests/heavy/indexed_lexical_hybrid.sh
tests/heavy/adaptive_prefix_recall.sh
tests/heavy/sqlstate_contract.sh
```
