# Security Model

pgContext SQL APIs are designed to work with ordinary PostgreSQL security
controls. Catalog-changing functions run with pgContext catalog privileges, set
a safe function `search_path`, and still check the SQL session user for
collection ownership and source-table privileges before exposing or mutating
user data.

Source tables remain application-owned PostgreSQL tables. Table ACLs and row
level security policies continue to apply to search, count, facet, point, and
registration paths that read or expose source data.

Filters are parsed into typed structures before SQL rendering. Registered
columns and JSONB paths are resolved through catalog metadata; predicate values
are bound as SPI parameters instead of being interpolated into SQL text.

Security-definer paths are tested with attacker-controlled schemas earlier in
the caller `search_path` to ensure pgContext resolves its own catalog objects
and PostgreSQL built-ins deliberately.

Telemetry surfaces are local SQL-visible counters and typed statuses. Automatic
execution telemetry stores the collection association plus bounded
strategy/completion labels and numeric work; it never copies vectors, payloads,
source keys, role names, filter values, literal query text, or caller-provided
tenant dimensions. Manual cohort labels are caller-controlled bounded ASCII
operator labels and may intentionally encode an application's tenant/cohort
dimension, so applications must apply their own PII policy to those labels.
Visibility is derived from `SESSION_USER` membership through public visibility
views. Every membership-filtered visibility view is a PostgreSQL security
barrier, so caller-supplied non-leakproof predicates cannot inspect rows before
the membership condition is applied. Callers receive no direct access to the
private telemetry table.
Query backends enqueue fixed-size numeric/label events without supplying a role
OID or database identity. The background worker derives its database and
extension-owner identity from server state and writes the private table directly
in its own transaction; there is no public security-definer telemetry writer.
Queue-health counters are database-local and require membership in `pg_monitor`.

The public `_collection_acl` view is intentionally different: it is an
unfiltered pre-authorization lookup used to resolve a collection or alias before
membership can be checked. Every collection and alias entry in this minimal
directory is publicly enumerable, including its collection ID, owner role, and
whether a source table is bound. Source-table identity, source keys, vectors,
payload metadata, and operational state remain private or membership-filtered.

## Security-Definer Review Notes

Security-definer functions are limited to extension catalog maintenance and
diagnostic aggregation. They set a fixed `search_path` of `pg_catalog`,
`pgcontext`, and `public`; resolve user relations through registered catalog
metadata or validated qualified names; and still check the SQL session user
before exposing source rows or mutating collection-owned metadata.

Reviewed groups:

- Collection catalog: `create_collection`, `collection_info`,
  `register_vector`, and `drop_collection`.
- Payload and point catalog: `register_filter_column`, `register_jsonb_path`,
  `upsert_points`, and `delete_points`.
- Source-row readers: `search`, filtered `search`, candidate recheck,
  `count`, `facet`, `scroll`, and `query`.
- Operations and telemetry: `index_status`, `estimate_index_memory`,
  `optimization_status`, `vacuum_advice`, `record_query_stat`,
  `query_cohort_stats`, `query_execution_stats`, and the `pg_monitor`-restricted
  `query_telemetry_queue_stats`.
- Model metadata: `register_model_version`, `model_versions`,
  `create_embedding_migration`, `update_embedding_migration`, and
  `embedding_migrations`.
- Artifact operations: `publish_artifact_segment`,
  `publish_artifact_segment_file`, `artifact_segments`,
  `artifact_segment_memory`, `artifact_segment_diagnostics`,
  `artifact_segment_serving_readiness`, `artifact_segment_mmap_payload`,
  `cleanup_artifact_segments`, and `retire_artifact_segment`.

Release tests cover hostile `search_path` shadow objects, default privileges,
ACL/RLS source-row boundaries, and telemetry privacy. New security-definer
functions must add the same kind of catalog classification, SQLSTATE, and
hostile-input coverage before becoming part of the stable surface.

## PostgreSQL-Native Operational Boundary

Using PostgreSQL ACLs, RLS, transactions, WAL, backup, and restore is an
intentional product difference from systems that copy payloads into a separate
service. The operational consequence is that pgContext does not provide a
second authorization policy or an independent consistency and backup domain:
source-table privileges and transaction visibility remain authoritative.

When migrating, preserve the source tables, roles, grants, RLS policies, and
pgContext catalog schema in the same PostgreSQL backup/restore workflow. Verify
the application role against representative search and filter queries after
restore; do not translate PostgreSQL policy into a pgContext-specific ACL.
