# Quickstart

The fastest way to try pgContext is to pull the pre-built Docker image — no build step needed. The image is multi-arch (`linux/amd64` and `linux/arm64`) and works on macOS, Linux, and Windows via Docker Desktop.

```sh
docker pull ghcr.io/evokoa/pgcontext:pg17-v0.2.0
docker run -d --rm \
  --name pgcontext \
  -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=pgcontext \
  -p 5432:5432 \
  ghcr.io/evokoa/pgcontext:pg17-v0.2.0
```

Verify the extension is loaded (uses `psql` inside the container, so you don't need a local PostgreSQL client):

```sh
docker exec pgcontext psql -U postgres -d pgcontext \
  -c "SELECT extname, extversion FROM pg_extension WHERE extname = 'pgcontext';"
```

*(If the v0.2.0 registry tag is not yet available, build and run the same demo locally using `scripts/quickstart.sh` instead.)*

To exercise the current checkout immediately, you can use the bundled script:

```sh
scripts/quickstart.sh
```

This builds the local image, starts PostgreSQL 17, creates the extension, and
runs [the packaged demo](../playground/demo.sql) with exact search, metadata
filtering, and a real persisted HNSW ordered scan. Clean up with:

```sh
scripts/quickstart.sh clean
```

Continue with the [collection quickstart](user_guide/quickstart.md) or inspect
the [playground contract](user_guide/playground.md).
