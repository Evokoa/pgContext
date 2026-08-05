# Adaptive-Dimension (Matryoshka) Retrieval

A Matryoshka model promises that the leading coordinates of an embedding are
themselves a usable embedding at a lower dimension. pgContext uses that promise
for one thing only: making **candidate generation** cheaper.

The stored vector is never truncated or rewritten, and final ranking always
rechecks the full authoritative dimensions under MVCC and RLS. So at a
sufficient candidate budget a prefix path returns the *identical ordered
answer* that full-vector exact search returns — the full-vector scan is its own
oracle.

> **Maturity: Experimental.** The mechanism is correctness-preserving and
> bounded. Whether reading a prefix is actually *faster* depends on corpus size
> and dimension, and the frozen 1M/10M frontier has not been run. Measure before
> enabling it in production.

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
| `0` | Selects the narrowest certified prefix, if the candidate budget leaves oversampling room |
| positive | Pins that prefix. An **undeclared** value is rejected and the query falls back to the full dimensions rather than reading an uncertified cut point |
| `-1` | Disables prefix candidate generation entirely |

Every one of these falls back to full-dimension candidate generation when the
collection has no certified policy, when the profile is ineligible or absent,
or when the candidate budget leaves no room to oversample. A collection that
never declares a policy behaves exactly as it did before this feature existed.

## How it works

1. The candidate probe scores `pgcontext.vector_prefix(column, N)` against a
   query prefix. The stored value is untouched.
   When the profile promises `unit_l2`, the **query** prefix is renormalized
   under `cosine` and `inner_product`, where rescaling one side cannot reorder
   results. Under `l2` it is **not**: `|q - s|^2` reweights the dot-product term
   against a per-row `|s|^2`, so scaling only the query would produce an order
   that is neither "truncate both" nor "renormalize both". `l2` prefixes are
   truncated plainly on both sides.
2. The probe admits more candidates than the caller asked for
   (`4 ×` the limit, capped by the remaining candidate budget), because a prefix
   distance only approximates the full-dimension distance.
3. The executor's source recheck rereads the current rows under MVCC and RLS
   and recomputes the exact full-dimension distance, then ranks and truncates.

Nothing downstream trusts the prefix score. That is what makes the ordered
answer identical to full-vector search.

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

The serving strategy appears in query telemetry as `dense_adaptive_prefix`
(versus `dense_exact` for the full-dimension path):

```sql
SELECT strategy, query_count, total_candidates, avg_latency_ms
  FROM pgcontext.query_execution_stats()
 WHERE collection_name = 'docs';
```

## Limits

| Bound | Value |
| --- | --- |
| Declared prefixes per profile | 8 |
| Prefix dimension | `1..=16000`, strictly below the full dimension |
| Candidate oversampling factor | 4× the requested limit, capped by the candidate budget |

## Related

- [SQL API contract](api_reference.md) — `register_embedding_profile` and
  `vector_prefix` signatures.
- [Dense vectors and exact search](vector_search.md) — the full-vector path this
  optimizes and is measured against.
