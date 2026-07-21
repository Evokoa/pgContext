---
name: Bug report
about: Something in pgContext doesn't work as documented
title: ""
labels: bug
assignees: ""
---

**What happened**

A clear description of the incorrect behavior.

**What you expected**

What should have happened instead.

**Reproduction**

Minimal SQL or shell steps to reproduce, starting from `CREATE EXTENSION pgcontext;`
if relevant. Include the exact error message or output, not a paraphrase.

```sql

```

**Environment**

- pgContext version (`SELECT extversion FROM pg_extension WHERE extname = 'pgcontext';`):
- PostgreSQL version (`SELECT version();`):
- Install method (Docker / Homebrew / PGXN / source build):
- OS/architecture:

**Additional context**

Logs, `EXPLAIN (ANALYZE, BUFFERS)` output, or anything else that narrows it down.

> Found a security vulnerability instead? Do not open an issue — see [SECURITY.md](../../SECURITY.md).
