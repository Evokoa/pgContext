# PostgreSQL-Native Lexical and Fuzzy Retrieval

pgContext serves keyword retrieval through PostgreSQL itself. You register a
**lexical source** — an ordered set of weighted text or JSON fields, or a stored
`tsvector` column — and pgContext renders every query from that registration.
PostgreSQL owns parsing, dictionaries, matching, ranking, collation, MVCC, ACLs,
and RLS. pgContext owns validated metadata, bounded orchestration, an exact
fallback, provenance, and diagnostics.

Optional trigram retrieval works the same way through a registered **fuzzy
source** backed by `pg_trgm`.

## Registering a lexical source

```sql
SELECT pgcontext.create_collection('articles', 'public.articles');
SELECT pgcontext.backfill_points('articles', 10000);

SELECT pgcontext.register_lexical_source(
    collection          => 'articles',
    source_name         => 'article',
    text_columns        => ARRAY['title', 'body'],
    text_configuration  => 'pg_catalog.english',
    field_weights       => ARRAY['A', 'D'],
    ranker              => 'ts_rank_cd',
    normalization       => 0
);
```

| Argument | Meaning |
| --- | --- |
| `text_columns` | Source-table columns in document order (1..=16). |
| `text_configuration` | Schema-qualified text-search configuration. Defaults to `pg_catalog.english`. |
| `field_weights` | Per-column PostgreSQL weight `A`/`B`/`C`/`D`. Defaults to `D`. |
| `json_paths` | Per-column dotted path into a `json`/`jsonb` column, e.g. `tags.topic`. Up to 16 components. |
| `ranker` | `ts_rank` or `ts_rank_cd`. |
| `normalization` | PostgreSQL rank normalization bitmask, `0..=63`. |
| `rank_weights` | `{D, C, B, A}` `real[]`, each within `0.0..=1.0`. Defaults to `{0.1, 0.2, 0.4, 1.0}`. |

Source names are validated identifiers: `[a-z_][a-z0-9_]{0,62}`.

### Stored, generated, or trigger-maintained documents

When the source table already carries a `tsvector` column, register it directly:

```sql
SELECT pgcontext.register_lexical_document_source(
    'articles', 'stored', 'document', 'pg_catalog.english'
);
```

pgContext never writes that column. Keep it current with a generated column or a
trigger; PostgreSQL's own MVCC then keeps the document consistent with the row.
Stored-vector sources carry no raw text, so `pgcontext.lexical_headline` is not
available for them.

### Per-row `tsquery` bindings

A `tsquery` column can be registered and referenced from the typed query:

```sql
SELECT pgcontext.register_lexical_tsquery('articles', 'article', 'saved', 'saved_query');
```

## Querying

`pgcontext.query_lexical` builds a validated query-plan leaf that composes with
every other Q1 branch through `pgcontext.execute_query`.

```sql
SELECT point_id, source_key, score
  FROM pgcontext.execute_query(
      'articles',
      pgcontext.query_lexical(
          'article',
          jsonb_build_object('form', 'plain', 'text', 'postgres storage'),
          NULL,
          10
      )
  );
```

### Query forms

| `form` | Fields | PostgreSQL constructor |
| --- | --- | --- |
| `plain` | `text` | `plainto_tsquery` |
| `structured` | `text` | `to_tsquery` (caller supplies `tsquery` syntax) |
| `phrase` | `text` | `phraseto_tsquery` |
| `web_search` | `text` | `websearch_to_tsquery` (raw-input safe) |
| `prefix` | `term` | `to_tsquery('term:*')` |
| `distance` | `left`, `right`, `distance` | `tsquery_phrase` |
| `boolean` | `operator` (`and`/`or`/`not`), `clauses` | `&&`, `||`, `!!` |
| `weight_restricted` | `weights`, `query` | `ts_filter` over the registered document |
| `registered_tsquery` | `name` | the registered per-row `tsquery` column |

`and` and `or` require at least two clauses, `not` requires exactly one, and a
Boolean node accepts at most 64 clauses. Query text is bounded at 4096 bytes,
the tree at 256 nodes and 16 levels, and phrase distance at 16384 lexemes.
`prefix` terms accept only alphanumeric characters and `_`, so no `tsquery`
operator can reach the parser from untrusted input.

`weight_restricted` restricts the **document** with `ts_filter`, so it is only
valid at the root of a lexical query.

> **Weight restriction forgoes the index.** A restricted match is *not* a subset
> of the unrestricted match — removing a lexeme can make a negated clause become
> true — so the candidate probe must evaluate the restricted expression. That
> expression does not match an index keyed on the unrestricted document, so a
> weight-restricted query is served by a bounded sequential scan even when an
> index is attached. It stays bounded by the candidate allowance and the query
> timeout, but do not expect index-speed latency from this form on a large
> corpus.

Every form is a `HigherIsBetter` leaf and accepts the same filter JSON as the
other Q1 branches.

## Indexes

pgContext creates and owns lexical indexes so the index expression always
matches the canonical document expression the query paths render:

```sql
SELECT pgcontext.create_lexical_index('articles', 'article');           -- GIN
SELECT pgcontext.create_lexical_index('articles', 'article', 'gist');   -- GiST
SELECT pgcontext.detach_lexical_index('articles', 'article');
```

`pgcontext.attach_lexical_index` binds an index you created yourself. Attachment
requires a valid, live, non-partial GIN or GiST index on the registered source
relation. The full `pg_get_indexdef` text is recorded and rechecked on every
query, so an index that is later redefined fails closed rather than silently
changing semantics.

**Serving contract.** With no attached index, the exact path evaluates the
complete invoker-visible corpus or reports budget exhaustion — it never
truncates and calls the result complete. With an attached index, pgContext
starts from the registered `@@` predicate, probes at most the remaining
candidate allowance plus one, marks the page incomplete when that boundary is
crossed, and then rereads the current source rows under MVCC and RLS to reapply
the predicate and recompute the exact native rank. Index candidates are never
authoritative.

### Candidate budget

`pgcontext.lexical_candidate_budget` (default `1000`, maximum `10000`) bounds
how many candidates one registered lexical or fuzzy source may admit from an
attached index before the page is marked incomplete. A query whose match set
exceeds the allowance fails closed rather than returning a silently truncated
answer:

```sql
SET pgcontext.lexical_candidate_budget = 5000;
```

Raise it for broad queries over large corpora. It only ever raises the
allowance: the effective budget is `max(setting, the query's own limit)`, so
setting it below a query's `limit` has no effect. To cap per-query work, lower
the query's `limit` or the collection's `query_timeout_ms`.

## Fuzzy (trigram) sources

Fuzzy retrieval is optional and requires `pg_trgm`. pgContext resolves the
extension through its own catalog entry, so a relocated `pg_trgm` schema works
and `search_path` is never trusted. `pg_trgm` is **not** a pgContext install
requirement.

```sql
CREATE EXTENSION IF NOT EXISTS pg_trgm;
SELECT pgcontext.register_fuzzy_source('articles', 'body_trgm', 'body');
SELECT pgcontext.create_fuzzy_index('articles', 'body_trgm');           -- gin_trgm_ops

SELECT point_id, source_key, score
  FROM pgcontext.execute_query(
      'articles',
      pgcontext.query_fuzzy('body_trgm', 'postgrs', 'similarity', 0.35, NULL, 10)
  );
```

Modes are `similarity`, `word_similarity`, and `strict_word_similarity`.
Thresholds are finite and within `0.0 < threshold <= 1.0`. An indexed probe sets
the matching `pg_trgm` threshold GUC and restores the previous value through a
scope guard when the probe returns or fails; because the setting is written
transaction-locally, a PostgreSQL error that unwinds past the guard is discarded
by PostgreSQL's own transaction rollback. Either way a query never leaks a
threshold into the surrounding transaction. Each mode restores its own
documented default (`0.3`, `0.6`, `0.5`) when the session had no explicit value.
The final score is always recomputed with the explicit similarity function and
typed threshold, never from the GUC.

## Highlighting

```sql
SELECT point_id, headline
  FROM pgcontext.lexical_headline(
      'articles', 'article', ARRAY[1, 2, 3]::bigint[],
      jsonb_build_object('form', 'plain', 'text', 'postgres')
  );
```

The call admits at most 1000 point IDs, 4096 option bytes, and 8 MiB of source
document bytes before PostgreSQL builds any markup. The 2 MiB output cap is a
hard limit on the returned result, enforced while reading the response —
PostgreSQL offers no way to bound `ts_headline` output before generating it, so
keep `MaxFragments`/`MaxWords` modest for large batches. Exceeding any bound
raises `program_limit_exceeded`.

> **Sanitize the output.** `ts_headline` returns PostgreSQL's own markup — by
> default `<b>` and `</b>` around matches, and the surrounding source text is
> reproduced verbatim. pgContext does not escape it. Escape or sanitize the
> result for your output context (HTML, terminal, JSON-in-attribute, …) before
> rendering it.

## Inspecting registrations

```sql
SELECT * FROM pgcontext.lexical_sources('articles');
SELECT * FROM pgcontext.fuzzy_sources('articles');
SELECT stage, detail, strategy FROM pgcontext.explain('articles', 'article');
```

Registration metadata lives in private catalog tables and is exposed only
through membership-filtered security-barrier views, so a non-member observes
nothing. Registration, index attachment, and drops require collection ownership;
every path additionally requires source-relation `SELECT`.

Membership and source-table privilege are both evaluated against the session
role (`SESSION_USER`), consistently with the rest of pgContext's catalog views.
`SET ROLE` therefore does not narrow which registrations are *listed*, though
the source rows a query can read are still governed by PostgreSQL's own ACLs and
RLS at execution time.

## Dump, restore, and drift

Catalog rows store both stable names and resolved OIDs. After a dump/restore or
a source-table rewrite, re-derive the OIDs:

```sql
SELECT pgcontext.refresh_lexical_catalog('articles');
```

Rows whose stable names no longer resolve to a compatible object are left
untouched and fail closed at query time. Column, type, collation, text-search
configuration, relation, and index drift are each detected before Q1 execution
starts.

## Limits

| Bound | Value |
| --- | --- |
| Registered fields per lexical document | 16 |
| JSON path components per field | 16 |
| Query text bytes | 4096 |
| Lexical query nodes | 256 |
| Lexical query depth | 16 |
| Boolean clauses per node | 64 |
| Phrase distance | 16384 |
| Headline points / option bytes / source bytes / output bytes | 1000 / 4096 / 8 MiB / 2 MiB |

## Related

- [Hybrid retrieval](hybrid_retrieval.md) — fusing lexical with dense branches.
- [Retrieval methods](retrieval_methods.md) — where lexical sits among the
  available methods.
- [SQL API contract](api_reference.md) — contract-guaranteed signatures.
