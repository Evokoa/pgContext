# pgvector HNSW compatibility profile

This directory vendors the dense `vector` and `halfvec` L2, inner-product,
cosine, and L1 portions of pgvector's upstream HNSW regression tests. The
fixtures are derived from pgvector 0.8.3 (upstream commit represented by the
repository's certified reference snapshot) and retain pgvector's PostgreSQL
license in `LICENSE.pgvector`.

The only semantic adaptation is index DDL: source columns and query operators
remain pgvector-owned, while `USING hnsw` and pgvector's HNSW opclasses are
replaced with `USING pgcontext_hnsw` and the companion bridge opclasses. This
matches pgContext's migration contract: applications keep their existing
pgvector columns and SQL but explicitly build a pgContext index.

Run the profile against a PostgreSQL 17 server with `vector`, `pgcontext`, and
`pgcontext_pgvector` installed:

```sh
scripts/check-pgvector-regression-compat.sh
```

This is intentionally a bounded HNSW compatibility profile, not a claim that
pgContext implements pgvector's IVFFlat access method or pgvector-specific
HNSW GUCs.
