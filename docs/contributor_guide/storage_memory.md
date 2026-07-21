# Storage and Memory

PostgreSQL tables are authoritative. pgContext catalogs describe collections
and serving state; HNSW index pages and segment files are rebuildable
acceleration artifacts. They must never become the only copy of application
vectors or metadata.

The access method validates page headers, record lengths, metric bindings, and
tuple references before traversal. Candidate rows are reloaded through
PostgreSQL and rechecked for visibility, predicates, ACL/RLS, and exact score.
Storage loaders fail closed for path escape, checksum drift, incompatible
format versions, truncated records, and memory-budget violations.

Rust heap allocations are backend-local. Read-only mappings validate bounds and
format before exposing bytes, and mapped experimental artifacts do not replace
PostgreSQL shared buffers. Review the [storage contract](hnsw_storage_contract.md),
[unsafe review](unsafe_review.md), and [operations guide](../user_guide/operations.md).
