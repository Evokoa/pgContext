# pgContext Playground

The playground is a disposable PostgreSQL 17 database built from the local
source tree. It demonstrates ordinary PostgreSQL source tables, exact search,
persisted HNSW, and metadata filtering without requiring a separate vector
service.

```sh
scripts/quickstart.sh
```

Use `scripts/quickstart.sh psql` for an interactive SQL prompt and
`scripts/quickstart.sh clean` to remove the container and volume. The sample
password is intentionally development-only; do not expose this Compose service
to an untrusted network.
