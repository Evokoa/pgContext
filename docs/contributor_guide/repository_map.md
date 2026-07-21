# Repository Map

| Path | Responsibility |
|---|---|
| `crates/context-core` | Shared vector, collection, and error domain types |
| `crates/context-filter` | Filter AST, validation, and SQL-safe rendering inputs |
| `crates/context-query` | Exact and hybrid query planning/execution kernels |
| `crates/context-index` | HNSW algorithms and index-facing abstractions |
| `crates/context-storage` | Durable segment/page formats and validated loaders |
| `crates/context-pg` | PostgreSQL SQL, SPI, catalog, type, and access-method adapter |
| `sql/` | Checked-in generated extension SQL contract |
| `playground/` | Packaged end-to-end demo |
| `scripts/` | Focused gates, generators, installers, and evidence runners |
| `release/` | Release policy, payload, Homebrew, Docker, and readiness tooling |
| `docs/user_guide` | User-facing behavior and support boundaries |
| `docs/contributor_guide` | Architecture, testing, safety, and release maintenance |

Dependency direction is toward pure Rust kernels. PostgreSQL-specific types,
SPI, SQLSTATE mapping, and FFI stay in `context-pg`; validated storage unsafe
work stays in `context-storage`.
