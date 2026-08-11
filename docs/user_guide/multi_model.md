# Multi-Model Retrieval

Multi-model retrieval lets an old and a new embedding profile serve the same
collection while a backfill is incomplete. It is **Stable**. Each
profile keeps its own vector representation, dimensions, metric, query value,
HNSW index, and lifecycle. pgContext never compares native distance scores
between profiles; it combines one-based ranks with weighted reciprocal-rank
fusion (RRF).

The source table remains authoritative. One source row and one pgContext point
ID identify the occurrence across every profile. PostgreSQL MVCC, `SELECT`
privileges, row-level security, registered filters, deleted-point state, and
source-version checks are reapplied independently to every branch.

## Register Versioned Profiles

A profile used by `query_multi_model` must bind two distinct `bigint` columns:

- `source_version_column` is the current content version.
- `embedding_version_column` is the content version used to create that
  profile's vector.

The vector is eligible only when both versions are non-null and equal. A source
edit therefore makes the old embedding stale immediately, before a replacement
vector is written.

```sql
CREATE TABLE documents (
  id bigint PRIMARY KEY,
  tenant_id text NOT NULL,
  source_version bigint NOT NULL,
  legacy_version bigint,
  modern_version bigint,
  legacy_embedding pgcontext.vector(1536),
  modern_embedding pgcontext.vector(3072)
);

CREATE INDEX documents_legacy_hnsw ON documents
USING pgcontext_hnsw
  (legacy_embedding pgcontext.vector_hnsw_cosine_ops);

CREATE INDEX documents_modern_hnsw ON documents
USING pgcontext_hnsw
  (modern_embedding pgcontext.vector_hnsw_cosine_ops);

SELECT pgcontext.register_embedding_profile(
  'documents',
  'legacy_v1',
  'legacy_embedding',
  'public.documents_legacy_hnsw',
  jsonb_build_object(
    'representation', 'dense',
    'dimensions', 1536,
    'normalization', 'unit_l2',
    'metric', 'cosine',
    'provider', 'example',
    'model', 'legacy',
    'revision', '1',
    'input_template', '{text}',
    'output_template', '{vector}',
    'bit_order', NULL,
    'byte_order', NULL,
    'scale', NULL,
    'zero_point', NULL,
    'configuration_hash', '0123456789abcdef',
    'source_version_column', 'source_version',
    'embedding_version_column', 'legacy_version'
  ),
  'active'
);
```

Register the second profile in `shadow` while its embeddings are being
generated, then inspect coverage and promote it:

```sql
SELECT * FROM pgcontext.embedding_profile_coverage('documents');
SELECT pgcontext.set_embedding_profile_lifecycle(
  'documents', 'modern_v2', 'active'
);
```

Coverage is invoker-scoped. Its active, current, and stale counts include only
mapped source rows visible under the caller's current `SELECT` privileges and
row-level security policy; it does not reveal a collection-wide row count to a
tenant-restricted caller.

Only `active` and `draining` profiles serve queries. `shadow` accepts a
backfill without serving it. An active profile can move to `draining`, roll
back to `active`, or retire. `retired` is terminal. A `failed` profile must
return through `shadow` before serving again.

## Query Multiple Profiles

```sql
SELECT pgcontext.query_multi_model(
  'documents',
  jsonb_build_array(
    jsonb_build_object(
      'profile', 'legacy_v1',
      'configuration_hash', '0123456789abcdef',
      'query', '[0.1, ...]',
      'limit', 50,
      'weight', 1.0
    ),
    jsonb_build_object(
      'profile', 'modern_v2',
      'configuration_hash', 'fedcba9876543210',
      'query', '[0.2, ...]',
      'limit', 50,
      'weight', 1.0
    )
  ),
  '{"must":[{"key":"tenant_id","match":"acme"}]}'::jsonb,
  20,
  60,
  102,
  true
);
```

Arguments after `filter` are final `limit`, RRF `k`, the global candidate
allowance, and `require_all_profiles`. The allowance includes one completeness
probe per branch, so two branch limits of 50 require at least 102 admissions.
Profile names must be unique and must select distinct source columns. Every
weight must be finite and positive. The configuration hash, vector syntax,
representation, dimensions, metric, version columns, source relation, and live
non-partial `pgcontext_hnsw` index are validated before branch execution.
For partitioned sources, pgContext resolves only indexes attached beneath the
registered parent index and admits at most 4,096 parent/child index identities
across the complete request. A larger hierarchy fails before EXPLAIN or
candidate work. EXPLAIN inspection also uses an iterative, node- and
depth-bounded walk, so a deeply nested partition plan fails closed.

The canonical 16 MiB extension-memory allowance covers retained request and
profile metadata, candidate identity state, executor allocations, and the
final JSON report. The SQL adapter preflights the simultaneous copies needed
to cross the JSONB, typed-query, and executor boundaries; a request can
therefore hit the memory allowance before the transport-neutral 16 MiB opaque
query-text ceiling. Filter depth, nodes, and the 64 KiB scalar-byte allowance
are checked before the adapter serializes, clones, or resolves catalog state.
Only filter keys referenced by the shared filter are loaded; each registered
JSONB path is capped at 16 segments and 8,192 bytes before Rust materializes
it. Source identities longer than 1,024 bytes likewise fail before Rust
materializes their text values.

With `require_all_profiles = true`, any missing, non-serving, changed, or stale
profile contract fails before candidate work. Setting it to `false` explicitly
allows ready branches to serve; the report then uses `completion: "degraded"`
and names every skipped profile and reason. Permission, malformed-request, and
source-contract failures never degrade.

The JSON report contains:

- `completion` and ordered `missing_profiles`;
- one `branches` entry per declaration, including lifecycle, status, strategy,
  registration revision, index/source OIDs, candidate count, recheck count,
  retained count, and probe exhaustion;
- ordered unique `results` with point/source identity and fused score;
- per-result `contributions` with profile, registration revision, current
  source version, exact-or-HNSW source kind, source authority, one-based rank,
  diagnostic native score, weight, and RRF contribution.

Native scores are diagnostic only. They are never normalized, averaged, or
used to order candidates from different profiles. Final ties use ascending
stable point ID.

Each branch normally reports `hnsw_with_authoritative_recheck`. pgContext
first verifies that PostgreSQL selected the registered HNSW index, charges the
access method's reported traversal work plus one native score comparison for
every returned candidate to the query-wide budget, and then rereads the
bounded candidate identities under current
MVCC, ACL, RLS, deletion, filter, and version state.

PostgreSQL can occasionally complete a verified HNSW plan without reporting
any access-method work, including a cold first query behind RLS. In that case
pgContext does not treat an empty probe as a complete result. It counts the
complete invoker-visible, version-current corpus against the remaining
comparison allowance. If the corpus fits, the branch runs an exact scan and
reports `exact_fallback_with_authoritative_recheck`; otherwise the statement
fails with budget exhaustion. The fallback never returns a silently partial
branch.

## Cutover and Migration Records

Embedding migrations now reference immutable profile names, not mutable model
version rows:

```sql
SELECT * FROM pgcontext.create_embedding_migration(
  'documents', 'legacy_v1', 'modern_v2', 100000
);
SELECT * FROM pgcontext.update_embedding_migration(1, 25000, 'running');
SELECT * FROM pgcontext.embedding_migrations();
```

The migration catalog tracks bounded progress; it does not run provider model
inference. The removed `_model_versions`, `register_model_version`, and
`model_versions` surfaces have no compatibility shim.

## Operational Policy

Weights are workload policy, not a universal model calibration. The frozen
certification workload declares equal `1:1` weights before execution, uses
eight held-out queries over independent 4D and 8D spaces, and reports A-only,
B-only, fused, filtered-partial, and degraded curves. Each fused query uses two
50-candidate branches, `k = 60`, and a 102-candidate allowance; each single
baseline gets the same allowance. Dataset and workload hashes, raw samples,
latency/cost summaries, PostgreSQL version, hardware, and relevant GUCs are
part of the report contract.

The frozen equal-weight workload passes on PostgreSQL 17 and 18 at one million
rows: fused recall is no worse than the stronger single-profile baseline under
the same declared global budget. The workload, weights, dataset generator, and
decision rule were fixed before those certification runs; no post-result
tuning was used to obtain the Stable result. An application may intentionally
choose a different calibrated tradeoff, but it must freeze and validate that
policy on its own held-out workload.

Cancellation and PostgreSQL statement timeouts cover preparation, candidate
work, authoritative recheck, and final JSON construction. They abort the
statement and return no partial JSON report. Without a collection override,
the elapsed allowance is 500 ms. A collection owner may set
`query_timeout_ms` to widen this mixed-model path up to the global 60-second
ceiling; other canonical query surfaces continue to treat that setting only as
a tighter cap. Reported elapsed and memory usage
include preparation and report finalization. Default telemetry does not record query vectors,
filters, source keys, tenant values, or source text. Run the PG17 and PG18
`tests/heavy/multi_model_coverage.sh` and `multi_model_rls_acl.sh` gates before
a production cutover.

See [Collections](collections.md), [SQL API contract](api_reference.md),
[Security](security.md), and [Operations](operations.md).
