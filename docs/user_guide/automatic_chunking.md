# Automatic document chunking

pgContext can turn an ordinary PostgreSQL document row into a source-linked,
query-visible chunk generation. The source row remains authoritative. Chunk
rows, contextual prefixes, deterministic fixture embeddings, jobs, and staging
records are derived state and can be rebuilt.

The first versioned parser contract covers plain UTF-8 text, Markdown, and HTML.
It uses the versioned `unicode_words_v1` tokenizer and deterministic token-safe
overlap. PDF, Office, OCR, image/layout parsing, network fetches, and
model-driven splitting are not part of this contract.

This capability is Experimental. Its complete PG17 and PG18 lifecycle suites
pass, but both frozen one-million-row lanes miss the predeclared
1,000-chunks-per-second publication floor: PG17 measures 10.837 chunks per
second and PG18 measures 33.325. Stable promotion therefore remains a measured
no-go; the threshold was not relaxed after either result.

## Setup

The source table must have a primary key named `id`, a non-null text column,
and a non-null positive `bigint` source-version column. First create a
collection and a user-owned projection table, then register an immutable
profile and source:

```sql
SELECT pgcontext.create_collection('documents', 'app.documents');
SELECT pgcontext.create_document_chunk_projection('app.document_chunks');

SELECT pgcontext.register_chunking_profile(
    'default',
    'markdown_v1',
    384,  -- target tokens
    512,  -- hard tokens per chunk
    32,   -- useful non-final minimum
    64,   -- adjacent overlap
    8388608,
    true  -- store a separate heading/context prefix
);

SELECT pgcontext.register_document_source(
    'documents', 'body', 'body', 'source_version',
    'app.document_chunks', 'default'
);
```

The projection belongs to the caller, not the extension. Do not alter its
column contract while it is registered. pgContext locks and verifies its OID
and required column types before reading or publishing. Registration requires
the caller to hold `SELECT`, `INSERT`, `UPDATE`, and `DELETE` on the projection.

Install the optional transactional outbox trigger when inserts, updates, and
deletes should enqueue or invalidate work automatically:

```sql
SELECT pgcontext.install_document_chunk_trigger('documents', 'body');
```

An application may instead enqueue bounded source-key batches explicitly:

```sql
SELECT pgcontext.enqueue_document_chunking(
    'documents', 'body', ARRAY['doc-1', 'doc-2']
);
```

## Shadow, promote, rollback, and drain

Each source binds a profile alias, not a cached profile value. An alias has one
current profile, one optional non-serving shadow, and at most eight retained
predecessors. Pointer changes are constant-size catalog updates; they never
rewrite every source, job, or projection row.

```sql
SELECT pgcontext.prepare_chunking_profile_alias('default', 'next-profile');
SELECT pgcontext.enqueue_document_chunking_profile(
    'documents', 'body', 'next-profile', ARRAY['doc-1', 'doc-2']
);
SELECT pgcontext.promote_chunking_profile_alias('default', 'next-profile');
```

Before promotion, the shadow is never query-visible. After promotion, current
ready chunks win per source key; retained predecessors fill current-version
coverage gaps in most-recent-promotion order. Preparing another shadow does
not expose it or replace those fallbacks. Rollback selects the newest retained
predecessor. Drain only after its remaining coverage and rollback value are no
longer needed:

```sql
SELECT pgcontext.rollback_chunking_profile_alias('default');
SELECT pgcontext.drain_chunking_profile_alias('default', 'old-profile');
```

Preparing a different shadow replaces the old shadow pointer. Existing old
shadow work is fail-closed and becomes superseded lazily when claim or another
fenced lifecycle boundary encounters it.

## Worker lifecycle

Workers claim at most 256 jobs for 1–60,000 milliseconds. A claim returns the
job ID, a monotonically increasing fencing token, and a versioned
`chunk_worker_request_v1` JSON envelope containing only text currently visible
to the invoker under PostgreSQL ACL and RLS.

```sql
SELECT *
FROM pgcontext.claim_document_chunk_jobs(32, 30000, 'chunk-worker-a');
```

Send each request as one newline-delimited frame to
`pgcontext-chunk-worker`. The persistent process accepts
`chunk_worker_request_v1`, emits `chunk_worker_response_v1`, and exits cleanly
after a `shutdown` line. It reads no files and performs no network access.

Stage and publish a complete response with the same fenced token:

```sql
SELECT pgcontext.stage_document_chunks(:job_id, :lease_token, :response_jsonb);
SELECT pgcontext.publish_document_chunk_generation(:job_id, :lease_token);
```

Staging reparses and rechunks the current authoritative row. Every identity,
span, token count, link, hash, and output field must equal that canonical pass.
Publication rereads the row again, rechecks source and projection OIDs,
attnums, types, collation, registration revision, source version/hash, lease,
ACL, and RLS, inserts the complete projection, records one fake-embedding job
per occurrence, and flips the current alias in the same transaction. Partial
or stale work is never returned. A previous ready generation remains visible
only while its source version and SHA-256 still match an invoker-visible
authoritative row. A source edit, delete, SELECT revocation, or RLS change
hides stale chunks immediately, before any replacement is ready.

Use these lifecycle controls for long-running workers:

- `heartbeat_document_chunk_job(job_id, lease_token, lease_millis)` renews a
  current lease without weakening its fencing token.
- `checkpoint_document_chunk_job(job_id, lease_token, status,
  processed_units, total_units)` records monotonic `parsing`, `chunking`, and
  `embedding` progress.
- `fail_document_chunk_job(job_id, lease_token, error_code)` terminates a
  leased attempt with a bounded content-free worker failure code.
- `cancel_document_chunk_job(job_id)` cancels queued work immediately. Active
  work enters `cancel_requested`; the current worker acknowledges it at a
  heartbeat, or bounded expiry cleanup terminalizes it and removes staging.
- `retry_document_chunk_job(job_id)` requeues a bounded failed or cancelled
  attempt; a job has at most three claims.
- `document_chunking_progress(collection, source_name)` returns content-free
  job, progress, staging-byte, and current-document counts.

`fake_process_document_chunk_job` is an internal deterministic certification
helper. Production applications should run the external worker protocol.

## Reading, rollback, and deletion

Only current complete aliases are returned:

```sql
SELECT source_key, occurrence_id, original_text, start_byte, end_byte,
       structure_path, fake_embedding
FROM pgcontext.current_document_chunks(
    'documents', 'body', ARRAY['doc-1']
)
ORDER BY source_key, ordinal;
```

`original_text` and its byte/character span are the citation authority.
`retrieval_text` and the optional `context_prefix` are derived retrieval input;
never substitute them for the citation. Document instructions and markup are
data, not trusted prompts.

An owner can make a previously published generation current again only when
its source key, version, digest, and registration revision still match the
current invoker-visible source row:

```sql
SELECT pgcontext.rollback_document_chunk_generation(
    'documents', 'body', 'doc-1', :generation_id
);
```

Deleting a source row through the installed trigger removes every current
alias for that source key and retires the published generation. Explicit
invalidation is also available through
`invalidate_document_chunks(collection, source_name, source_keys)`.

If a ready or retired user-owned projection is modified after publication and
fails its stored digest, the collection/source owner can rebuild it:

```sql
SELECT pgcontext.rebuild_document_chunk_job(:job_id);
```

The function first rechecks current source ACL/RLS and identity, removes the
generation's current alias, clears its projection and fake-embedding rows, and
requeues the same immutable source version. A worker must claim, stage, and
publish it again before it becomes current. Other job states are not eligible.

## Frozen limits

- source keys per enqueue/read: 256; each key: 1–1,024 bytes
- source document: at most 8 MiB and 2,000,000 tokens
- adjacent chunk overlap: at most 64 tokens and strictly below the target
- chunks per generation: at most 16,384
- parser structure depth: 32
- parser structure-path segment: at most 512 UTF-8 bytes
- retained predecessor profiles per alias: 8
- staged decoded projection: 32 MiB and 1,000,000 JSONB iterator tokens
- encoded worker frame: 40 MiB
- contextual prefix: 96 tokens
- lease: at most 60 seconds; at most three claims
- complete parser/worker operation: at most 120 seconds, with cooperative
  interrupt checks during tokenization and output construction

The 120-second value is an outer parser/worker safety ceiling. A detached
worker request also freezes the initial lease deadline, which is at most 60
seconds and is therefore the tighter deadline for that request. A database
heartbeat protects later database mutations but does not rewrite an already
issued worker envelope; controllers must issue work that can finish inside the
claim lease.

The worker admits a frame before allocating it. PostgreSQL admits raw JSONB
bytes and iterator nodes before constructing a Rust JSON value. Errors and
default diagnostics contain typed reasons and identities, never document or
chunk text.

## Backup and recovery

Profiles and source registrations participate in extension configuration dump.
Source and projection tables follow ordinary PostgreSQL backup/WAL rules.
Queued jobs, leases, staging rows, and current aliases are derived operational
state and are intentionally excluded from logical extension configuration
dump; enqueue or rebuild them after a logical restore. Physical recovery and
transaction rollback preserve atomic alias publication through PostgreSQL WAL.

Raw staged response JSON is never exposed by membership-filtered views. The
visible staging surface contains only content-free counts, byte totals, and
timestamps; source keys are omitted from the public job view.

## Current scope and composition

The frozen 28-column projection includes nullable `page_number` and `region`
fields; the certified plain-text, Markdown, and HTML parsers emit them as null.
PDF/layout adapters and source MIME, title, URI, language, tenant, and arbitrary
metadata bindings remain outside this Experimental parser contract.

Profile aliases support bounded shadow publication, constant-size promotion,
ordered retained fallback, rollback, and explicit drain. Published projection
rows preserve exact citation spans and can be registered with the existing P11
multi-model and P12 detached-rerank APIs. P13 does not automatically provision
those query registrations or choose application-specific embedding/rerank
models; that composition remains explicit so PostgreSQL authority and profile
selection stay visible to the operator.
