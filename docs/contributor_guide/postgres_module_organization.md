# PostgreSQL Integration Module Organization

`context-pg` owns every PostgreSQL-specific concern: SQL objects, SPI access,
catalog hydration, ACL/RLS checks, pgrx conversion, SQLSTATE translation, index
access-method callbacks, and PostgreSQL-backed tests. This makes it the largest
workspace crate, but its dependency direction remains correct: reusable types
and algorithms stay in the pure crates and `context-pg` adapts them.

`context-pg` is decomposed into the private boundaries below:

| Owner | Responsibilities | Existing boundary | Decision |
|---|---|---|---|
| `hnsw_hierarchy` | hierarchical graph construction and search | graph mutation/search implementation is isolated in `graph_impl` | Keep the pure hierarchy owner while separating its implementation-heavy graph operations. |
| `artifact_segments` | publish, validate, list, retire, and reclaim rebuildable artifacts | diagnostics, serving readiness, SPI persistence, result conversion, and file cleanup are separate submodules or private include fragments | Keep one transactional owner; its operations share catalog locks and lifecycle invariants. |
| `table_search` | registered-table exact search, scroll, count, facets, and source visibility | catalog access, candidate recheck, grouped search, named search, recommendations, and shared support are submodules | Keep the facade; isolated search strategies remain behind explicit private boundaries. |
| `retrieval` | compose `context-query` ports over PostgreSQL | SPI exact/HNSW/filter candidates, authoritative source recheck, telemetry, and cancellation adapters share one private module | Keep PostgreSQL transport and security preambles outside the pure executor; new candidate kinds implement the existing ports here. |
| `build_jobs` | build-job lifecycle and authoritative source scanning | backend identity, job types, validation, and SPI persistence are separate private fragments | Keep one lifecycle owner so status transitions and catalog writes cannot drift. |
| `vector_catalog` | dense and sparse vector registration/configuration | SPI persistence and catalog hydration are isolated from the SQL-facing facade | Keep one catalog owner until dense and sparse catalogs gain independently changing contracts. |
| `operations` | index status, diagnostics, memory estimates, optimization, vacuum advice, and recall checks | advisor logic and checked catalog/value helpers are private fragments | Keep one operator-facing diagnostics facade while centralizing checked conversions. |
| `hnsw_am` | access-method registration, callback routing, scan state, persisted pages, and validation | SQL declarations, callback, FFI, bitmap, options, codec, storage, vacuum, WAL, and MVCC concerns are isolated | Keep one PostgreSQL AM owner; SQL dependencies and unsafe callback invariants are independently reviewable. |
| `vector_variants` | PostgreSQL types, casts, distance functions, and aggregates for half, sparse, and bit vectors | aggregate finalization helpers are isolated; exact kernels remain in `context-core` | Keep the SQL type facade; do not duplicate exact metric semantics here. |

No additional crate is justified by this review. A future split requires at
least one concrete trigger: a different dependency surface, an independently
published contract, a distinct team owner, or a reusable pure component that
can be tested without PostgreSQL. File length alone is not a crate boundary.

The mechanical guard is `scripts/check-source-hygiene.sh`: Rust files over the
review threshold must have a pinned maximum and cannot silently grow. The
architectural guard is `scripts/check-crate-boundaries.sh`, which prevents
PostgreSQL and transport concerns from leaking into pure crates.
