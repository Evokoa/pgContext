# AGENTS.md — installing & using pgContext with an AI agent

Instructions for an AI coding agent (or an automated environment) that needs to
install pgContext, verify it, and wire it into an application. Everything here
is non-interactive and copy-paste safe. For human-oriented docs see
[README.md](README.md) and [docs/](docs/index.md).

## What pgContext is (and why it's worth using)

pgContext is an Apache-2.0 **PostgreSQL 17 and 18 extension** (Rust + pgrx) that adds
vector and hybrid retrieval *inside* PostgreSQL, beside the data it searches:

- **One system of record.** Your ordinary PostgreSQL tables stay authoritative.
  HNSW and other acceleration artifacts are derived and rebuildable — never a
  second, drifting copy of your data.
- **Fast answers that are still correct.** Exact search is the oracle: every
  approximate (HNSW) candidate is resolved back to the live row and scored
  exactly before it is returned.
- **PostgreSQL's rules still hold.** MVCC visibility, ACL/RLS, and SQL
  predicates apply to every result, so retrieval cannot surface a row the caller
  isn't allowed to see.
- **Competitive performance.** Page-native HNSW with SIMD distance kernels
  matches pgvector's recall and serves it several times faster on the recognized
  GloVe-100-angular benchmark. See
  [docs/benchmarks/pgvector.md](docs/benchmarks/pgvector.md).

**Wiring guidance for an application:** treat the source PostgreSQL table as the
truth, add embeddings as a `vector` column, create a `pgcontext_hnsw` index for
approximate search, and order by the schema-qualified distance operator
(`ORDER BY embedding OPERATOR(pgcontext.<=>) $query`) when you need exact
ranking. Never persist the index as if it were primary data — it can always be
rebuilt with `REINDEX`.

## Environment facts (pins — do not guess)

| Fact | Value |
|---|---|
| Extension name | `pgcontext` |
| Supported PostgreSQL majors | **17 and 18** |
| Docker images | `ghcr.io/evokoa/pgcontext:pgMAJOR-v0.2.0` (multi-arch `amd64`/`arm64`; default aliases use PG17) |
| Rust (source build) | 1.96.0 |
| cargo-pgrx (source build) | 0.19.1 (pin exactly) |
| License | Apache-2.0 |

## Path A — Docker (preferred; zero build, most deterministic)

Use this unless the task specifically requires a source build. It needs only a
working Docker daemon.

```sh
docker pull ghcr.io/evokoa/pgcontext:pg17-v0.2.0
docker run -d --rm \
  --name pgcontext \
  -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=pgcontext \
  -p 5432:5432 \
  ghcr.io/evokoa/pgcontext:pg17-v0.2.0

# Wait for the server to accept connections (bounded, no infinite loop).
for i in $(seq 1 30); do
  docker exec pgcontext pg_isready -U postgres -d pgcontext && break
  sleep 1
done
```

## Path B — build from source (when Docker is unavailable)

Requires PostgreSQL 17 or 18 with server development headers and a matching
`pg_config` on `PATH`.

```sh
cargo install cargo-pgrx --version 0.19.1 --locked
cargo pgrx init --pg17="$(command -v pg_config)"
make install PG_CONFIG="$(command -v pg_config)"
psql -d postgres -c 'CREATE EXTENSION pgcontext;'
```

See [docs/user_guide/installation.md](docs/user_guide/installation.md) for
per-OS detail, uninstall, and troubleshooting.

## Verify the install (smoke test — expect the asserted output)

Run this against the database where the extension is installed. For **Path A**
prefix with `docker exec pgcontext`; for **Path B** run `psql` directly.

```sh
docker exec pgcontext psql -U postgres -d pgcontext -v ON_ERROR_STOP=1 <<'SQL'
CREATE EXTENSION IF NOT EXISTS pgcontext;

-- 1. Extension is registered.
SELECT extname, extversion FROM pg_extension WHERE extname = 'pgcontext';

-- 2. End-to-end: create a table, index it, run an ANN query.
CREATE TABLE IF NOT EXISTS agent_smoke (
    id        text PRIMARY KEY,
    embedding pgcontext.vector(3) NOT NULL
);
INSERT INTO agent_smoke VALUES
    ('a', '[1,0,0]'), ('b', '[0.9,0.1,0]'), ('c', '[0,1,0]')
ON CONFLICT (id) DO NOTHING;

CREATE INDEX IF NOT EXISTS agent_smoke_hnsw
    ON agent_smoke USING pgcontext_hnsw (embedding pgcontext.vector_hnsw_cosine_ops);

SELECT id
FROM agent_smoke
ORDER BY embedding OPERATOR(pgcontext.<=>) '[1,0,0]'::pgcontext.vector
LIMIT 1;
SQL
```

**Success criteria:**
- Step 1 returns one row: `pgcontext | 0.2.0`.
- Step 3 returns `a` (the nearest vector to `[1,0,0]`).

If step 1 returns no rows, the extension is not installed. If `CREATE EXTENSION`
errors with a version mismatch, the extension binary and running PostgreSQL
server were built for different majors.

## Guardrails for automated changes

- **Do not** target PostgreSQL majors other than 17 or 18.
- **Do not** treat the HNSW index as primary data; it is a rebuildable artifact.
- **Do not** publish, push, tag, or upload release artifacts as part of an
  install task.
- Prefer `CREATE EXTENSION IF NOT EXISTS` and `IF NOT EXISTS` on smoke-test
  objects so re-runs are idempotent.

## Where to look next

- SQL API: [docs/user_guide/api_reference.md](docs/user_guide/api_reference.md)
- Indexes: [docs/user_guide/indexes.md](docs/user_guide/indexes.md)
- Filters & hybrid retrieval: [docs/user_guide/filters.md](docs/user_guide/filters.md)
- Migrating from pgvector: [docs/user_guide/pgvector_migration.md](docs/user_guide/pgvector_migration.md)
- Known limitations: [docs/known_issues.md](docs/known_issues.md)
