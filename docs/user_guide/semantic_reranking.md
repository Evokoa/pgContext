# Semantic reranking

Provider-neutral semantic reranking is **Experimental**. Its PostgreSQL 17 and
18 in-server, backup/restore, and one-million-row certification gates pass
locally. Stable promotion still requires retained hosted build and smoke
evidence for `pgcontext-worker` on Darwin and Linux, each on arm64 and x86_64.

pgContext can release a bounded set of currently authorized source texts to an
external reranker, then treat the returned scores as untrusted ordering input.
The model process never receives database credentials or authority. PostgreSQL
rechecks every final row under the caller's current ACL, RLS, filter, deletion,
source-version, and content-hash state.

This is a detached two-step API. An application prepares an envelope, sends it
to its chosen worker, and finalizes the worker response in PostgreSQL.

Prepared requests are transaction- and WAL-durable on the live cluster for
their bounded lifetime, but they are intentionally absent from logical
extension dumps. Source registrations are restored; applications must prepare
a new request after logical backup and restore.

## Register a source

The source table must already back a collection. It needs a single-column
primary key named `id`, one `text` column, and one positive `bigint` source
version column.

```sql
SELECT pgcontext.register_semantic_rerank_source(
    'articles',
    'body',
    'body_text',
    'source_version'
);

-- Metadata may use only registered fields, and its value must match the row.
SELECT pgcontext.register_filter_column('articles', 'kind', 'kind');
```

Registration records relation, key, text, version, type, collation, and content
hash identity. Each prepared request also freezes a digest of every filter and
metadata binding it used, including relation OID, attnum, column name, type
OID, type modifier, collation OID, and JSON path. Only a collection owner can
register or replace a binding. Missing
objects, incompatible types, lost privileges, re-registration, and catalog
drift fail closed.

## Prepare an authorized envelope

Candidate identities normally come from `query_multi_model` or another bounded
retrieval stage. Each occurrence includes its logical point, fused rank and
score, and rank-fusion contributions.

```sql
SELECT pgcontext.prepare_semantic_rerank(
    'articles',
    'body',
    'postgres storage internals',
    '[
      {
        "occurrence_id": 11,
        "point_id": 42,
        "fused_rank": 1,
        "fused_score": 0.0164,
        "contributions": [
          {
            "profile": "dense_v2",
            "rank": 1,
            "native_score": 0.12,
            "weight": 1.0,
            "contribution": 0.0164
          }
        ],
        "metadata": [{"key": "kind", "value": "article"}]
      }
    ]'::jsonb,
    'my-cross-encoder',
    7,
    5000,
    'require_reranker',
    '{"must":[{"key":"tenant","match":"acme"}]}'::jsonb,
    false
);
```

Preparation validates the complete request before creating private request
state. It then resolves only the declared point IDs, applies the registered
filter with bound parameters, reads the source under current MVCC and RLS, and
returns a versioned envelope containing the authorized text and SHA-256 content
digest. Private candidate rows retain identity, version, digest, fusion
provenance, and allow-listed metadata, but do not duplicate source text or
source keys.

Every metadata key must name a registered filter field. Preparation compares
the supplied value with the authoritative row, and finalization repeats that
comparison so metadata drift cannot survive an otherwise unchanged text hash.

The limits are:

- 512 unique occurrences and point IDs;
- 32 KiB of text per candidate;
- 64 KiB for the query;
- 4 MiB for the complete envelope projection;
- 6 MiB for one encoded newline-delimited worker frame;
- eight metadata pairs per candidate;
- 127 fusion contributions per candidate;
- an expiry from 1 to 60,000 milliseconds.

PostgreSQL admits JSONB before converting it into Rust values. Candidate input
is limited to 4 MiB, 100,000 iterator tokens, and nesting depth 64. A filter is
limited to 256 KiB, 1,024 tokens, and the same depth. A worker response is
limited to 256 KiB, 4,096 tokens, and the same depth. These structural limits
apply even when every individual field is otherwise valid.

## Finalize untrusted output

The response must echo envelope version `3`, request ID, model name, and model
revision. Scores must be finite, unique, and refer only to released occurrence
IDs.

```sql
SELECT pgcontext.finalize_semantic_rerank(
    123,
    '{
      "version": 3,
      "request_id": 123,
      "model": "my-cross-encoder",
      "model_revision": 7,
      "scores": [{"occurrence_id": 11, "score": 0.91}]
    }'::jsonb,
    NULL
);
```

`allow_partial = false` requires exactly one score for every released
candidate. With `allow_partial = true`, omitted or newly invisible rows produce
`partial_reranked`; they never masquerade as a complete result. Ties break by
ascending occurrence ID. Returned rows include point and occurrence identity,
the reranker score, fused score/rank, source version, contribution provenance,
and allow-listed metadata. They never include the released text.

An identical replay is idempotent, but it still rechecks current PostgreSQL
authorization and source state before returning rows. A different replay for
the same request is rejected.

## Failure policy

`require_reranker` fails when the worker is unavailable, times out, crashes,
expires, or returns partial output. `allow_fused_fallback` may return the stored
fused order for the operational reasons `unavailable`, `timeout`, `crash`,
`partial_output`, or `expired`:

```sql
SELECT pgcontext.finalize_semantic_rerank(123, NULL, 'timeout');
```

Fallback is visible as `status = "degraded_reranker"` and includes a bounded
`degraded_reason`. Cancellation, malformed or injected output, permission loss,
RLS/filter changes, source edits, registration drift, and model/request
identity mismatch never use a permissive fallback.

Expired or finalized requests can be removed in bounded batches:

```sql
SELECT pgcontext.cleanup_semantic_rerank_requests(1000);
```

## Worker deployment

`pgcontext-worker` is a separate Rust binary; it is not loaded into a
PostgreSQL backend. The certified `linear_pair_v1` adapter is a private contract
fixture, not a claim of general transformer compatibility. It loads only an
operator-provided artifact whose exact byte length and SHA-256 digest match an
immutable manifest. The manifest also fixes model/tokenizer revision, score
contract, candidate/token/time limits, retry count, circuit-breaker threshold
and cooldown, platforms, SPDX identifier, license URL, and
`operator_provided_only` distribution.

Token ceilings are adapter-manifest specific. The certified `linear_pair_v1`
fixture accepts at most eight query tokens and sixteen document tokens; a
future adapter must declare and certify its own values without weakening the
PostgreSQL envelope and JSONB limits above.

No weights are bundled or downloaded. The worker has no network adapter or
default egress path. It uses a current-thread Tokio runtime, a tracked blocking
scorer, cooperative cancellation, bounded retries, and one persistent circuit
breaker for the lifetime of the process.

Run `pgcontext-worker score --manifest PATH` as a newline-delimited service.
Write one `rerank_envelope_v3` JSON object per line. A successful line produces
one `rerank_response_v3` line with the same request ID. An operational failure
produces a bounded `rerank_failure_v1` line:

```json
{"version":1,"request_id":123,"error":"timeout","failure_reason":"timeout"}
```

Route a success object to `finalize_semantic_rerank(request_id, response,
NULL)`. Route an operational failure to
`finalize_semantic_rerank(request_id, NULL, failure_reason)`. Invalid input is
terminal and content-free. Write the reserved line `shutdown` to request
cooperative cancellation; the process joins any active scorer before exiting.
The input reader stops at the 6 MiB frame ceiling even if a sender never writes
a newline. CRLF input is accepted by removing one trailing carriage return.

Default diagnostics are content-free: do not add query, source, tenant,
metadata values, or provider payloads to worker or PostgreSQL logs. The runtime
and direct-license selection record is in
`design/p12-worker-runtime-spike.md`.
