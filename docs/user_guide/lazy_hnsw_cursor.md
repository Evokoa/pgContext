# Internal lazy HNSW cursor

Phase 15 puts pgContext's graph-read HNSW traversal behind one statement-local,
resumable cursor. This is an internal foundation for later bounded beam search,
not a SQL API or a user-selectable planner mode.

## Contract

The cursor owns only traversal state: the pending and nearest heaps, visited
nodes, adjacency scratch space, cumulative work, and a typed terminal reason.
It supports seed, peek, pop, bounded advance, finish, and exhausted operations.
Peek performs no graph work. Frontier order is ascending distance and then
internal node ID; final result order remains ascending distance and then point
ID. Nodes are visited once, and masked nodes may connect traversal but cannot
enter the result set.

Existing eager graph-read entry points now drain this same cursor, so page,
mapped, quantized, segmented, delta-overlay, and filtered HNSW serving keep the
same SQL behavior. PostgreSQL still resolves approximate heap identities under
the current snapshot and exactly rechecks the authoritative source operator
before final ordering. The cursor does not weaken MVCC, ACL, RLS, deletion, or
source-row checks.

Cursor state borrows its graph adapter, query, comparison budget, and
cancellation hook. It is deliberately not cloneable, serializable, or valid
outside the statement and index snapshot that created it.

## Bounds and termination

One advance requests 1–256 frontier expansions; the internal default is 32.
The cursor admits at most 10,000,000 node expansions. This node-work counter is
separate from the Phase 8 query executor's provider/topology-call expansion
budget. Query-wide comparison and memory ceilings retain their Phase 8 bounds,
and the cursor additionally freezes a 10,000,000 adjacency-entry ceiling for
certification. Allocation projections cover the
visited set, both heaps, adjacency scratch, final results, and returned frontier
batches before capacity growth. The standalone release gate also enforces a
256 MiB process-RSS ceiling.

Terminal diagnostics contain no vectors, filters, source keys, or row data.
They are `exhausted`, `cancelled`, `comparison_budget`, `expansion_budget`,
`edge_budget`, `memory_budget`, or `adapter_error`. Only `exhausted` proves a
complete provider result; every other reason remains visibly incomplete.

## Certification and maturity

The frozen manifest is `hnsw_lazy_cursor_v1`. Its deterministic 10,000-row,
32-dimensional fixture runs 128 paired eager/cursor queries and checks exact
ordered results plus identical comparison, expansion, and edge work. Batch
sizes from 1 through 256, all supported metrics, ties, masks, sparse ACORN
connectors, cancellation, and exact budget boundaries have focused coverage.
The owned source registry covers page, mapped full-precision, mapped
quantized, segmented, and delta-overlay graph reads.
The release-mode gate requires cursor p50 latency no worse than 1.10 times the
eager compatibility drain and retained bytes no worse than 1.05 times the
pre-cursor projection.

The capability remains **Internal**. It changes no public SQL object, GUC,
planner default, index format, or compatibility promise. Later graph phases may
consume it only after preserving these bounds and PostgreSQL authority checks.
