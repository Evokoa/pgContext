# Adaptive-Dimension (Matryoshka) Retrieval

A Matryoshka model promises that the leading coordinates of an embedding are
themselves a usable embedding at a lower dimension. pgContext uses that promise
for one thing only: making **candidate generation** cheaper.

The stored vector is never truncated or rewritten, and final ranking always
rechecks the full authoritative dimensions under MVCC and RLS. Before reading
a prefix, pgContext proves that the request's cardinality and Rust-owned-memory
budgets can admit every visible candidate and rerank it exactly. If they
cannot, it selects full-vector exact search before doing prefix work. The
returned ordered answer is therefore the same as the full-vector exact oracle.

> **Maturity: Experimental.** The mechanism is correctness-preserving and
> bounded, but the current scan-based implementation is a **performance
> no-go**: its small-corpus PG17/PG18 gate performs more work than full-vector
> exact search, and larger corpora safely select exact fallback under the
> 10,000-candidate execution ceiling. Do not enable it for latency improvement.

## Certifying prefixes on a profile

Prefixes are declared on an embedding profile, not per query:

```sql
SELECT pgcontext.register_embedding_profile(
    'docs', 'mrl', 'embedding', 'public.docs_hnsw',
    jsonb_build_object(
        'representation', 'dense', 'dimensions', 768,
        'normalization', 'unit_l2', 'metric', 'cosine',
        'provider', 'acme', 'model', 'mrl-768', 'revision', '1',
        'input_template', '{text}', 'output_template', '{vector}',
        'bit_order', NULL, 'byte_order', NULL, 'scale', NULL, 'zero_point', NULL,
        'configuration_hash', '0123456789abcdef',
        'matryoshka_prefixes', jsonb_build_array(128, 256, 512)
    )
);
```

`matryoshka_prefixes` accepts 1..=8 strictly ascending positive dimensions, each
strictly below the profile's `dimensions`.

### What is eligible, and why

A policy is accepted only for a `dense` or `half` representation under the
`l2`, `inner_product`, or `cosine` metric. Those are the representations and
metrics whose coordinates stay independently interpretable at an arbitrary cut
point.

Everything else is rejected at registration:

| Rejected | Reason |
| --- | --- |
| `bit` | Packed bits have no coordinate boundary at an arbitrary prefix |
| `sparse` | Coordinates are indexed, not positional |
| `int8` / `uint8` with a scale | An affine interpretation is not preserved by truncation |
| `hamming` / `jaccard` | Set metrics are not defined coordinate-wise |
| `l1` | Not certified here; add it only with evidence |

Descending or duplicated prefixes, a zero prefix, and a prefix that reaches the
full dimension are also rejected.

## Choosing a prefix at query time

```sql
SET pgcontext.adaptive_prefix_dimensions = 0;    -- automatic (default)
SET pgcontext.adaptive_prefix_dimensions = 128;  -- pin a declared prefix
SET pgcontext.adaptive_prefix_dimensions = -1;   -- read the full dimensions
```

| Setting | Behavior |
| --- | --- |
| `0` | Selects the narrowest certified prefix when the complete visible corpus can be covered within the cardinality and memory budgets |
| positive | Pins that prefix. An **undeclared** value is rejected and the query falls back to the full dimensions rather than reading an uncertified cut point |
| `-1` | Disables prefix candidate generation entirely |

Every one of these falls back to full-dimension candidate generation when the
collection has no certified policy, when the profile is ineligible or absent,
or when candidate, comparison, recheck, transient-memory, or expansion budgets
cannot fund exhaustive prefix coverage. A collection that never declares a
policy behaves exactly as it did before this feature existed.

## How it works

1. The candidate probe scores `pgcontext.vector_prefix(column, N)` against a
   query prefix. The stored value is untouched.
   When the profile promises `unit_l2`, the **query** prefix is renormalized
   under `cosine` and `inner_product`, where rescaling one side cannot reorder
   results. Under `l2` it is **not**: `|q - s|^2` reweights the dot-product term
   against a per-row `|s|^2`, so scaling only the query would produce an order
   that is neither "truncate both" nor "renormalize both". `l2` prefixes are
   truncated plainly on both sides.
2. A bounded preflight counts the invoker-visible corpus only up to the point
   needed to prove whether exhaustive prefix coverage fits. If it does not fit,
   pgContext runs full-vector exact search immediately.
3. The first prefix probe admits `4 ×` the requested limit. When that page is
   not already exhaustive, one wider step admits the complete visible corpus
   and uses the next declared prefix when one exists. Both pages, both scans,
   the expansion, the exact rechecks, and Rust-owned candidate memory are
   charged before the first prefix query runs.
4. The executor's source recheck rereads every admitted current row under MVCC
   and RLS, recomputes the exact full-dimension distance, then ranks and
   truncates.

Nothing downstream trusts the prefix score, and a non-exhaustive prefix page is
never reported as complete. That is what makes the ordered answer identical to
full-vector search.

`pgcontext.vector_prefix(vector, dimensions)` is immutable and parallel-safe.
Note that pgContext's own probe passes the width as a **bind parameter**, and
PostgreSQL matches expression indexes structurally, so an index built over
`vector_prefix(col, 128)` will not be matched by the probe. Treat the function
as a projection you can use in your own SQL, not as an index-acceleration hook.

## Scope: exact candidate generation only

Adaptive dimensions apply to the **exact** dense candidate path. A query served
through an attached HNSW index (`pgcontext.hnsw_search`, or any path that
selects the HNSW adapter) ignores certified prefixes entirely: the index is
built over the full vector, so a prefix cut point has no meaning inside its
graph. Subvector or prefix-quantized index support is a separate, unshipped
piece of work.

If your collection is served by HNSW today, this feature will do nothing for it.

## Observing it

An exhaustive prefix schedule appears in query telemetry as
`dense_adaptive_prefix_exhaustive`. A budget rejection appears as a
`dense_exact_adaptive_*_budget` strategy, and a disabled or uncertified path
uses the ordinary exact strategy. `adaptive_prefix_dimensions` records the
last prefix used, `adaptive_termination` records why the schedule completed or
fell back, and `total_expansions` records widening work:

```sql
SELECT strategy, query_count, total_candidates, total_rechecks,
       total_expansions, adaptive_prefix_dimensions,
       adaptive_termination, avg_latency_ms
  FROM pgcontext.query_execution_stats()
 WHERE collection_name = 'docs';
```

## Limits

| Bound | Value |
| --- | --- |
| Declared prefixes per profile | 8 |
| Prefix dimension | `1..=16000`, strictly below the full dimension |
| Initial candidate width | 4× the requested limit |
| Widening steps | At most 1 after the initial probe |
| Single-leaf candidate ceiling | 10,000 total candidate admissions across all prefix steps |

The executor also applies the request's comparison, recheck, stage, expansion,
elapsed-time, and transient-memory limits. Composite query branches retain
their smaller per-leaf allocation and commonly select exact fallback.

Elapsed time is enforced by PostgreSQL's current-statement timeout across the
preflight, prefix scans, and exact recheck; it is not estimated during the
cardinality preflight. A timeout can therefore cancel an in-flight schedule,
but cancellation is fail-closed and never returns a partial prefix answer.

The frozen Phase 10 manifest records 1M and 10M rows as `no_go` with
`full_vector_exact_fallback`: both exceed the 10,000-row authoritative recheck
ceiling, so the pure preflight returns `recheck_budget` with zero prefix steps.
The pure manifest pins the 1,000,000-comparison allowance and the
zero-prefix `recheck_budget` decision. On the development harness, the frozen
500 ms global elapsed limit is tighter than a 1M-row exact scan, so the live PG17
and PG18 gate requires fail-closed cancellation with zero prefix expansion. The
preserved 10M command requires the same fail-closed behavior and records whether
elapsed or comparison budget terminates first. Exact fallback is not a promise
that an over-budget or timed-out exact scan will return results.

## Related

- [SQL API contract](api_reference.md) — `register_embedding_profile` and
  `vector_prefix` signatures.
- [Dense vectors and exact search](vector_search.md) — the full-vector path this
  optimizes and is measured against.
