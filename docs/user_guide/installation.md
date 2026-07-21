# Installation

pgContext V1 supports PostgreSQL 17 exclusively. Extension binaries, development headers, `pg_config`, and the running server must all use the same major version. Installing a PG17 build into another major is unsupported.

## Installation Methods

| Method | Host | Builds locally | Availability |
|---|---|---:|---|
| GHCR image | Docker on Linux, macOS, or Windows | No | With the v0.1.0 release |
| Manual source | Linux/macOS, or Windows through WSL2 | Yes | From the checkout or source archive |
| Local Compose playground | Docker on Linux, macOS, or Windows | Yes | From the checkout |
| PGXN | Linux/macOS source hosts | Yes | Future update |
| Homebrew | macOS with Homebrew PostgreSQL 17 | Yes | Future update |

Shell scripts target Bash. On Windows, use Docker Desktop with WSL2 and run
them inside WSL2; native PowerShell and Command Prompt are not supported build
shells. The image itself supports `linux/amd64` and `linux/arm64`.

## Prebuilt Docker image

The prebuilt image is published with the v0.1.0 release:

```sh
docker pull ghcr.io/evokoa/pgcontext:pg17-v0.1.0
docker run -d --rm \
  --name pgcontext \
  -e POSTGRES_PASSWORD=postgres \
  -e POSTGRES_DB=pgcontext \
  -p 5432:5432 \
  ghcr.io/evokoa/pgcontext:pg17-v0.1.0
```

Verify PostgreSQL, the extension, dense HNSW, and metadata filtering:

```sh
docker exec pgcontext psql -U postgres -d pgcontext \
  -c 'SHOW server_version_num' \
  -c "SELECT extversion FROM pg_extension WHERE extname = 'pgcontext';"
docker exec -i pgcontext psql -U postgres -d pgcontext -v ON_ERROR_STOP=1 \
  < playground/demo.sql
```

Use the immutable manifest digest from the published release for controlled
deployments. `pg17-v0.1.0`, `pg17-0.1.0`, `v0.1.0`, and `0.1.0` are immutable
version aliases. Only `pg17` and `latest` are rolling convenience aliases.

Cleanup:

```sh
docker stop pgcontext
```

## PGXN source installation

> **Coming soon.** PGXN publication is a future update. Until it is available,
> use the Docker image, the manual source build below, or the local Compose
> playground.

Prerequisites:

- Rust 1.96.0;
- `cargo-pgrx` 0.19.1;
- PostgreSQL 17 server development headers and `pg_config`;
- a C linker and ordinary build tools;
- `pgxnclient` for the `pgxn install` command.

```sh
cargo install cargo-pgrx --version 0.19.1 --locked
cargo pgrx init --pg17="$(command -v pg_config)"
pgxn install pgContext
psql -d postgres -c 'CREATE EXTENSION pgcontext;'
```

`pgContext` is the distribution name; `pgcontext` is the extension name.

## Homebrew (coming soon)

> **Coming soon.** A Homebrew formula — `brew install pgcontext` from the Evokoa
> tap, building against `postgresql@17` — will be added in a future update.

## Manual source build

Select the exact PG17 installation when several PostgreSQL versions coexist:

```sh
export PG_CONFIG=/usr/lib/postgresql/17/bin/pg_config
cargo install cargo-pgrx --version 0.19.1 --locked
cargo pgrx init --pg17="${PG_CONFIG}"
make install PG_CONFIG="${PG_CONFIG}"
psql -d postgres -c 'CREATE EXTENSION pgcontext;'
```

The final install may need filesystem privileges for PostgreSQL's extension
directories. Preserve `PG_CONFIG` if privilege escalation is required; do not
install into a different PostgreSQL major.

## Local Compose playground

```sh
git clone https://github.com/evokoa/pgcontext.git
cd pgcontext
scripts/quickstart.sh         # build, start, and run the demo
scripts/quickstart.sh setup   # start without demo data
scripts/quickstart.sh psql    # interactive prompt
scripts/quickstart.sh clean   # remove container and volume
```

The Compose password is development-only. Do not expose this configuration to
an untrusted network.

## Uninstall

Remove the extension from each database before deleting installed files, and
drop dependent objects only after review:

```sql
SELECT pgcontext.drop_collection('collection_name');  -- once per collection
-- Drop dependent application tables or vector columns only after review.
DROP EXTENSION pgcontext;
```

Then remove the installed files with the method you installed by: `make
uninstall PG_CONFIG=...` for a source build, or removing the container and
volume for Docker (`scripts/quickstart.sh clean`).

## Common failures

- `postgres.h: No such file or directory`: install PostgreSQL 17 server
  development headers and confirm `pg_config --includedir-server`.
- `pg_config must report PostgreSQL 17`: select the PG17 binary explicitly.
- `cargo pgrx` cannot find PG17: rerun `cargo pgrx init --pg17=...`.
- `permission denied` during install: use the filesystem privilege model for
  that PostgreSQL installation while preserving `PG_CONFIG`.
- image tag not found or PGXN distribution missing: these artifacts are
  published with the v0.1.0 release; until then, use local Compose or a manual
  source build.
- extension cannot be dropped: identify dependent vector columns/tables and
  remove them deliberately; do not use `CASCADE` without review.

More diagnosis is in [Troubleshooting](troubleshooting.md). Backup and rebuild
procedures are in [Operations](operations.md).
