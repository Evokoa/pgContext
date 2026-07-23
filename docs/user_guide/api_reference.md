# SQL API Contract

pgContext uses one contract registry in `context-pg` to classify SQL-visible
objects as stable, experimental, or internal. A compatibility check compares
installed `pgcontext` functions against that registry so new SQL functions
cannot be added without a compatibility decision.

## Compatibility Policy

The full support, version, upgrade, and deprecation policy lives in
[`support_policy.md`](support_policy.md). The summary below applies to stable
SQL objects listed in this reference.

Stable SQL functions, types, operators, casts, status values, and SQLSTATE
categories follow semantic extension-version compatibility after the first
production release. Patch releases may add optional fields, functions, enum
values, and diagnostics, but must not remove or repurpose stable SQL objects.

Breaking changes require a new extension version, upgrade notes, and a named
migration path. A change is breaking when it removes a stable SQL object,
changes a stable function signature or return column, changes the meaning of a
stable status value, changes a documented SQLSTATE for the same failure class,
or makes previously valid stable input fail without an explicit migration.

Deprecated stable APIs remain callable for at least one minor release after
the replacement is documented. Deprecation notices belong in the user guide,
release notes, and upgrade guide; clients should not need to infer deprecation
from free-form PostgreSQL messages.

Diagnostic rows and telemetry use stable column names, typed status values, and
numeric counters. Human-readable messages may become more specific, so clients
must branch on SQLSTATEs and documented status fields instead of parsing text.
Experimental and internal objects are excluded from this compatibility promise.

## Stable User APIs

Collection and registration:

- `pgcontext.create_collection(collection_name text)`
- `pgcontext.create_collection(collection_name text, table_name text)`
- `pgcontext.create_collection_alias(alias_name text, target_collection_name text)`
- `pgcontext.collection_aliases()`
- `pgcontext.collection_info(collection_name text)`
- `pgcontext.collection_limits(collection_name text)`
- `pgcontext.configure_collection_limits(collection_name text, strict_mode boolean, max_dimensions integer, max_vectors integer, max_points bigint, max_filter_nodes integer, max_search_limit integer, max_candidate_budget integer, query_timeout_ms integer, max_index_memory_bytes bigint)`
- `pgcontext.drop_collection(collection_name text)`
- `pgcontext.drop_collection_alias(alias_name text)`
- `pgcontext.register_vector(collection_name text, vector_name text, vector_column text, dimensions integer, metric text)`
- `pgcontext.collection_vectors(collection_name text)`
- `pgcontext.configure_vector(collection_name text, vector_name text, hnsw_options jsonb, quantization_options jsonb, status text)`
- `pgcontext.register_filter_column(collection_name text, filter_key text, column_name text)`
- `pgcontext.register_jsonb_path(collection_name text, filter_key text, column_name text, path text[])`
- `pgcontext.upsert_points(collection_name text, source_keys text[])`
- `pgcontext.delete_points(collection_name text, source_keys text[])`
- `pgcontext.bulk_upsert_points(collection_name text, source_keys text[], batch_size integer)`
- `pgcontext.bulk_delete_points(collection_name text, source_keys text[], batch_size integer)`
- `pgcontext.backfill_points(collection_name text, batch_size integer)`
- `pgcontext.set_payload(collection_name text, source_keys text[], payload jsonb)`
- `pgcontext.delete_payload(collection_name text, source_keys text[], payload_keys text[])`
- `pgcontext.clear_payload(collection_name text, source_keys text[])`

Search and query:

- `pgcontext.search(collection text, vector vector, limit integer)`
- `pgcontext.search(collection text, vector_name text, vector vector, limit integer)`
- `pgcontext.search(collection text, vector vector, filter text, limit integer)`
- `pgcontext.search(collection text, vector_name text, vector vector, filter text, limit integer)`
- `pgcontext.search(collection text, vector vector, candidate_point_ids bigint[], limit integer)`
- `pgcontext.search(collection text, vector_name text, vector vector, candidate_point_ids bigint[], limit integer)`
- `pgcontext.search(collection text, vector vector, filter text, candidate_point_ids bigint[], limit integer)`
- `pgcontext.search(collection text, vector_name text, vector vector, filter text, candidate_point_ids bigint[], limit integer)`
- `pgcontext.search(query vector, point_ids bigint[], vectors vector[], metric text, limit integer)`
- `pgcontext.recommend(collection text, positive_point_ids bigint[], negative_point_ids bigint[], limit integer)`
- `pgcontext.recommend(collection text, positive_vectors vector[], negative_vectors vector[], limit integer)`
- `pgcontext.discover(collection text, context_point_ids bigint[], limit integer)`
- `pgcontext.explore(collection text, context_point_ids bigint[], limit integer)`
- `pgcontext.query(collection text, vector vector, text_query text, text_column text, limit integer)`
- `pgcontext.query_nearest(vector vector, limit integer)`
- `pgcontext.query_nearest(vector_name text, vector vector, filter jsonb, limit integer)`
- `pgcontext.query_sparse_nearest(vector_name text, vector sparsevec, filter jsonb, limit integer)`
- `pgcontext.query_sparse_nearest(vector_name text, vector sparsevec, limit integer)`
- `pgcontext.query_full_text(text_query text, text_column text, limit integer)`
- `pgcontext.query_late_interaction(query_vectors vector[], candidates_per_query integer, limit integer)`
- `pgcontext.query_recommend(positive_point_ids bigint[], negative_point_ids bigint[], limit integer)`
- `pgcontext.query_discover(context_point_ids bigint[], limit integer)`
- `pgcontext.query_lookup(point_ids bigint[])`
- `pgcontext.query_prefetch(branches jsonb[])`
- `pgcontext.query_weight(branch jsonb, weight double precision)`
- `pgcontext.query_score_threshold(branch jsonb, min_score double precision, max_score double precision)`
- `pgcontext.query_formula(branch jsonb, formula text)`
- `pgcontext.query_rerank(branch jsonb, limit integer)`
- `pgcontext.execute_query(collection text, plan jsonb)`
- `pgcontext.explain(collection text, text_column text)`
- `pgcontext.scroll(collection text, cursor text, limit integer)`
- `pgcontext.count(collection text)`
- `pgcontext.count(collection text, filter text)`
- `pgcontext.facet(collection text, field text, filter text, limit integer)`
- `pgcontext.grouped_search(collection text, vector vector, group_by text, group_limit integer, limit integer)`
- `pgcontext.grouped_search(collection text, vector_name text, vector vector, group_by text, group_limit integer, limit integer)`

`pgcontext.search` is the stable single-vector retrieval surface. Use it for
exact or ANN-style nearest-neighbor retrieval over one dense vector branch,
including filter-first search, candidate recheck, and grouped exact search by a
registered payload field. `pgcontext.query` is the multi-stage retrieval
pipeline: in the first production surface it fuses one registered dense vector
branch with one PostgreSQL full-text branch. Experimental overloads also expose
exact dense+sparse RRF fusion. Additional ANN sparse branch planners and broader
multi-branch planning remain deferred instead of being hidden behind `search`.

Dense vector compatibility:

- SQL type `vector`, including `vector(n)` catalog metadata for dimensions
  `1..=16000`; assignments with a different dimension fail with SQLSTATE
  `22023`
- `pgcontext.l2_distance(left vector, right vector)`
- `pgcontext.inner_product(left vector, right vector)`
- `pgcontext.negative_inner_product(left vector, right vector)`
- `pgcontext.cosine_distance(left vector, right vector)`
- `pgcontext.l1_distance(left vector, right vector)`
- `pgcontext.vector_dims(vector vector)`
- Operators `<->`, `<#>`, `<=>`, `<+>`, `<`, `<=`, `=`, `<>`, `>=`, and `>`
- B-tree operator class `pgcontext.vector_ops`
- Aggregates `pgcontext.sum(vector)` and `pgcontext.avg(vector)`
- An assignment cast from `real[]` to `vector`; explicit-only casts from
  `integer[]` and `double precision[]` reject elements that are not exactly
  representable as `real`; and an assignment cast from `vector` to `real[]`

Operations, diagnostics, and telemetry:

- `pgcontext.index_status(index_name text)`
- `pgcontext.index_diagnostics(index_name text)`
- `pgcontext.estimate_index_memory(index_name text)`
- `pgcontext.index_advisor(collection text)`
- `pgcontext.optimization_status(collection text)`
- `pgcontext.vacuum_advice(index_name text)`
- `pgcontext.hnsw_serving_stats()` — this backend's packed-generation
  serving counters: `pack_builds`, `pack_reuses`, `last_pack_bytes`,
  `last_pack_millis`, `total_pack_millis`, `shared_attaches`,
  `shared_publishes`, `shared_publish_skips`, `mapped_attaches`,
  `mapped_publishes`, `mapped_publish_skips`, `page_native_fallbacks`,
  `delta_segment_records`, `delta_segment_scans`. Local pack/reuse counters describe the calling
  backend only; `shared_*` counters describe this backend's activity
  against the cross-backend shared registry (see
  `pgcontext.hnsw_shared_serving`), while `mapped_*` counters describe
  immutable file-generation serving (see `pgcontext.hnsw_mmap_serving`);
  `page_native_fallbacks` counts queries served from unpacked directory
  reads because no pack was available and
  `pgcontext.hnsw_pack_on_first_use` was off; `delta_segment_records` and
  `delta_segment_scans` describe the persisted segmented-write delta region
  (see `pgcontext.hnsw_delta_segment_limit`) — rows absorbed without a
  graph splice and scans that merged the region with base-graph results.
- `pgcontext.hnsw_build_stats()` — phase timing of this backend's most
  recent HNSW bulk build: `last_build_tuples`, `graph_millis` (heap scan
  plus in-memory graph construction), `write_millis` (snapshot extraction,
  page writes, Generic-WAL emission). All zeros before the first build.
- `pgcontext.recall_check(exact_point_ids bigint[], candidate_point_ids bigint[], min_recall double precision)`
- `pgcontext.telemetry()`
- `pgcontext.record_query_stat(collection text, cohort text, query_kind text, result_count bigint, candidate_count bigint, latency_ms double precision)`
- `pgcontext.record_query_stat(collection text, cohort text, query_kind text, result_count bigint, candidates_considered bigint, rows_rechecked bigint, rows_pruned bigint, recall_threshold double precision, recall_achieved double precision, latency_ms double precision, lifecycle_state pgcontext."QueryLifecycleState")`
- `pgcontext.query_cohort_stats()`
- `pgcontext.query_execution_stats()` — membership-filtered automatic rollups
  by actual strategy, completion, latency bucket, lifecycle state, and bounded
  executor work counters
- `pgcontext.query_telemetry_queue_stats()` — `pg_monitor`-restricted health
  counters for the bounded asynchronous delivery queue
- `pgcontext.register_model_version(collection text, model_name text, model_version text, dimensions integer, metric text)`
- `pgcontext.model_versions()`
- `pgcontext.create_embedding_migration(collection text, source_model_name text, source_model_version text, target_model_name text, target_model_version text, total_points bigint)`
- `pgcontext.update_embedding_migration(migration_id bigint, processed_points bigint, status text)`
- `pgcontext.embedding_migrations()`

Stable status values use the SQL enum labels below. String inputs that update
catalog state, such as `pgcontext.update_embedding_migration(..., status text)`,
may accept lowercase command strings, but result rows expose the typed enum
labels.

- `pgcontext."EmbeddingMigrationStatus"`: `Planned`, `Running`, `Completed`, `Failed`
- `pgcontext."IndexAdvisorRecommendation"`: `NoAction`, `CreateBtreeIndex`,
  `CreateGinIndex`, `AnalyzeTable`, `AvoidCandidateMaterialization`,
  `TuneHnswSettings`
- `pgcontext."IndexDiagnosticStatus"`: `Ready`, `IndexNotReady`, `IndexCorrupt`,
  `UnsupportedAccessMethod`
- `pgcontext."IndexLifecycleStatus"`: `Ready`, `Building`, `Invalid`
- `pgcontext."IndexMemoryEstimateStatus"`: `Projected`, `UnsupportedAccessMethod`,
  `UnavailableStatistics`
- `pgcontext."OptimizationStatus"`: `Indexed`, `ExactOnly`, `MissingArtifacts`,
  `StaleCatalog`
- `pgcontext."QueryCohortStatus"`: `Observed`
- `pgcontext."QueryExplainStatus"`: `Ready`, `Fallback`, `Policy`
- `pgcontext."QueryLatencyBucket"`: `Lt1Ms`, `Lt10Ms`, `Lt100Ms`, `Lt1S`, `Gte1S`,
  `Unspecified`
- `pgcontext."QueryLifecycleState"`: `Unspecified`, `Exact`, `Indexed`, `Fallback`,
  `IndexNotReady`, `IndexCorrupt`, `ArtifactMissing`
- `pgcontext."RecallCheckStatus"`: `Passing`, `Failing`, `EmptyExact`
- `pgcontext."TelemetryStatus"`: `Active`, `Empty`, `MissingArtifacts`, `StaleCatalog`
- `pgcontext."VacuumAdviceStatus"`: `Healthy`, `VacuumRecommended`,
  `AnalyzeRecommended`, `UnsupportedAccessMethod`

Experimental build-job metadata uses the `pgcontext."BuildJobStatus"` labels `Planned`,
`Running`, `CancelRequested`, `Cancelled`, `Completed`, `Failed`, and
`Abandoned`. These rows track backend-local progress and operator intent for
future artifact builds; they do not make `CREATE INDEX` resumable and they do
not make a partially built artifact query-safe.

## GUCs

These HNSW tuning GUCs are SQL-visible for the experimental HNSW serving path.
They are outside the first stable SQL compatibility promise until HNSW serving
graduates from the experimental parity row.

- `pgcontext.hnsw_m`
- `pgcontext.hnsw_ef_construction`
- `pgcontext.hnsw_ef_search`
- `pgcontext.hnsw_candidate_budget`
- `pgcontext.hnsw_iterative_expansion_limit`
- `pgcontext.hnsw_recall_threshold`
- `pgcontext.pgvector_compat_warnings` (coexist-mode advisory notice;
  see [pgvector_coexist.md](pgvector_coexist.md))

## Experimental APIs

pgvector coexist-mode tooling (see [pgvector_coexist.md](pgvector_coexist.md)
for semantics and caveats):

- `pgcontext.migration_report()` returns one row per pgvector-typed
  column: `(schema_name text, table_name text, column_name text,
  type_name text, dimensions int, pgvector_indexes text[],
  pgcontext_indexes text[], conversion_supported bool, blockers text[],
  suggested_command text)`. Read-only. The blockers are a fail-closed
  inventory for ownership conversion; array, generated, partitioned,
  dependent-view, defaulted, and complex-index shapes are reported rather
  than guessed.
- `pgcontext.adopt_pgvector(target regclass DEFAULT NULL, dry_run bool
  DEFAULT true, drop_old bool DEFAULT false)` returns
  `(index_name text, action text, command text, executed bool)` rows;
  migrates pgvector `hnsw`/`ivfflat` indexes to `pgcontext_hnsw`
  equivalents through the `pgcontext_pgvector` companion bridge. Dry-run by
  default; execution fails closed when the bridge is absent. Only
  extension-owned, usable, plain
  single-column indexes are accepted. HNSW build options and tablespace are
  preserved. If `drop_old` is requested, the replacement must first pass an
  exact-oracle recall gate; a failure aborts the transaction without dropping
  the source index.
- `pgcontext.compare_indexes(table_name text, column_name text,
  queries int DEFAULT 20)` returns one row per ANN index on the column:
  `(index_name text, access_method text, operator text, p50_ms float8,
  p95_ms float8, recall_at_10 float8)`, measured with sampled stored
  vectors against an exact same-operator oracle; indexes the planner
  never chose report NULL measurements. Read-only.
- `pgcontext.enable_pgvector_binding()` always raises
  `feature_not_supported` with companion-bridge guidance. pgContext and
  pgvector themselves can be installed in either order.
- `pgcontext.start_pgvector_ownership_conversion(target regclass,
  column_name text, mode text DEFAULT 'fast', metric text DEFAULT 'cosine',
  application_uses_column_lists bool DEFAULT false,
  application_dependencies_reviewed bool DEFAULT false)` starts a persisted
  conversion for a table-owner role. `mode` is `fast` or
  `restricted_online`. Online mode immediately adds a canonical shadow column
  and synchronization trigger, so its explicit column-list attestation is
  mandatory. Every mode requires the application-dependency attestation
  because PostgreSQL cannot discover relation references hidden in application
  SQL or string-bodied SQL/PLpgSQL functions. The caller must have `CREATE` on
  the target schema (and any preserved nondefault index tablespace) whenever
  the conversion builds replacement indexes.
- `pgcontext.run_pgvector_ownership_conversion(conversion_id bigint,
  batch_size int DEFAULT 1000, sessions_drained bool DEFAULT false)` performs
  one bounded step. Fast mode requires the session-drain attestation and
  completes atomically. Online mode backfills at most `batch_size` mismatches;
  once backfill is complete, `next_command` contains a `CREATE INDEX
  CONCURRENTLY` command that must be run as its own top-level statement. Call
  `run_pgvector_ownership_conversion` again to certify that index and advance
  the job to `ready`.
- `pgcontext.cutover_pgvector_ownership_conversion(conversion_id bigint,
  sessions_drained bool DEFAULT false)` performs the locked online name swap
  after exact row validation and requires all application sessions to have
  been drained/reprepared. `pgcontext.finalize_pgvector_ownership_conversion`
  then irreversibly drops the synchronized pgvector rollback column, while
  `pgcontext.rollback_pgvector_ownership_conversion` restores the original
  column and indexes before finalization.
- `pgcontext.pgvector_ownership_conversions()` lists only jobs owned by a role
  of which `SESSION_USER` is a member. The private job catalog is not dumped;
  in-flight relation/type OIDs are intentionally never resumed after restore.

Ownership conversion is deliberately restricted to permanent ordinary heap
tables and directly pgvector-owned `vector`/`halfvec` columns. It refuses
unsupported defaults, generated/dependent expressions, column ACLs, views,
catalog-discoverable function dependencies, constraints, RLS policies, user triggers, publications, extended
statistics, partitions/inheritance, replica identity, composite dependencies,
column comments/nondefault storage/statistics, and complex or counterfeit
indexes. Source HNSW indexes with per-index options are refused because the
current `pgcontext_hnsw` AM cannot represent them; IVFFlat list options are
intentionally discarded during the documented rebuild-as-HNSW conversion.
Invalid indexes and indexes with comments are refused rather than silently
normalizing or losing metadata. Fast conversion uses a binary metadata type
change and rebuilds certified source ANN indexes as HNSW; for a dimensioned
source it preserves the dimension invariant with a validated CHECK constraint
so the heap is not rewritten. Restricted-online conversion supports at most one
source ANN index and requires its metric to match the requested replacement.

Index maintenance:

- `pgcontext.compact(index regclass)` returns one row,
  `(live_rows bigint, base_records_read bigint, delta_records_drained
  bigint)`. Rebuilds a `pgcontext_hnsw` index's graph from the index's own
  pages — never the heap — and reopens an empty delta segment, restoring the
  fast insert path once
  `pgcontext.hnsw_delta_segment_limit` writes have accumulated. Results are
  unchanged by a compaction.

  It publishes the rebuilt graph with a single atomic metapage update, so
  concurrent readers see either the old graph or the new one, never a partial
  result. Two limits: the superseded pages stay in place, so compaction
  restores write throughput without shrinking the relation on disk (use
  `REINDEX` for that), and it can only drop deleted rows that a preceding
  `VACUUM` has tombstoned.

  It takes `ShareUpdateExclusiveLock` on the parent table for the rest of the
  transaction, which is the lock `VACUUM` holds, so the two wait for each other
  rather than interleaving. Ordinary `INSERT`/`UPDATE`/`DELETE` are not blocked.
  If it nonetheless observes a concurrent index mutation it raises
  `serialization_failure` and changes nothing.

  By default an insert that fills the delta segment runs this same compaction
  itself, so calling it explicitly is only needed when
  `pgcontext.hnsw_compact_on_threshold` is off — see
  [index configuration](indexes.md).

`pgcontext_hnsw`, its handler function, and four dense-vector operator classes
are SQL-visible for ongoing access-method work:

- `pgcontext.vector_hnsw_ops` for L2 (the default)
- `pgcontext.vector_hnsw_ip_ops` for inner-product ordering
- `pgcontext.vector_hnsw_cosine_ops` for cosine distance
- `pgcontext.vector_hnsw_l1_ops` for L1 distance

They are not yet covered by the first production compatibility promise.

Quantization helpers are SQL-visible for inspecting and testing encoded
representations:

- `pgcontext.binary_quantize(vector)` returns a `bitvec` sign code.
- `pgcontext.scalar_quantize(vector, min real, max real, levels integer)`
  returns scalar/SQ8-style byte codes.

Backend-local build metadata is experimental and owner-scoped:

- `pgcontext.start_build_job(collection text, artifact_kind text,
  artifact_name text, target_name text, total_units bigint)` creates a running
  job row for one PostgreSQL backend. If a previous `running` or
  `cancel_requested` row for the same target belongs to a backend that no
  longer appears in `pg_stat_activity`, it is first recorded as `abandoned` so
  a replacement build can start without manual catalog repair.
- `pgcontext.build_jobs(collection text)` lists visible build jobs for a
  collection.
- `pgcontext.update_build_job(build_job_id bigint, processed_units bigint,
  status text, error_message text default null)` records progress or a terminal
  status from the backend that owns the running job.
- `pgcontext.request_build_cancel(build_job_id bigint)` records cooperative
  cancellation intent.
- `pgcontext.retry_build_job(build_job_id bigint)` claims a failed, cancelled,
  or abandoned job for the current backend while preserving recorded progress.
- `pgcontext.run_build_job(build_job_id bigint, units_per_step bigint default 1)`
  executes experimental `segment` and `mmap` jobs synchronously in the current
  backend, advances bounded progress, honors already-visible cancellation, and
  records a terminal status.
- `pgcontext.encode_artifact_segment(kind text, payload bytea)` encodes an
  experimental rebuildable artifact byte stream with a versioned segment header
  and checksum. The currently exposed kind is `hnsw_graph`.
- `pgcontext.validate_artifact_segment(segment bytea)` validates an encoded
  segment through the mmap-safe loader and returns kind, payload length, and
  checksum metadata without marking the artifact query-safe.
- `pgcontext.validate_hnsw_graph_artifact(segment bytea)` validates both the
  outer segment header and the portable HNSW graph payload format, returning
  record count, dimensions, and base-neighbor count. Malformed graph payloads
  raise data-corruption SQLSTATE instead of being accepted as serving input.
- `pgcontext.publish_artifact_segment(build_job_id bigint, segment bytea)`
  validates encoded segment bytes for a completed visible `segment` or `mmap`
  build job and records manifest metadata only.
- `pgcontext.publish_artifact_segment_file(build_job_id bigint, segment bytea)`
  validates encoded segment bytes, atomically materializes them under a
  generated PostgreSQL data-directory-relative `pgcontext_artifacts/...` path,
  reloads the file through the validator, and records the generated relative
  path with `file_materialized` lifecycle state.
- `pgcontext.artifact_segments(collection text)` lists visible validated
  artifact manifests for a collection, including generated relative paths for
  file-materialized artifacts.
- `pgcontext.artifact_segment_memory(collection text)` reports per-artifact
  payload bytes, segment header bytes, total mapped bytes, lifecycle state, and
  whether the artifact has a materialized file path.
- `pgcontext.artifact_segment_serving_readiness(collection text,
  max_mapped_bytes bigint)` reloads visible artifact files and reports whether
  each artifact is safe for the mmap serving path under the supplied memory
  budget. It never serves vectors; it gates on `mmap` kind, file-materialized
  lifecycle, root-confined paths, segment checksum/catalog metadata agreement,
  and mapped bytes within budget.
- `pgcontext.artifact_segment_mmap_payload(collection text, artifact_name text,
  max_mapped_bytes bigint)` reloads one visible `mmap` artifact through the same
  readiness gate and returns its validated payload bytes only when the file is
  serving-ready under the supplied memory budget. Missing, corrupt, drifted,
  path-escaped, metadata-only, non-`mmap`, or over-budget artifacts fail closed
  with a prerequisite-state error instead of returning bytes. This is an
  experimental loader primitive, not a stable vector search API or durable HNSW
  artifact payload contract.
- `pgcontext.search_mmap_hnsw_artifact(collection text, artifact_name text,
  vector vector, max_mapped_bytes bigint, candidate_limit integer,
  limit integer)` is an experimental mmap serving slice: it opens a
  serving-ready HNSW graph artifact through a validated read-only OS mapping,
  traverses persisted graph links, merges source points added after the
  generation high-water mark, and returns source-table rechecked rows scored
  from the authoritative registered vector column. Corrupt artifact payloads
  raise data-corruption SQLSTATE; not-ready artifacts fail closed with
  prerequisite-state errors.
- `pgcontext.artifact_segment_diagnostics(collection text)` reloads each
  visible file-materialized artifact through the segment loader, rejects catalog
  paths outside `pgcontext_artifacts/...`, and compares file metadata with the
  catalog. The constrained `status` text is one of `ready`, `metadata_only`,
  `artifact_missing`, `checksum_mismatch`, `artifact_corrupt`,
  `metadata_mismatch`, or `path_rejected`. The `repair_advice` column is
  deterministic: `ready` means no action, metadata-only, retired, or pathless
  manifests are not cleanup candidates, `path_rejected` means fix or remove the
  invalid catalog path before cleanup, and missing, corrupt, checksum-drifted,
  or metadata-drifted artifacts should be retired or rebuilt after
  investigation. The `cleanup_eligible` column is true only for root-confined
  materialized artifact paths that are eligible for
  `retire_artifact_segment` cleanup.
- `pgcontext.retire_artifact_segment(artifact_id bigint)` marks the visible
  manifest `retired` and then attempts to remove its generated, root-confined
  materialized artifact file. Missing files are tolerated; catalog paths outside
  `pgcontext_artifacts/...` are rejected without touching the filesystem.
- `pgcontext.cleanup_artifact_segments(collection text, dry_run boolean)`
  reports or retires manifest-known materialized files that are missing,
  corrupt, checksum-drifted, or metadata-drifted. It also reports or removes
  regular orphan `.pgctxseg` files under the generated per-collection artifact
  directory when no visible manifest references them. Orphan rows use
  `artifact_id = 0`, `status = 'orphaned_file'`, and
  `lifecycle_state = 'orphaned_file'`. Symlinks, directories, and non-segment
  files are skipped.

The metadata is stored in extension catalogs and is useful for explicit
operator workflows. It is not a shared Rust worker queue, and it is not a stable
serving contract for HNSW, segment, or mmap artifacts. File materialization is
an experimental publication primitive; it does not make artifacts query-safe.
- `pgcontext.scalar_reconstruct(codes bytea, min real, max real, levels integer)`
  reconstructs scalar byte codes to `vector`.
- `pgcontext.product_quantize(vector, subvector_dimensions integer, codebooks jsonb)`
  returns product-quantization byte codes using JSONB centroid codebooks.
- `pgcontext.product_reconstruct(codes bytea, subvector_dimensions integer, codebooks jsonb)`
  reconstructs product byte codes to `vector`.
- `pgcontext.rerank_quantized_candidates(query vector, point_ids bigint[], original_vectors vector[], metric text, limit integer)`
  treats `point_ids` as approximate candidates only, requires one original dense
  vector per candidate, and returns exact rerank scores in final SQL order.

Experimental vector variants are SQL-visible so users can test parsing,
validation, and exact scoring outside the stable compatibility promise:

- SQL types `halfvec`, `sparsevec`, and `bitvec`
- Pgvector-style `halfvec(n)`, `sparsevec(n)`, and `bitvec(n)` typmods with dimension
  enforcement on assignment
- Text constructors `pgcontext.halfvec(text)`, `pgcontext.sparsevec(text)`,
  and `pgcontext.bitvec(text)`
- Dimension helpers `pgcontext.halfvec_dims(halfvec)`,
  `pgcontext.sparsevec_dims(sparsevec)`, and `pgcontext.bitvec_dims(bitvec)`
- Exact half-vector distance functions for L2, inner product, negative inner
  product, cosine, and L1
- Explicit-only, rounding half-vector casts from `real[]`, `integer[]`, and
  `double precision[]` to `halfvec`, and an assignment cast from `halfvec` to
  `real[]`
- Half-vector aggregates `pgcontext.sum(halfvec)` and `pgcontext.avg(halfvec)`
- Sparse-vector array constructor
  `pgcontext.sparsevec_from_arrays(integer[], real[], integer)` plus
  `pgcontext.sparsevec_indices(sparsevec)` and
  `pgcontext.sparsevec_values(sparsevec)` accessors
- Sparse-vector casts between `real[]`, dense `vector`, and `sparsevec`
- Sparse-vector aggregates `pgcontext.sum(sparsevec)` and
  `pgcontext.avg(sparsevec)`
- Exact sparse-vector distance functions for L2, inner product, negative inner
  product, cosine, and L1
- Bit-vector Hamming and Jaccard distance functions
- Bit-vector casts from `boolean[]` to `bitvec` and from `bitvec` to
  `boolean[]`
- Bit-vector casts from PostgreSQL `bit` and `bit varying` to `bitvec`, and
  from `bitvec` to PostgreSQL `bit` and `bit varying`
- Pgvector-compatible built-in `bit` Hamming and Jaccard distance functions,
  plus `<~>` and `<%>` operator overloads
- Bit-vector aggregates `pgcontext.bit_or(bitvec)` and
  `pgcontext.bit_and(bitvec)`
- Distance operator overloads for halfvec (`<->`, `<#>`, `<=>`, `<+>`),
  sparsevec (`<->`, `<#>`, `<=>`, `<+>`), and bitvec (`<~>`, `<%>`)
- Default btree ordering opclasses `pgcontext.halfvec_ops`,
  `pgcontext.sparsevec_ops`, and `pgcontext.bitvec_ops` for deterministic
  equality, comparison, and ordinary PostgreSQL btree indexes

The non-dense HNSW operator classes are first-class SQL contracts:

- `halfvec_hnsw_ops`, `halfvec_hnsw_ip_ops`,
  `halfvec_hnsw_cosine_ops`, and `halfvec_hnsw_l1_ops`
- `sparsevec_hnsw_ops`, `sparsevec_hnsw_ip_ops`,
  `sparsevec_hnsw_cosine_ops`, and `sparsevec_hnsw_l1_ops`
- `bitvec_hnsw_hamming_ops` and `bitvec_hnsw_jaccard_ops`

Each class stores a dense graph payload, traverses with the matching metric,
and returns the exact operator distance type. The variant types and their
non-index SQL helpers remain experimental as a broader compatibility surface.

Experimental sparse collection metadata validates table-backed `sparsevec`
source columns and stores per-vector sparse storage/index/status metadata:

- `pgcontext.register_sparse_vector(collection_name text, vector_name text, vector_column text, dimensions integer, metric text)`
- `pgcontext.collection_sparse_vectors(collection_name text)`
- `pgcontext.configure_sparse_vector(collection_name text, vector_name text, storage_options jsonb, index_options jsonb, status text)`
- `pgcontext.attach_sparse_hnsw_index(collection_name text, vector_name text, index_name text)`
  validates and binds a schema-qualified, metric-matched sparse HNSW index.

- `pgcontext.search_sparse(query sparsevec, point_ids bigint[], vectors sparsevec[], metric text, limit integer)`
  scores explicit sparse candidate arrays with `l2`, `inner_product`,
  `cosine`, or `l1` and returns exact top-k rows with deterministic tie breaks.
- `pgcontext.search_sparse(collection text, vector_name text, query sparsevec, limit integer)`
  serves a validated attached sparse HNSW index with exact source rerank, or
  falls back to exhaustive exact table-backed scoring when no valid binding exists.
- `pgcontext.search_sparse(collection text, vector_name text, query sparsevec, filter text, limit integer)`
  applies registered-field filter JSON through an HNSW candidate mask and the
  final authoritative source recheck.
- `pgcontext.explain_sparse(collection text, vector_name text, query sparsevec, limit integer)`
  reports the actual exact/HNSW strategy and scored/candidate/recheck counters.
- `pgcontext.query(collection text, vector vector, sparse_vector_name text, sparse_query sparsevec, limit integer)`
  fuses dense exact search with exact sparse search through reciprocal rank
  fusion. Sparse ANN branches remain outside the stable compatibility promise.
- `pgcontext.rerank_late_interaction(query_vectors vector[], point_ids bigint[], candidate_vectors vector[], candidate_offsets integer[], limit integer)`
  partitions candidate token vectors by point, scores each point with exact
  MaxSim inner product, enforces a comparison budget, and returns final rerank
  order with deterministic tie breaks.
- `pgcontext.register_late_interaction(collection text, source_table text, token_source text)`
  binds a collection's source-table `vector[]` column, materializes one private
  pgContext token row per array element under invoker ACL/RLS, installs a
  same-transaction source-DML capture trigger, and builds a collection-scoped
  inner-product HNSW index. The ordinary-table source must expose `id` as a
  `NOT NULL`, immediate, single-column unique key. An empty source is registered
  as `building` until a repair can infer dimensions and publish the index.
- `pgcontext.repair_late_interaction(collection text, batch_size integer)`
  atomically replaces the derived token rows with keyset pagination and a
  per-batch materialization byte budget,
  refreshes restored table bindings, reinstalls the capture trigger, and
  rebuilds the HNSW index. A failed statement or savepoint rolls the previous
  token generation and index back intact.
- `pgcontext.search_late_interaction(collection text, query_vectors vector[], vector_column text, limit integer)`
  exact-scores active collection points from a table-backed `vector[]` source
  column with MaxSim and returns final SQL order with ACL/deleted-point checks.
- `pgcontext.explain_late_interaction(collection text, query_vectors vector[], vector_column text)`
  reports the exact table scan, MaxSim comparison count, comparison budget, and
  typed ANN-planner readiness diagnostics for a table-backed late-interaction
  query without materializing candidate vectors.
- `pgcontext.search_late_interaction_ann(collection text, query_vectors vector[], candidates_per_query integer, limit integer)`
  serves approximate candidates from the collection's registered, pgContext-owned
  token relation and collection-scoped inner-product HNSW generation. The
  candidate prefix expands geometrically, within collection and comparison
  budgets, when deleted or RLS-hidden candidates are removed by the invoker-side
  source recheck. Final ordering always uses exact MaxSim over the current source
  row; token vectors and source-table identifiers are not caller parameters.
- `pgcontext.explain_late_interaction_ann(collection text, query_vectors vector[], candidates_per_query integer)`
  validates the bound source column and exact owned HNSW predicate, expression,
  dimension, and opclass before reporting the ANN planner strategy. It does not
  expose raw token vectors or exact global token counts.
- `pgcontext.search_late_interaction_ann(collection text, query_vectors vector[], vector_column text, token_table text, token_source_key_column text, token_vector_column text, candidates_per_query integer, limit integer)`
  is the legacy experimental overload for user-maintained companion tables. It
  uses a companion token table with a `pgcontext_hnsw` index to collect
  approximate candidate source keys from a `NOT NULL` source-key column and a
  dimensioned `vector(n) NOT NULL` token column, deduplicates them, and
  exact-reranks the authoritative source-table `vector[]` values with MaxSim.
  The function validates the declared token dimension in O(1), including for an
  empty token table, and rejects mixed query dimensions or query/token mismatch
  with SQLSTATE `22023`. It
  validates source and token-table ACLs, rejects strict collection
  `max_candidate_budget` violations for the projected token-candidate work,
  rejects planner-projected comparison budget violations before collecting
  token candidates, and enforces the actual hydrated exact-rerank budget while
  scoring source-table vectors.
- `pgcontext.explain_late_interaction_ann(collection text, query_vectors vector[], vector_column text, token_table text, token_source_key_column text, token_vector_column text, candidates_per_query integer)`
  is the legacy companion-table explain overload. It validates the companion
  token table, `NOT NULL` source-key and token-vector
  columns, declared `vector(n)` dimensions, HNSW index, ACLs, strict collection
  candidate budget, and planner budget before reporting the
  `ann_candidate_serving` planner strategy.

Product codebooks are JSON arrays shaped as
`[[[centroid_value, ...], ...], ...]`, one codebook array per subvector. These
functions, quantized candidate rerank, sparse exact array search, and
late-interaction rerank surfaces are experimental; HNSW token candidate serving
for late interaction is also experimental and must not expose approximate scores
as final SQL scores.

## Current Maturity Boundary

Named sparse ANN, SQL `bit` ANN indexing, quantized index scan serving,
external artifact import/export APIs, and full multi-vector serving are not
stable today. Missing
product behavior, longer-duration certification, and broader workload coverage
are tracked in the public roadmap.
