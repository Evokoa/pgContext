# Rebuildable Storage Artifacts

pgContext treats segment artifacts as rebuildable cache data. Losing a segment
file must not lose primary PostgreSQL table data. This is an intentional
operational difference: backups must preserve source tables and pgContext
catalog metadata, while acceleration files may be validated and copied for
speed or rebuilt after restore.

The current `context-storage` segment format has a fixed 40-byte header followed
by payload bytes. Header fields are little-endian and include:

- magic bytes identifying pgContext segment files;
- a format version, currently `1`;
- an endian marker;
- a segment kind;
- reserved bytes, currently zero;
- payload byte length;
- a deterministic checksum.

Format version `1` is the current segment format. pgContext writes only
version `1` artifacts, and the readable-version window is fixed at minimum `1`,
maximum `1`. The loader rejects unknown versions instead of attempting
best-effort reads. Future
incompatible formats must use a new version number, keep old loader behavior
pinned by tests, widen the readable-version window only when backwards loading is
supported, and document whether operators should rebuild artifacts from source
tables or run an explicit import/export migration.

The checksum covers the header with the checksum field zeroed plus the payload.
It is used to detect accidental artifact corruption, not to authenticate data.

Experimental SQL helpers expose the same byte-level contract for tests,
diagnostics, and artifact-publication plumbing:

- `pgcontext.encode_artifact_segment(kind text, payload bytea)` writes a
  versioned segment header and checksum around a rebuildable payload. The
  currently exposed kind is `hnsw_graph`.
- `pgcontext.validate_artifact_segment(segment bytea)` validates segment bytes
  through the mmap-safe loader and reports kind, payload length, and checksum.
  Malformed segment bytes raise SQLSTATE `XX001`; unsupported segment kinds
  raise SQLSTATE `22023`.
- `pgcontext.publish_artifact_segment(build_job_id bigint, segment bytea)`
  validates bytes for a completed visible `segment` or `mmap` build job and
  records collection-owned manifest metadata: build job, artifact target,
  segment kind, format version, payload length, checksum, and lifecycle state.
- `pgcontext.publish_artifact_segment_file(build_job_id bigint, segment bytea)`
  performs the same validation, writes the bytes through the context-storage
  atomic writer under a generated PostgreSQL data-directory-relative
  `pgcontext_artifacts/...` path, reloads the file through the validator, and
  records the generated relative path with `file_materialized` lifecycle state.
- `pgcontext.artifact_segments(collection text)` lists those visible manifests,
  including generated relative paths for file-materialized artifacts.
- `pgcontext.artifact_segment_memory(collection text)` reports deterministic
  mmap budget diagnostics for each visible manifest: payload bytes, fixed header
  bytes, total mapped bytes, lifecycle state, and whether a file has been
  materialized.
- `pgcontext.artifact_segment_diagnostics(collection text)` reloads
  file-materialized manifests through the segment loader, rejects catalog paths
  that escape `pgcontext_artifacts/...`, and reports constrained text statuses:
  `ready`, `metadata_only`, `artifact_missing`, `checksum_mismatch`,
  `artifact_corrupt`, `metadata_mismatch`, or `path_rejected`. It also reports
  deterministic `repair_advice` and `cleanup_eligible` columns. Ready artifacts
  need no action; metadata-only, retired, or pathless manifests are not cleanup
  candidates; rejected paths must be fixed or removed from the catalog before
  cleanup; and missing, corrupt, checksum-drifted, or metadata-drifted artifacts
  should be retired or rebuilt after investigation. `cleanup_eligible` is true
  only for root-confined materialized artifact paths that
  `retire_artifact_segment` may clean up.
- `pgcontext.retire_artifact_segment(artifact_id bigint)` marks the manifest
  `retired`, then attempts to remove the generated materialized artifact file
  when it still exists. It tolerates already-missing files, rejects catalog paths
  that escape `pgcontext_artifacts/...`, and does not rebuild or replace
  artifacts.
- `pgcontext.cleanup_artifact_segments(collection text, dry_run boolean)` scans
  visible file-materialized manifests for loader-visible cleanup candidates:
  missing files, checksum drift, corrupt files, or catalog/file metadata drift.
  It also scans the generated per-collection artifact directory for regular
  `.pgctxseg` files that are not referenced by any visible manifest, which can
  happen if a backend stops after the atomic file write but before catalog
  publication. With `dry_run = true`, it reports the manifests it would retire
  and orphan files it would remove without mutating catalog rows or files. With
  `dry_run = false`, it retires those manifests and removes generated files
  when they still exist, and removes orphan `.pgctxseg` files. Ready artifacts,
  metadata-only artifacts, symlinks, directories, and non-segment files are
  skipped; escaped catalog paths raise SQLSTATE `22023` instead of being
  cleaned up.

The metadata-only publisher does not store payload bytes or write files. The
file-materialization publisher never accepts an operator-supplied path and does
not derive storage paths from artifact names or targets; it uses generated
identifier-based paths under the PostgreSQL data directory. The memory
diagnostic is catalog-derived and does not open or trust artifact files; the
file diagnostic opens only generated, root-confined artifact paths before
classifying corruption or catalog drift. The retire and cleanup helpers attempt
cleanup only for generated, root-confined artifact paths. These helpers do not
make mmap/vector serving stable. `pgcontext.artifact_segment_serving_readiness`
is the experimental read-only gate every mmap serving path must pass: it
requires `mmap` artifacts, file-materialized lifecycle, confined paths, reload
and checksum/catalog agreement, and mapped bytes within the caller supplied
budget before reporting `serving_ready = true`.
`pgcontext.artifact_segment_mmap_payload` uses that same gate for one visible
artifact and copies validated payload bytes at the SQL compatibility boundary.
Internally, `pgcontext.search_mmap_hnsw_artifact` holds a reader pin and a real
read-only OS mapping for the query, validates borrowed bytes, traverses the
persisted HNSW links, merges source points added after the generation high-water
mark, and rechecks/scores candidates against the authoritative source table. It
remains an experimental serving surface and fails closed for not-ready or
corrupt artifacts.

The loader rejects malformed artifacts before exposing payload bytes. The same
validation path also supports memory-mapped readers by returning a borrowed
payload view after checking header fields, payload bounds, section alignment,
and checksum integrity without copying payload data. Covered bad paths include
truncated headers, unknown versions, wrong endian markers, oversized payload
lengths, truncated payloads, misaligned mmap sections, overflowing section
ranges, and checksum mismatches. Atomic write/reload, import/export, and loader
fuzzing are covered by tests.

HNSW graph artifacts use a portable payload format inside the outer segment:
payload magic/version, record count, dimensions, and little-endian records with
contiguous node ids, point ids, dense vectors, and base-layer neighbor ids.
`pgcontext.validate_hnsw_graph_artifact(segment)` validates that inner payload
before any mmap serving path may consume it, rejecting truncated records,
non-contiguous node ids, out-of-range neighbors, and invalid vectors as corrupt
artifact input. Source-built artifacts contain real HNSW base links; old or
manually constructed edgeless artifacts retain an exact compatibility fallback.
Final SQL scores always come from current source-table vectors, not artifact
scores.

Segment writers use a same-directory temporary file, sync the temporary file,
rename it over the target path, sync the parent directory, and then reload the
target through the validator. Readers also reject encoded files larger than the
maximum header-plus-payload size before loading bytes into memory.

Import and export operations in `context-storage` are rebuildable-artifact copy
primitives for internal and release-gate use. They are not exposed as stable SQL
operator APIs in this release; the SQL validators and manifest APIs above are
experimental metadata primitives. Until artifact file publication and serving
APIs are explicitly stabilized, operators should use normal PostgreSQL
backup/restore for authoritative data and rebuild acceleration artifacts from
catalog metadata and source tables.

## Snapshot, Export, And Import Procedure

Use PostgreSQL backup/restore as the authoritative recovery path:

1. Take a normal PostgreSQL base backup, logical dump, or managed-service
   snapshot that includes user source tables and the `pgcontext` catalog schema.
2. Treat pgContext index and segment artifacts as optional cache files. If an
   artifact copy is useful, copy it only after the writer has
   completed its atomic rename and the validator accepts the target file.
3. Restore PostgreSQL data first. Confirm that collections, points, vectors,
   filters, aliases, model versions, and migration records are present in the
   catalog before rebuilding derived artifacts.
4. Rebuild or revalidate pgContext acceleration artifacts from source tables.
   Do not trust a copied artifact unless its header version, bounds, alignment,
   and checksum pass the loader.
5. If a copied artifact fails validation, delete the artifact copy and rebuild
   it from PostgreSQL source tables. Do not repair binary artifacts by hand.

The operator contract is therefore to snapshot PostgreSQL, optionally copy
validated rebuildable artifacts for speed, and prefer rebuild over binary import
whenever there is a version, checksum, or ownership mismatch.

The segment loader is covered by a `cargo-fuzz` target named `segment_loader`.
New binary-loader fixes should add a focused unit test and, when practical, a
small corpus seed under `fuzz/corpus/segment_loader/`.
