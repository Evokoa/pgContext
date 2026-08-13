# Exact-first readiness

Exact-first readiness keeps the PostgreSQL table authoritative while a derived
vector index is planned, built, cancelled, retried, or replaced. Registration
commits only after pgContext can execute a complete exact dense-vector scan over
the caller's current visible rows. Index work changes cost, never membership or
final exact ordering.

This surface is Experimental. The current implementation requires at least one
registered dense `vector(n)` binding; additional filter and payload bindings
may be normalized for future composition, but the public exact-first search and
advisor consume dense bindings. Lexical, fuzzy, sparse, binary, and composite
apply adapters remain deferred. The advisor currently freezes exact-only, HNSW,
or IVFFlat plans.

## Lifecycle

Readiness has five stable labels:

- `exact_only`: exact search is complete and no current optimization exists.
- `building`: exact search remains available while a fenced job is active.
- `indexed`: a live, valid, structurally verified current index is published.
- `stale`: relation, key, column, type, typmod, or collation identity drifted.
- `degraded`: exact search remains available after optimization failure or
  cancellation.

`recommend_only` and `exact_only` never create an index. `enqueue` creates a
fenced operational job. A controller must claim it, run the returned reviewed
`CREATE INDEX CONCURRENTLY` as a top-level PostgreSQL statement, and then call
`publish_exact_first_build`. `apply_foreground` uses non-concurrent `CREATE
INDEX` in the calling transaction and therefore suits only explicitly reviewed
maintenance windows.

```sql
SELECT * FROM pgcontext.register_exact_first(
  'products', 'public.products',
  jsonb_build_object(
    'version', 'exact_first_registration_v1',
    'key_column', 'id',
    'bindings', jsonb_build_array(jsonb_build_object(
      'name', 'embedding', 'column', 'embedding', 'kind', 'dense',
      'dimensions', 768, 'metric', 'cosine'
    ))
  )
);

SELECT * FROM pgcontext.exact_first_search(
  'products', 'embedding', $1::pgcontext.vector, 20
);
```

`exact_first_search` returns the native `real` distance and uses a
deterministic source-key tie break. It remains the complete exact oracle in all
readiness states. A published ANN index is available to compatible ordinary
PostgreSQL ordered scans; the Experimental API does not silently substitute an
approximate result for this exact oracle.

The controller protocol is `exact_first_advisor` →
`apply_exact_first_plan(..., 'enqueue')` → `claim_exact_first_build` → top-level
DDL → `publish_exact_first_build`. Heartbeat before the 60-second lease expires.
Cancellation is cooperative; an expired `cancel_requested` lease is finalized
on the next claim. Failed or cancelled plans may be requeued at most three
attempts with `retry_exact_first_build`.

## Bounds and durability

The P14 manifest freezes 256 inspected columns and indexes, 1 MiB registration
JSON, 256 KiB objectives JSON, 16,384 JSON iterator nodes, depth 64, 128-byte
names, 64 KiB generated DDL, 16 immutable plan revisions, 16 retained
optimization targets, three attempts, and a 60-second lease. JSON raw bytes,
nodes, and depth are admitted before serde allocation. The required 10M
IVFFlat verification queries use the existing hard maximum of 10,000,000
posting visits; ordinary sessions retain their configured/default budget.

Logical registrations and immutable plans participate in extension
configuration dump. Operational jobs, leases, invalid samples, and optimization
targets do not; after logical restore the exact path remains authoritative and
optimization is rebuilt or revalidated. HNSW and IVFFlat indexes remain normal
PostgreSQL derived indexes and follow PostgreSQL physical backup/WAL behavior.

All source scans execute as the invoker, preserving MVCC, ACL, and RLS. Catalog
functions authorize `SESSION_USER` through collection ownership and use a pinned
search path. Source-key and registered-column OID, attnum, type, typmod, and
collation drift fail closed instead of rebinding.

## Certification status

The retained gate runs exact queries during a genuine top-level concurrent
build while `COPY`, insert, update, key update, and delete execute, then compares the
published index with the exact oracle and exercises replay, cancellation,
restart, lease expiry, retry, and non-superuser operation. Both frozen
ten-million-row lanes preserve bit-exact membership/scores, 100% top-10 recall,
and exceed the 5,000-row/s build floor (PG17: 34,682; PG18: 33,253), but miss the
250 ms building-query p95, 100 ms indexed-query p95, and 2 GiB temp ceilings
(PG17: 7.850 s / 490 ms / 12.81 GB; PG18: 6.894 s / 468 ms / 13.74 GB).
Stable promotion is therefore a measured no-go and the capability remains
Experimental.

The RLS lane compares the complete returned key sequence with the exact
policy-visible oracle before reporting success. Publication establishes
structural index validity only. The target's
`recall_bps` remains null; the retained certification workload measures recall
against the exact oracle as a separate Stable-promotion gate.
