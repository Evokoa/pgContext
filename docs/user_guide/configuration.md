# Configuration

pgContext V1 has no required external service. Configuration is split between
PostgreSQL, collection registration, ordinary indexes, and experimental HNSW
reloptions.

1. Configure PostgreSQL memory, WAL, authentication, ACL/RLS, backup, and
   connection settings normally.
2. Register an application-owned table with `pgcontext.create_collection`.
3. Register vector and filter fields explicitly; unregistered payload fields
   are not accepted by filter JSON.
4. Add ordinary B-tree or GIN indexes where PostgreSQL filter planning benefits.
5. Add `pgcontext_hnsw` only after exact results provide a correctness oracle.

HNSW construction/search tuning is documented in [Indexes](indexes.md). Strict
collection limits and SQL-visible statuses are documented in the
[API reference](api_reference.md). Do not treat experimental metadata fields as
active enforcement unless the referenced API explicitly says they are consumed.
