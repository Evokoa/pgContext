# Client-Facing Examples

These examples are intended for application teams evaluating pgContext without
using hidden catalog tables or internal APIs. The runnable SQL files live under
`examples/sql/`.

## Example Map

- `01_pgvector_migration.sql`: register an existing pgvector-style table, run
  exact search, use distance operators, and check recall.
- `02_qdrant_filters.sql`: register ordinary and JSONB payload fields, run
  Qdrant-style filters, count, facet, grouped search, payload updates, and bulk
  point maintenance.
- `03_hybrid_recommend_tenant_ops.sql`: combine hybrid dense plus full-text
  retrieval with recommendation search, discovery/explore search, tenant
  filters, and operational telemetry.
- `04_model_quantization.sql`: record model versions and migrations, use
  collection aliases for cutover, and call SQL quantization helpers.
- `05_named_dense_sparse_vectors.sql`: show named dense vector registration,
  exact search over a registered sparse source column with
  `pgcontext.search_sparse`, and exact dense+sparse RRF query fusion with
  `pgcontext.query`.

Each script starts with `CREATE EXTENSION IF NOT EXISTS pgcontext;` and can be
adapted into migrations or application smoke tests.
